use super::*;
use crate::RequestOutcome;
use crate::execution::INVOCATION_OVERHEAD as INVOCATION_BYTES;
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

pub(super) fn module() -> ScalarModule {
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
                    Handler {
                        name: "Read".into(),
                        parameters: vec![],
                        reply: Some(ValueType::Int),
                    },
                ],
            },
            initializer: entry,
            entries: vec![entry as wings_abi::ScalarEntry; 4].into_boxed_slice(),
            _library: None,
            lease: None,
        }),
    }
}

#[test]
fn ready_publication_after_an_empty_claim_prevents_parking_without_removing_retry_backoff() {
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
            workers: 2,
            capacity: 2,
            requests: 2,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, limits, 1).unwrap();
    let handler = module.handler("Drop").unwrap();
    let mut state = handle.shared.lock();
    assert!(state.pop_ready(0).is_none());
    assert!(state.should_park(0, false, false));
    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
    let message = envelope(
        actor,
        &handler,
        &[],
        32,
        0,
        ListPayload::default(),
        BytePayload::default(),
        false,
        Reply::None,
    )
    .unwrap();
    actor.mailbox.push(message).unwrap();
    state.enqueue(pid);
    handle.shared.changed.notify_all();
    assert!(!state.should_park(0, false, false));
    assert!(state.should_park(1, false, false));
    assert!(state.should_park(0, false, true));
    assert!(!state.should_park(0, true, true));
    assert!(!state.should_park(1, true, false));
    assert_eq!(state.pop_ready(0), Some(pid.slot as usize));
    assert!(state.pop_ready(0).is_none());
    assert!(state.should_park(0, false, false));
    state.enqueue(pid);
    drop(state);
    drop((scheduler, handle, handler, module, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn large_mailboxes_preserve_fifo_wraparound_and_reserved_starting_position() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 512,
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
    let handler = module.handler("Drop").unwrap();
    for capacity in [100, 256] {
        let pid = handle.spawn(&module, limits, capacity).unwrap();
        let mut state = handle.shared.lock();
        let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
        let baseline = actor.account.used();
        for sequence in 1..=capacity {
            let message = envelope(
                actor,
                &handler,
                &[],
                sequence as u64,
                0,
                ListPayload::default(),
                BytePayload::default(),
                false,
                Reply::None,
            )
            .unwrap();
            actor.mailbox.push(message).unwrap();
        }
        let first = actor.mailbox.pop();
        actor.starting = true;
        let baseline_full = actor.account.used();
        assert!(
            envelope(
                actor,
                &handler,
                &[],
                999,
                0,
                ListPayload::default(),
                BytePayload::default(),
                false,
                Reply::None
            )
            .is_err()
        );
        assert_eq!(actor.account.used(), baseline_full);
        actor.mailbox.restore(first);
        actor.starting = false;
        for sequence in 1..=capacity / 2 {
            assert_eq!(actor.mailbox.pop().max_steps, sequence as u64);
        }
        for sequence in capacity + 1..=capacity + capacity / 2 {
            let message = envelope(
                actor,
                &handler,
                &[],
                sequence as u64,
                0,
                ListPayload::default(),
                BytePayload::default(),
                false,
                Reply::None,
            )
            .unwrap();
            actor.mailbox.push(message).unwrap();
        }
        for sequence in capacity / 2 + 1..=capacity + capacity / 2 {
            assert_eq!(actor.mailbox.pop().max_steps, sequence as u64);
        }
        assert_eq!(actor.mailbox.count, 0);
        assert_eq!(actor.account.used(), baseline);
        drop(state);
        handle.stop(pid).unwrap();
    }
    drop((scheduler, handle, module, handler, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn running_workers_keep_delivered_timeout_ticket_while_target_cannot_start() {
    for workers in [1, 2] {
        for cancel in [false, true] {
            for stop_target in [false, true] {
                let limits = Resources {
                    bytes: 32 << 20,
                    processes: 4,
                    messages: 8,
                    requests: 4,
                };
                let vm = ResourceAccount::vm(limits);
                let tenant = vm.tenant_accounted(limits).unwrap();
                let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
                let scheduler = Scheduler::new(
                    &tenant,
                    SchedulerConfig {
                        workers,
                        capacity: 2,
                        requests: 1,
                        ..SchedulerConfig::default()
                    },
                )
                .unwrap();
                let handle = scheduler.handle();
                let module = module();
                let process_limit = Resources {
                    bytes: 8 << 20,
                    ..limits
                };
                let owner = handle.spawn(&module, process_limit, 1).unwrap();
                let target = handle.spawn(&module, process_limit, 1).unwrap();
                let (account, target_capability) = {
                    let state = handle.shared.lock();
                    let actor = state.actor(handle.shared.identity, target).unwrap();
                    (actor.account.clone(), actor.capability)
                };
                let guard = account
                    .reserve(Resources {
                        bytes: account.limit().bytes
                            - account.used().bytes
                            - std::mem::size_of::<Envelope>() as u64,
                        ..Resources::default()
                    })
                    .unwrap();
                let effect = || crate::execution::ProcessEffect {
                    operation: wings_abi::EFFECT_REQUEST,
                    target: target_capability,
                    handler: 3,
                    arguments: [0; 64],
                    count: 0,
                    callback: 0,
                    timeout_ms: if cancel { 60000 } else { 0 },
                    record_mask: 0,
                    lists: ListPayload::default(),
                    bytes: BytePayload::default(),
                };
                let (status, capability) = handle.shared.effect(owner, effect(), 32, &pool);
                assert_eq!(status, wings_abi::STATUS_OK);
                if cancel {
                    handle.shared.lock().cancel(owner, capability).unwrap();
                    handle.shared.changed.notify_all();
                }
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let mut state = handle.shared.lock();
                    if state.requests[0]
                        .pending
                        .as_ref()
                        .unwrap()
                        .completion
                        .is_none()
                    {
                        assert!(state.requests[0].pending.as_ref().unwrap().publisher);
                        assert!(state.cancel(owner, capability).is_err());
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "early completion was not delivered"
                    );
                    drop(state);
                    std::thread::yield_now();
                }
                assert_eq!(vm.used().requests, 1);
                assert_eq!(
                    handle.shared.effect(owner, effect(), 32, &pool).0,
                    wings_abi::STATUS_REQUEST_REJECTED
                );
                handle.stop(owner).unwrap();
                assert_eq!(vm.used().requests, 1);
                if stop_target {
                    handle.stop(target).unwrap();
                }
                drop(guard);
                let deadline = Instant::now() + Duration::from_secs(3);
                while vm.used().requests != 0 {
                    assert!(
                        Instant::now() < deadline,
                        "target publisher was not retired"
                    );
                    std::thread::yield_now();
                }
                drop((scheduler, handle, pool, account, tenant));
                assert_eq!(vm.used(), Resources::default());
            }
        }
    }
}

#[test]
fn requester_exit_and_completion_delivery_retain_target_ticket_until_publication_ends() {
    for workers in [1, 2] {
        for deliver in [false, true] {
            for self_request in [false, true] {
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
                        workers,
                        capacity: 2,
                        requests: 1,
                        ..SchedulerConfig::default()
                    },
                )
                .unwrap();
                let handle = scheduler.handle();
                let module = module();
                let owner = handle.spawn(&module, limits, 1).unwrap();
                let target = if self_request {
                    owner
                } else {
                    handle.spawn(&module, limits, 1).unwrap()
                };
                let mut state = handle.shared.lock();
                let account = state
                    .actor(handle.shared.identity, owner)
                    .unwrap()
                    .account
                    .clone();
                let request_charge = account
                    .reserve(Resources {
                        requests: 1,
                        ..Resources::default()
                    })
                    .unwrap();
                let completion_charge = account
                    .reserve(Resources {
                        messages: 1,
                        bytes: std::mem::size_of::<Envelope>() as u64,
                        ..Resources::default()
                    })
                    .unwrap();
                let key = RequestKey {
                    slot: 0,
                    generation: 1,
                };
                state.requests[0].pending = Some(Pending {
                    owner,
                    target,
                    capability: 19,
                    deadline: Instant::now(),
                    outcome: None,
                    publisher: true,
                    completion: Some(Completion {
                        handler: module.handler("Done").unwrap(),
                        max_steps: 32,
                        charge: completion_charge,
                    }),
                    _charge: request_charge,
                });
                if deliver {
                    state.maintain(handle.shared.identity);
                    assert_eq!(account.used().requests, 1);
                    assert!(
                        state.requests[0]
                            .pending
                            .as_ref()
                            .unwrap()
                            .completion
                            .is_none()
                    );
                    assert!(state.cancel(owner, 19).is_err());
                    let actor = state.actor_mut(handle.shared.identity, owner).unwrap();
                    let message = actor.mailbox.pop();
                    assert_eq!(&message.arguments[..3], &[19, 1, 0]);
                    drop(message);
                }
                let retired = state.retire(handle.shared.identity, owner).unwrap();
                drop(retired);
                assert_eq!(account.used().messages, 0);
                assert!(state.actor(handle.shared.identity, owner).is_err());
                assert_eq!(account.used().requests, u64::from(!self_request));
                if !self_request {
                    assert!(
                        state.requests[0]
                            .pending
                            .as_ref()
                            .unwrap()
                            .completion
                            .is_none()
                    );
                    assert_eq!(state.requests[0].generation, 1);
                    let retired = state.retire(handle.shared.identity, target).unwrap();
                    drop(retired);
                }
                assert!(state.requests[0].pending.is_none());
                assert_eq!(state.requests[0].generation, 2);
                state.finish(key, RequestOutcome::Reply(99));
                assert_eq!(account.used().requests, 0);
                drop((state, account));
                drop((scheduler, handle, tenant));
                assert_eq!(vm.used(), Resources::default());
            }
        }
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
        BytePayload::default(),
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
        deadline: Instant::now() + Duration::from_secs(10),
        outcome: None,
        publisher: true,
        completion: Some(Completion {
            handler: module.handler("Done").unwrap(),
            max_steps: 32,
            charge: completion,
        }),
        _charge: charge,
    });
    state.requests[0].pending.as_mut().unwrap().outcome = Some(RequestOutcome::Cancelled);
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
    assert!(
        state.requests[0]
            .pending
            .as_ref()
            .unwrap()
            .completion
            .is_none()
    );
    assert_eq!(state.requests[0].generation, u64::MAX);
    assert_eq!(
        state
            .actor(handle.shared.identity, pid)
            .unwrap()
            .account
            .used()
            .requests,
        1
    );
    assert!(state.cancel(pid, 17).is_err());
    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
    let completion = actor.mailbox.pop();
    assert_eq!(&completion.arguments[..completion.count], &[17, 2, 0]);
    drop(completion);
    assert_eq!(actor.account.used().messages, 0);
    state.finish(key, RequestOutcome::Reply(99));
    assert!(state.requests[0].pending.is_none());
    assert_eq!(state.requests[0].generation, 0);
    state.finish(key, RequestOutcome::Reply(100));
    assert_eq!(
        state
            .actor(handle.shared.identity, pid)
            .unwrap()
            .account
            .used()
            .requests,
        0
    );
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
fn ready_queues_preserve_fifo_uniqueness_removal_and_generation_reuse() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant_accounted(limits).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 6,
            requests: 4,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = module();
    let pids: Vec<_> = (0..6)
        .map(|_| handle.spawn(&module, limits, 1).unwrap())
        .collect();
    let mut state = handle.shared.lock();
    for pid in &pids {
        let message = envelope(
            state.actor(handle.shared.identity, *pid).unwrap(),
            &module.handler("Drop").unwrap(),
            &[],
            32,
            0,
            ListPayload::default(),
            BytePayload::default(),
            false,
            Reply::None,
        )
        .unwrap();
        state
            .actor_mut(handle.shared.identity, *pid)
            .unwrap()
            .mailbox
            .push(message)
            .unwrap();
        state.enqueue(*pid);
        state.enqueue(*pid);
    }
    state.remove_ready(2);
    assert_eq!(state.pop_ready(0), Some(0));
    assert_eq!(state.pop_ready(0), Some(4));
    assert_eq!(state.pop_ready(0), None);
    state.remove_ready(5);
    state.remove_ready(1);
    assert_eq!(state.pop_ready(1), Some(3));
    assert_eq!(state.pop_ready(1), None);
    state.enqueue(pids[2]);
    state.enqueue(pids[4]);
    let retired = state.retire(handle.shared.identity, pids[2]).unwrap();
    assert_eq!(state.pop_ready(0), Some(4));
    assert_eq!(state.pop_ready(0), None);
    assert_eq!(state.reserve_slot().unwrap(), 2);
    let replacement = state.pid(handle.shared.identity, 2);
    assert_ne!(replacement, pids[2]);
    state.slots[2].actor = Some(retired);
    state.slots[2].creating = false;
    state.enqueue(pids[2]);
    assert_eq!(state.pop_ready(0), None);
    state.enqueue(replacement);
    assert_eq!(state.pop_ready(0), Some(2));
    assert_eq!(state.pop_ready(0), None);
    let actor = state
        .actor_mut(handle.shared.identity, replacement)
        .unwrap();
    actor.owner = Some(0);
    state.enqueue(replacement);
    assert_eq!(state.pop_ready(0), None);
    let actor = state
        .actor_mut(handle.shared.identity, replacement)
        .unwrap();
    actor.owner = None;
    actor.last_worker = Some(0);
    state.enqueue(replacement);
    assert_eq!(state.pop_ready(1), Some(2));
    state
        .actor_mut(handle.shared.identity, replacement)
        .unwrap()
        .stopping = true;
    state.enqueue(replacement);
    assert_eq!(state.pop_ready(1), None);
    drop(state);
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
    assert!(
        validate(
            actor,
            &store,
            &arguments,
            false,
            1,
            &lists,
            &BytePayload::default()
        )
        .is_ok()
    );
    for mask in [0, 2, 3, u64::MAX] {
        assert!(
            validate(
                actor,
                &store,
                &arguments,
                false,
                mask,
                &lists,
                &BytePayload::default()
            )
            .is_err()
        );
    }
    assert!(
        validate(
            actor,
            &store,
            &[7, -1],
            false,
            1,
            &lists,
            &BytePayload::default()
        )
        .is_err()
    );
    assert!(
        validate(
            actor,
            &module.handler("Done").unwrap(),
            &[1, 0, 2],
            false,
            0,
            &ListPayload::default(),
            &BytePayload::default()
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
            &ListPayload::default(),
            &BytePayload::default()
        )
        .is_ok()
    );
    drop(state);
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_byte_envelopes_validate_mixed_masks_transfer_and_rollback_before_publish() {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant_accounted(limits).unwrap();
    let source = tenant.process_accounted(limits).unwrap();
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
    let mut module = module();
    Arc::get_mut(&mut module.inner).unwrap().manifest.handlers[1].parameters = vec![
        ("first".into(), ValueType::BytesRef),
        ("flag".into(), ValueType::Bool),
        ("record".into(), ValueType::Ref),
        ("list".into(), ValueType::ListRef),
        ("last".into(), ValueType::BytesRef),
    ];
    let store = module.handler("Store").unwrap();
    let mut quota = limits;
    quota.bytes = 1 << 20;
    let pid = handle.spawn(&module, quota, 1).unwrap();
    let arguments = [
        ListPayload::encode(0, 4),
        1,
        7,
        ListPayload::encode(0, 2),
        ListPayload::encode(4, 4),
    ];
    assert!(handle.send(pid, &store, &arguments, 4096).is_err());
    let lists = ListPayload {
        mask: 8,
        count: 2,
        ..ListPayload::default()
    };
    let mut state = handle.shared.lock();
    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
    let mut bytes = BytePayload::new(&source, 17, 8).unwrap();
    bytes.data.extend_from_slice(b"abcdefgh");
    assert!(validate(actor, &store, &arguments, false, 4, &lists, &bytes).is_ok());
    for mask in [0, 1, 5, 17] {
        assert!(validate(actor, &store, &arguments, false, mask, &lists, &bytes).is_err());
    }
    let filler = actor
        .account
        .reserve(Resources {
            bytes: quota.bytes - actor.account.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    let baseline = vm.used();
    assert!(
        envelope(
            actor,
            &store,
            &arguments,
            4096,
            4,
            lists,
            bytes,
            false,
            Reply::None
        )
        .is_err()
    );
    assert_eq!(vm.used().bytes, baseline.bytes - 8);
    assert_eq!(actor.mailbox.count, 0);
    assert_eq!(source.used(), Resources::default());
    drop(filler);
    let actor_baseline = actor.account.used();
    let mut bytes = BytePayload::new(&source, 17, 8).unwrap();
    bytes.data.extend_from_slice(b"abcdefgh");
    let message = envelope(
        actor,
        &store,
        &arguments,
        4096,
        4,
        lists,
        bytes,
        false,
        Reply::None,
    )
    .unwrap();
    actor.mailbox.push(message).unwrap();
    assert_eq!(source.used(), Resources::default());
    assert_eq!(
        actor.account.used().bytes,
        actor_baseline.bytes + 8 + std::mem::size_of::<Envelope>() as u64
    );
    let baseline = vm.used();
    let mut bytes = BytePayload::new(&source, 17, 8).unwrap();
    bytes.data.extend_from_slice(b"ijklmnop");
    assert!(
        envelope(
            actor,
            &store,
            &arguments,
            4096,
            4,
            lists,
            bytes,
            false,
            Reply::None
        )
        .is_err()
    );
    assert_eq!(vm.used(), baseline);
    assert_eq!(
        actor.mailbox.slots[0].as_ref().unwrap().bytes.data,
        b"abcdefgh"
    );
    drop(state);
    handle.stop(pid).unwrap();
    assert_eq!(tenant.used().messages, 0);
    drop((source, handle, scheduler, tenant));
    assert_eq!(vm.used(), Resources::default());
}
