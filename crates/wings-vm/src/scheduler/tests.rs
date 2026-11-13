use super::*;
use crate::RequestOutcome;
use crate::module::ModuleData;
use wings_abi::{ScalarContext, ScalarOutput, ValueType};
use wings_artifact::{Handler, Manifest};

unsafe extern "C" fn entry(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe { *output = ScalarOutput { state, reply: 0 } };
    wings_abi::STATUS_OK
}

fn module() -> ScalarModule {
    ScalarModule {
        inner: Arc::new(ModuleData {
            manifest: Manifest {
                format: wings_artifact::FORMAT_VERSION,
                abi: wings_abi::SCALAR_ABI_VERSION,
                target: wings_abi::host_target().unwrap().into(),
                process: "SchedulerInvariant".into(),
                state: ValueType::Int,
                handlers: vec![
                    Handler {
                        name: "Done".into(),
                        parameters: vec![
                            ("id".into(), ValueType::RequestId),
                            ("status".into(), ValueType::Int),
                            ("value".into(), ValueType::Int),
                        ],
                        reply: None,
                    },
                    Handler {
                        name: "Store".into(),
                        parameters: vec![
                            ("record".into(), ValueType::Ref),
                            ("list".into(), ValueType::ListRef),
                        ],
                        reply: None,
                    },
                    Handler {
                        name: "Drop".into(),
                        parameters: vec![],
                        reply: None,
                    },
                ],
            },
            initializer: entry,
            entries: vec![entry as wings_abi::ScalarEntry; 3].into_boxed_slice(),
            _library: None,
            lease: None,
        }),
    }
}

#[test]
fn completion_waits_for_mailbox_space_and_late_reply_cannot_reuse_exhausted_generation() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant_accounted(limits).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 1,
            capacity: 1,
            requests: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, limits, 1).unwrap();
    let mut state = handle.shared.lock();
    let actor = state.actor(handle.shared.identity, pid).unwrap();
    let charge = actor
        .account
        .reserve(Resources {
            requests: 1,
            ..Resources::default()
        })
        .unwrap();
    let completion = actor
        .account
        .reserve(Resources {
            messages: 1,
            bytes: std::mem::size_of::<Envelope>() as u64,
            ..Resources::default()
        })
        .unwrap();
    let message = envelope(
        actor,
        &module.handler("Drop").unwrap(),
        &[],
        32,
        0,
        ListPayload::default(),
        false,
        Reply::None,
    )
    .unwrap();
    state
        .actor_mut(handle.shared.identity, pid)
        .unwrap()
        .mailbox
        .push(message)
        .unwrap();
    state.requests[0].generation = u64::MAX;
    let key = RequestKey {
        slot: 0,
        generation: u64::MAX,
    };
    state.requests[0].pending = Some(Pending {
        owner: pid,
        target: pid,
        capability: 17,
        handler: module.handler("Done").unwrap(),
        deadline: Instant::now() + Duration::from_secs(10),
        max_steps: 32,
        outcome: None,
        completion_charge: completion,
        _charge: charge,
    });
    state.finish(key, RequestOutcome::Cancelled);
    state.finish(key, RequestOutcome::Reply(99));
    state.maintain(handle.shared.identity);
    assert_eq!(
        state.requests[0].pending.as_ref().unwrap().outcome,
        Some(RequestOutcome::Cancelled)
    );
    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
    drop(actor.mailbox.pop());
    actor.starting = true;
    state.maintain(handle.shared.identity);
    assert!(state.requests[0].pending.is_some());
    state
        .actor_mut(handle.shared.identity, pid)
        .unwrap()
        .starting = false;
    state.maintain(handle.shared.identity);
    assert!(state.requests[0].pending.is_none());
    assert_eq!(state.requests[0].generation, 0);
    state.finish(key, RequestOutcome::Reply(99));
    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
    let completion = actor.mailbox.pop();
    assert_eq!(&completion.arguments[..completion.count], &[17, 2, 0]);
    assert_eq!(actor.account.used().requests, 0);
    drop(completion);
    assert_eq!(actor.account.used().messages, 0);
    state.slots[pid.slot as usize].generation = u32::MAX;
    let last = state.pid(handle.shared.identity, pid.slot as usize);
    let retired = state.retire(handle.shared.identity, last).unwrap();
    assert_eq!(state.slots[pid.slot as usize].generation, 0);
    drop((state, retired));
    assert!(handle.spawn(&module, limits, 1).is_err());
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn cold_stack_process_denial_preserves_fifo_until_quota_is_released() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 16,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant_accounted(limits).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
            requests: 4,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = crate::tests::module();
    let blocked = handle
        .spawn(
            &module,
            Resources {
                bytes: 8 << 20,
                ..limits
            },
            2,
        )
        .unwrap();
    let healthy = handle.spawn(&module, limits, 1).unwrap();
    let account = handle
        .shared
        .lock()
        .actor(handle.shared.identity, blocked)
        .unwrap()
        .account
        .clone();
    let guard = account
        .reserve(Resources {
            bytes: account.limit().bytes
                - account.used().bytes
                - (2 * std::mem::size_of::<Envelope>()) as u64
                - INVOCATION_BYTES,
            ..Resources::default()
        })
        .unwrap();
    let bump = module.handler("Bump").unwrap();
    let mut first = handle
        .request(blocked, &bump, &[7], 32, Duration::from_secs(5))
        .unwrap();
    let mut second = handle
        .request(blocked, &bump, &[9], 32, Duration::from_secs(5))
        .unwrap();
    for expected in 1..=4 {
        let mut request = handle
            .request(healthy, &bump, &[1], 32, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            request.wait(Duration::from_secs(5)).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
        let snapshot = handle.snapshot(blocked).unwrap();
        assert_eq!(snapshot.committed_state, Some(0));
        assert_eq!(snapshot.resources.messages, 2);
        assert_eq!(snapshot.migrations, 0);
        assert_eq!(first.try_take().unwrap(), None);
        assert_eq!(second.try_take().unwrap(), None);
        assert_eq!(
            handle
                .request(blocked, &bump, &[1], 32, Duration::from_secs(5))
                .err()
                .unwrap(),
            "mailbox_full"
        );
    }
    drop(guard);
    assert_eq!(
        first.wait(Duration::from_secs(5)).unwrap(),
        Some(RequestOutcome::Reply(7))
    );
    assert_eq!(
        second.wait(Duration::from_secs(5)).unwrap(),
        Some(RequestOutcome::Reply(16))
    );
    assert_eq!(handle.snapshot(blocked).unwrap().committed_state, Some(16));
    drop((first, second, account, handle, scheduler, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_message_validation_rejects_forged_reference_masks_and_completion_phase() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 2,
        messages: 4,
        requests: 2,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant_accounted(limits).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 1,
            capacity: 1,
            requests: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, limits, 1).unwrap();
    let state = handle.shared.lock();
    let actor = state.actor(handle.shared.identity, pid).unwrap();
    let store = module.handler("Store").unwrap();
    let arguments = [7, 0];
    let lists = ListPayload {
        mask: 2,
        ..ListPayload::default()
    };
    assert!(validate(actor, &store, &arguments, false, 1, &lists).is_ok());
    for mask in [0, 2, 3, u64::MAX] {
        assert!(validate(actor, &store, &arguments, false, mask, &lists).is_err());
    }
    assert!(validate(actor, &store, &[7, -1], false, 1, &lists).is_err());
    assert!(
        validate(
            actor,
            &module.handler("Done").unwrap(),
            &[1, 0, 2],
            false,
            0,
            &ListPayload::default()
        )
        .is_err()
    );
    assert!(
        validate(
            actor,
            &module.handler("Done").unwrap(),
            &[1, 0, 2],
            true,
            0,
            &ListPayload::default()
        )
        .is_ok()
    );
    drop(state);
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}
