use super::*;
use wings_abi::{EFFECT_SELF_PID, EFFECT_SEND, EFFECT_SPAWN_SELF, EffectRequest};

thread_local! {
    static FAIL_INITIALIZER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

unsafe extern "C" fn identity(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let request = EffectRequest {
        operation: EFFECT_SELF_PID,
        callback: 0,
        timeout_ms: 0,
        record_mask: 0,
        handler: 0,
        target: 0,
        arguments: std::ptr::null(),
        count: 0,
    };
    unsafe { ((*context).effect_call.unwrap())(context, &request, &mut (*output).reply) }
}

unsafe extern "C" fn send_to(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let request = EffectRequest {
        operation: EFFECT_SEND,
        callback: 0,
        timeout_ms: 0,
        record_mask: 0,
        handler: 0,
        target: unsafe { *arguments },
        arguments: unsafe { arguments.add(1) },
        count: 1,
    };
    let mut ignored = 0;
    let status = unsafe { ((*context).effect_call.unwrap())(context, &request, &mut ignored) };
    unsafe {
        (*output).state = state;
    }
    status
}

unsafe extern "C" fn spawn_child(
    state: i64,
    _arguments: *const i64,
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
        operation: EFFECT_SPAWN_SELF,
        callback: 0,
        timeout_ms: 0,
        record_mask: 0,
        handler: 0,
        target: 0,
        arguments: std::ptr::null(),
        count: 0,
    };
    let status =
        unsafe { ((*context).effect_call.unwrap())(context, &request, &mut (*output).reply) };
    unsafe {
        (*output).state = state;
    }
    status
}

fn fixture() -> ScalarModule {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.entries[2] = identity;
    data.entries[1] = send_to;
    data.manifest.handlers[1].parameters = vec![
        ("target".into(), ValueType::Int),
        ("value".into(), ValueType::Int),
    ];
    data.manifest.handlers[1].reply = None;
    data.entries[3] = spawn_child;
    data.manifest.handlers[3].reply = Some(ValueType::Int);
    module
}

fn capability(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid) -> i64 {
    let request = worker
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    match worker.take_reply(request).unwrap().unwrap() {
        RequestOutcome::Reply(value) => value,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn capabilities_do_not_alias_exited_slots_other_workers_or_other_modules() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 2).unwrap();
    let old = capability(&mut worker, &module, pid);
    worker.stop(pid).unwrap();
    let replacement = worker.spawn(&module, limits(8 << 20), 2).unwrap();
    let current = capability(&mut worker, &module, replacement);
    assert_ne!(old, current);
    let mut other = ScalarWorker::new(&tenant, 1).unwrap();
    let foreign = other.spawn(&module, limits(8 << 20), 2).unwrap();
    let foreign = capability(&mut other, &module, foreign);
    let another_module = fixture();
    let another = worker.spawn(&another_module, limits(8 << 20), 2).unwrap();
    let another = capability(&mut worker, &another_module, another);
    for target in [old, foreign, another, -1, 0, i64::MAX] {
        let sender = worker.spawn(&module, limits(8 << 20), 1).unwrap();
        worker
            .send(sender, &module.handler("Bump").unwrap(), &[target, 7], 128)
            .unwrap();
        let mut exited = false;
        for _ in 0..8 {
            if matches!(worker.tick(128).unwrap(), WorkerEvent::Exited { pid, .. } if pid == sender)
            {
                exited = true;
                break;
            }
        }
        assert!(exited);
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::SendRejected)
        );
        assert_eq!(worker.queued(replacement).unwrap(), 0);
    }
    drop((other, worker));
    assert_eq!(vm.used(), Resources::default());
}

unsafe extern "C" fn conditional_initializer(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    if FAIL_INITIALIZER.with(std::cell::Cell::get) {
        STATUS_DIVISION_BY_ZERO
    } else {
        unsafe { initialize(state, arguments, output, context) }
    }
}

#[test]
fn failed_child_initialization_and_process_admission_leave_no_partial_actor() {
    let module = fixture();
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(32 << 20)).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let blocker = tenant
        .reserve(Resources {
            processes: tenant.limit().processes - tenant.used().processes,
            ..Resources::default()
        })
        .unwrap();
    let request = worker
        .request(
            pid,
            &module.handler("Ack").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    assert_eq!(worker.process_count(), 1);
    assert!(matches!(
        worker.tick(128).unwrap(),
        WorkerEvent::Exited { .. }
    ));
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(ExecutionFault::SpawnRejected)
    );
    drop(blocker);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    let mut module = fixture();
    Arc::get_mut(&mut module.inner).unwrap().initializer = conditional_initializer;
    let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let request = worker
        .request(
            pid,
            &module.handler("Ack").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    FAIL_INITIALIZER.with(|flag| flag.set(true));
    let event = worker.tick(128);
    FAIL_INITIALIZER.with(|flag| flag.set(false));
    assert_eq!(event.unwrap(), WorkerEvent::Suspended(pid));
    assert_eq!(worker.process_count(), 1);
    assert_eq!(vm.used().processes, 1);
    assert!(matches!(
        worker.tick(128).unwrap(),
        WorkerEvent::Exited { .. }
    ));
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(ExecutionFault::SpawnRejected)
    );
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Failed)
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn child_byte_admission_rejects_at_vm_and_tenant_without_partial_resources() {
    let module = fixture();
    for level in 0..2 {
        let vm = ResourceAccount::vm(limits(32 << 20));
        let tenant = vm.tenant(limits(32 << 20)).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
        let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
        let request = worker
            .request(
                pid,
                &module.handler("Ack").unwrap(),
                &[],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
        let account = if level == 0 { &vm } else { &tenant };
        let blocker = account
            .reserve(Resources {
                bytes: account.limit().bytes - account.used().bytes,
                ..Resources::default()
            })
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.process_count(), 1);
        assert!(matches!(
            worker.tick(128).unwrap(),
            WorkerEvent::Exited { .. }
        ));
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::SpawnRejected)
        );
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
        drop((blocker, worker));
        assert_eq!(vm.used(), Resources::default());
    }
}
