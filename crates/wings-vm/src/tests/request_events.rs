use super::*;
use wings_abi::{EFFECT_CANCEL_REQUEST, EFFECT_REQUEST, EFFECT_SELF_PID, EffectRequest};

unsafe extern "C" fn identity(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let request = EffectRequest {
        operation: EFFECT_SELF_PID,
        handler: 0,
        target: 0,
        arguments: std::ptr::null(),
        count: 0,
        callback: 0,
        timeout_ms: 0,
        record_mask: 0,
        list_mask: 0,
        bytes_mask: 0,
        string_mask: 0,
    };
    unsafe {
        (*output).state = state;
        ((*context).effect_call.unwrap())(context, &request, &mut (*output).reply)
    }
}

unsafe extern "C" fn begin(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    for _ in 0..2 {
        let status = unsafe { checkpoint(context) };
        if status != STATUS_OK {
            return status;
        }
    }
    let request = EffectRequest {
        operation: EFFECT_REQUEST,
        handler: 2,
        target: unsafe { *arguments },
        arguments: std::ptr::null(),
        count: 0,
        callback: 8,
        timeout_ms: unsafe { *arguments.add(1) },
        record_mask: 0,
        list_mask: 0,
        bytes_mask: 0,
        string_mask: 0,
    };
    let mut id = 0;
    let status = unsafe { ((*context).effect_call.unwrap())(context, &request, &mut id) };
    if status != STATUS_OK {
        return status;
    }
    let mode = unsafe { *arguments.add(2) };
    if mode == 1 {
        let request = EffectRequest {
            operation: EFFECT_CANCEL_REQUEST,
            handler: 0,
            target: id,
            arguments: std::ptr::null(),
            count: 0,
            callback: 0,
            timeout_ms: 0,
            record_mask: 0,
            list_mask: 0,
            bytes_mask: 0,
            string_mask: 0,
        };
        let status = unsafe { ((*context).effect_call.unwrap())(context, &request, &mut id) };
        if status != STATUS_OK {
            return status;
        }
    }
    if mode == 2 {
        return STATUS_DIVISION_BY_ZERO;
    }
    if mode >= 3 {
        let status = unsafe { checkpoint(context) };
        if status != STATUS_OK {
            return status;
        }
        let request = EffectRequest {
            operation: EFFECT_CANCEL_REQUEST,
            handler: 0,
            target: if mode == 3 { id } else { unsafe { *arguments } },
            arguments: std::ptr::null(),
            count: 0,
            callback: 0,
            timeout_ms: 0,
            record_mask: 0,
            list_mask: 0,
            bytes_mask: 0,
            string_mask: 0,
        };
        let status = unsafe { ((*context).effect_call.unwrap())(context, &request, &mut id) };
        if status != STATUS_OK {
            return status;
        }
    }
    unsafe {
        *output = ScalarOutput {
            state: state + 9,
            reply: id,
        };
    }
    STATUS_OK
}

unsafe extern "C" fn done(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe {
        *output = ScalarOutput {
            state: *arguments.add(1) * 100 + *arguments.add(2),
            reply: 0,
        };
    }
    STATUS_OK
}

pub(super) fn fixture() -> ScalarModule {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.entries[3] = begin;
    data.manifest.handlers[3].parameters = vec![
        ("target".into(), ValueType::Int),
        ("timeout".into(), ValueType::Int),
        ("mode".into(), ValueType::Int),
    ];
    data.manifest.handlers[3].reply = Some(ValueType::Int);
    data.entries[4] = identity;
    data.manifest.handlers[4].reply = Some(ValueType::Int);
    data.manifest.handlers.push(Handler {
        name: "Done".into(),
        parameters: vec![
            ("id".into(), ValueType::RequestId),
            ("status".into(), ValueType::Int),
            ("value".into(), ValueType::Int),
        ],
        reply: None,
    });
    let mut entries = data.entries.to_vec();
    entries.push(done);
    data.entries = entries.into_boxed_slice();
    data.manifest.validate().unwrap();
    module
}

fn capability(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid) -> i64 {
    let ticket = worker
        .request(
            pid,
            &module.handler("Fail").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    match worker.take_reply(ticket).unwrap().unwrap() {
        RequestOutcome::Reply(value) => value,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn full_completion_mailbox_retains_result_and_prepaid_message_until_space_exists() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
    let owner = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 60000, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    worker
        .send(owner, &module.handler("Add").unwrap(), &[5], 128)
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(target));
    assert_eq!(worker.queued(owner).unwrap(), 1);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(owner));
    assert!(matches!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Reply(_))
    ));
    assert_eq!(vm.used().requests, 1);
    assert_eq!(worker.resources(owner).unwrap().messages, 2);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(owner));
    assert_eq!(worker.committed_state(owner).unwrap(), 14);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(owner));
    assert_eq!(worker.committed_state(owner).unwrap(), 0);
    assert_eq!(vm.used().requests, 0);
    assert_eq!(vm.used().messages, 0);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn requester_exit_retains_request_ticket_until_target_finishes_without_notifying_reused_slot() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
    let owner = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 60000, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    worker.stop(owner).unwrap();
    assert_eq!(worker.queued(target).unwrap(), 1);
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(vm.used().requests, 1);
    let replacement = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    assert_eq!(worker.queued(replacement).unwrap(), 0);
    assert_eq!(worker.committed_state(replacement).unwrap(), 0);
    assert_eq!(vm.used().requests, 0);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn timeout_event_can_queue_before_target_starts_without_releasing_request_ticket() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant_accounted(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 2).unwrap();
    worker.enable_wait().unwrap();
    let owner = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 0, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    assert_eq!(worker.queued(target).unwrap(), 1);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    assert_eq!(worker.queued(owner).unwrap(), 1);
    assert_eq!(worker.resources(owner).unwrap().requests, 2);
    let baseline = vm.used();
    assert!(
        worker
            .request(
                target,
                &module.handler("Get").unwrap(),
                &[],
                128,
                Duration::ZERO
            )
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    worker.stop(owner).unwrap();
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(vm.used().requests, 1);
    assert_eq!(vm.used().messages, 1);
    worker.stop(target).unwrap();
    assert_eq!(vm.used().requests, 0);
    assert_eq!(vm.used().messages, 0);
    drop((worker, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn requester_account_and_pending_capacity_rejection_roll_back_event_and_target_message() {
    let module = fixture();
    for bounded in 0..4 {
        let vm = ResourceAccount::vm(limits(32 << 20));
        let tenant = vm.tenant(limits(32 << 20)).unwrap();
        let mut worker =
            ScalarWorker::with_request_capacity(&tenant, 2, if bounded == 0 { 1 } else { 4 })
                .unwrap();
        let mut local = limits(8 << 20);
        if bounded == 1 {
            local.requests = 1;
        }
        if bounded == 2 {
            local.messages = 1;
        }
        let owner = worker.spawn(&module, local, 2).unwrap();
        let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
        let target_id = capability(&mut worker, &module, target);
        let timeout = if bounded == 3 { -1 } else { 60000 };
        let ticket = worker
            .request(
                owner,
                &module.handler("Ack").unwrap(),
                &[target_id, timeout, 0],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
        assert_eq!(worker.queued(target).unwrap(), 0);
        assert_eq!(vm.used().messages, 1);
        assert_eq!(vm.used().requests, 1);
        assert!(matches!(
            worker.tick(128).unwrap(),
            WorkerEvent::Exited { .. }
        ));
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::RequestRejected)
        );
        assert_eq!(
            worker.take_reply(ticket).unwrap(),
            Some(RequestOutcome::Failed)
        );
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn language_request_vm_and_tenant_limits_reject_before_queueing_and_restore_all_charges() {
    let module = fixture();
    for level in 0..2 {
        for dimension in 0..3 {
            let vm = ResourceAccount::vm(limits(32 << 20));
            let tenant = vm.tenant(limits(32 << 20)).unwrap();
            let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
            let owner = worker.spawn(&module, limits(8 << 20), 2).unwrap();
            let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
            let target_id = capability(&mut worker, &module, target);
            let ticket = worker
                .request(
                    owner,
                    &module.handler("Ack").unwrap(),
                    &[target_id, 60000, 0],
                    128,
                    Duration::from_secs(60),
                )
                .unwrap();
            assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(owner));
            let account = if level == 0 { &vm } else { &tenant };
            let mut amount = Resources::default();
            match dimension {
                0 => amount.bytes = account.limit().bytes - account.used().bytes,
                1 => amount.messages = account.limit().messages - account.used().messages,
                _ => amount.requests = account.limit().requests - account.used().requests,
            }
            let blocker = account.reserve(amount).unwrap();
            let before = vm.used();
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
            assert_eq!(worker.queued(target).unwrap(), 0);
            assert_eq!(vm.used(), before);
            assert!(matches!(
                worker.tick(128).unwrap(),
                WorkerEvent::Exited { .. }
            ));
            assert_eq!(
                worker.take_reply(ticket).unwrap(),
                Some(RequestOutcome::Failed)
            );
            drop((blocker, worker));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

#[test]
fn target_stop_completes_once_and_pending_generation_does_not_accept_late_reply() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
    let owner = worker.spawn(&module, limits(8 << 20), 2).unwrap();
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 60000, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    worker.stop(target).unwrap();
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    assert!(matches!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Reply(_))
    ));
    assert_eq!(worker.committed_state(owner).unwrap(), 300);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Idle);
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 0, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Delivered(target));
    for _ in 0..16 {
        worker.tick(128).unwrap();
    }
    assert!(matches!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Reply(_))
    ));
    assert_eq!(worker.committed_state(owner).unwrap(), 100);
    assert_eq!(worker.queued(owner).unwrap(), 0);
    assert_eq!(vm.used().requests, 0);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn cancel_cannot_replace_selected_reply_or_consume_a_delivered_or_forged_request() {
    let module = fixture();
    for (mode, fill) in [(3, true), (3, false), (4, true)] {
        let vm = ResourceAccount::vm(limits(32 << 20));
        let tenant = vm.tenant(limits(32 << 20)).unwrap();
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
        let owner = worker.spawn(&module, limits(8 << 20), 1).unwrap();
        let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
        let target_id = capability(&mut worker, &module, target);
        let ticket = worker
            .request(
                owner,
                &module.handler("Ack").unwrap(),
                &[target_id, 60000, mode],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
        if fill {
            worker
                .send(owner, &module.handler("Add").unwrap(), &[5], 128)
                .unwrap();
        }
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(target));
        for _ in 0..16 {
            worker.tick(128).unwrap();
        }
        if fill && mode == 3 {
            assert!(matches!(
                worker.take_reply(ticket).unwrap(),
                Some(RequestOutcome::Reply(_))
            ));
            assert_eq!(worker.committed_state(owner).unwrap(), 0);
        } else {
            assert_eq!(
                worker.take_reply(ticket).unwrap(),
                Some(RequestOutcome::Failed)
            );
            assert_eq!(
                worker.last_exit().unwrap().execution.fault,
                Some(ExecutionFault::RequestRejected)
            );
        }
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn full_target_rolls_back_prepaid_completion_and_public_apis_cannot_inject_system_events() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).unwrap();
    let owner = worker.spawn(&module, limits(8 << 20), 2).unwrap();
    let target = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let target_id = capability(&mut worker, &module, target);
    let done = module.handler("Done").unwrap();
    let before = vm.used();
    assert!(worker.send(owner, &done, &[1, 0, 7], 128).is_err());
    assert!(
        worker
            .request(owner, &done, &[1, 0, 7], 128, Duration::from_secs(60))
            .is_err()
    );
    assert_eq!(vm.used(), before);
    let mut process = module.instantiate().unwrap();
    assert!(
        process
            .invoke_handler(&done, &[1, 0, 7], 128, &CancellationToken::default())
            .is_err()
    );
    drop(process);
    let ticket = worker
        .request(
            owner,
            &module.handler("Ack").unwrap(),
            &[target_id, 60000, 0],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    worker
        .send(target, &module.handler("Add").unwrap(), &[5], 128)
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
    assert_eq!(worker.queued(target).unwrap(), 1);
    assert_eq!(vm.used().requests, 1);
    assert_eq!(worker.resources(owner).unwrap().messages, 1);
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(ExecutionFault::RequestRejected)
    );
    assert_eq!(worker.committed_state(target).unwrap(), 5);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}
