use std::path::Path;
use std::time::{Duration, Instant};

use wings_abi::ValueType;
use wings_vm::{
    ExecutionFault, MessageValue, RequestOutcome, ResourceAccount, Resources, RestartPolicy,
    ScalarModule, ScalarSupervisor, ScalarWorker, Scheduler, SchedulerConfig, SchedulerExitReason,
    StackPool, SupervisorEvent, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let left = deadline.saturating_duration_since(Instant::now());
    assert!(!left.is_zero(), "record reply stage deadline");
    left
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    loop {
        remaining(deadline);
        if worker.tick(1).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
}

fn direct(module: &ScalarModule, state: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let account = tenant.process_accounted(limits()).unwrap();
    let shared = StackPool::new_accounted(tenant, 1, 0).unwrap();
    let pool = shared.for_process(&account).unwrap();
    let mut process = module.instantiate_with_heap(&pool, &account, 8).unwrap();
    for value in [i64::MIN, i64::MAX, 0] {
        assert_eq!(process.invoke("Value", &[value]).unwrap(), Some(value));
    }
    assert_eq!(process.invoke("Current", &[]).unwrap(), Some(3));
    assert!(process.invoke("Echo", &[0]).is_err());
    drop(process);
    let mut process = state.instantiate_with_heap(&pool, &account, 8).unwrap();
    assert_eq!(process.invoke("Current", &[]).unwrap(), Some(5));
    for value in [i64::MIN, i64::MAX, 37] {
        assert_eq!(process.invoke("Put", &[value]).unwrap(), Some(value));
        assert_eq!(process.invoke("Current", &[]).unwrap(), Some(value));
        assert_eq!(process.invoke("Read", &[]).unwrap(), Some(value));
    }
    remaining(deadline);
}

fn host_worker(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
    worker.enable_wait().unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 2, 8).unwrap();
    for value in [i64::MIN, i64::MAX, 0] {
        let request = worker
            .request(
                owner,
                &module.handler("Value").unwrap(),
                &[value],
                4096,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(value))
        );
    }
    let echoed = worker
        .request_values(
            owner,
            &module.handler("Echo").unwrap(),
            &[MessageValue::RecordInt(37)],
            4096,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(echoed).unwrap(),
        Some(RequestOutcome::Reply(37))
    );
    let retained = worker
        .request(
            owner,
            &module.handler("Current").unwrap(),
            &[],
            4096,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    let failed = worker
        .request(
            owner,
            &module.handler("Fail").unwrap(),
            &[],
            4096,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(failed).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(worker.last_exit().unwrap().committed_state, Some(37));
    assert_eq!(
        worker.take_reply(retained).unwrap(),
        Some(RequestOutcome::Reply(37))
    );
    let owner = worker.spawn_with_heap(module, limits(), 2, 8).unwrap();
    let sender = worker
        .open_request_ingress(owner, &module.handler("Value").unwrap(), 2, 4096)
        .unwrap();
    let (mut request, _) = sender.request(&[91], remaining(deadline)).unwrap();
    assert_eq!(request.reply_type(), ValueType::Ref);
    drain(&mut worker, deadline);
    drop(worker);
    assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Reply(91)));
    assert!(request.try_take().is_err());
    remaining(deadline);
}

fn supervised(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = ScalarSupervisor::new(tenant, 1, 2).unwrap();
    let child = supervisor
        .spawn_with_heap(module, limits(), 2, 8, RestartPolicy::Never)
        .unwrap();
    let owner = supervisor.current_pid(child).unwrap();
    for value in [i64::MIN, i64::MAX, 0] {
        let request = supervisor
            .request_values(
                owner,
                &module.handler("Value").unwrap(),
                &[MessageValue::Int(value)],
                4096,
                remaining(deadline),
            )
            .unwrap();
        loop {
            remaining(deadline);
            match supervisor.tick(1).unwrap() {
                SupervisorEvent::Worker(WorkerEvent::Idle) => break,
                SupervisorEvent::Worker(WorkerEvent::Suspended(_) | WorkerEvent::Delivered(_)) => {}
                event => panic!("unexpected record reply supervisor event: {event:?}"),
            }
        }
        assert_eq!(
            supervisor.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(value))
        );
    }
    supervisor.stop(child).unwrap();
    remaining(deadline);
}

fn scheduled_host(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 4,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let owner = handle.spawn_with_heap(module, limits(), 2, 8).unwrap();
    for value in [i64::MIN, i64::MAX, 0] {
        let mut request = handle
            .request(
                owner,
                &module.handler("Value").unwrap(),
                &[value],
                4096,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(request.reply_type(), ValueType::Ref);
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(value))
        );
    }
    let mut echoed = handle
        .request_values(
            owner,
            &module.handler("Echo").unwrap(),
            &[MessageValue::RecordInt(37)],
            4096,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        echoed.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Reply(37))
    );
    let mut retained = handle
        .request(
            owner,
            &module.handler("Current").unwrap(),
            &[],
            4096,
            remaining(deadline),
        )
        .unwrap();
    loop {
        remaining(deadline);
        let snapshot = handle.snapshot(owner).unwrap();
        if snapshot.owner.is_none() && snapshot.queued == 0 {
            break;
        }
        std::thread::yield_now();
    }
    handle.stop(owner).unwrap();
    assert_eq!(
        retained.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Reply(37))
    );
    assert!(retained.try_take().is_err());
    drop((retained, echoed, handle, scheduler));
    remaining(deadline);
}

fn arguments(mode: usize, same: bool) -> [i64; 5] {
    [
        37,
        if mode == 1 { 0 } else { 60000 },
        i64::from(mode == 2),
        i64::from(mode == 3),
        i64::from(same),
    ]
}

fn check(value: i64, mode: usize, same: bool) {
    let status = value / 1000 - 10;
    match mode {
        0 => assert_eq!(status, 0),
        1 => assert_eq!(status, 1),
        2 if same => assert_eq!(status, 2),
        2 => assert!(matches!(status, 0 | 2)),
        3 => assert_eq!(status, 3),
        _ => unreachable!(),
    }
    assert_eq!(value % 1000, if status == 0 { 37 } else { 0 });
}

fn language_worker(module: &ScalarModule, tenant: &ResourceAccount, mode: usize, same: bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 3).unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 3, 8).unwrap();
    assert!(
        worker
            .send(owner, &module.handler("Done").unwrap(), &[1, 0, 0], 128)
            .is_err()
    );
    let request = worker
        .request(
            owner,
            &module.handler("Start").unwrap(),
            &arguments(mode, same),
            8192,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    let outcome = worker.take_reply(request).unwrap();
    if mode == 2 && !same && outcome == Some(RequestOutcome::Failed) {
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, owner);
        assert_eq!(exit.handler(), "Start");
        assert_eq!(exit.committed_state, Some(0));
        assert_eq!(exit.execution.fault, Some(ExecutionFault::RequestRejected));
    } else if same && mode == 3 {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, owner);
        assert_eq!(exit.committed_state, Some(9));
        assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    } else {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        check(worker.committed_state(owner).unwrap(), mode, same);
    }
    if !same {
        if mode == 3 {
            let exit = worker.last_exit().unwrap();
            assert_ne!(exit.pid, owner);
            assert_eq!(exit.committed_state, Some(0));
            assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
        } else {
            let child = worker.pids().find(|pid| *pid != owner).unwrap();
            assert_eq!(worker.committed_state(child).unwrap(), 1);
        }
    }
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
}

fn language_scheduled(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    workers: usize,
    mode: usize,
    same: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 3,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let owner = handle.spawn_with_heap(module, limits(), 3, 8).unwrap();
    assert!(
        handle
            .send(owner, &module.handler("Done").unwrap(), &[1, 0, 0], 128)
            .is_err()
    );
    let mut request = handle
        .request(
            owner,
            &module.handler("Start").unwrap(),
            &arguments(mode, same),
            8192,
            remaining(deadline),
        )
        .unwrap();
    let outcome = request.wait(remaining(deadline)).unwrap();
    if (same && mode == 3) || (mode == 2 && !same && outcome == Some(RequestOutcome::Failed)) {
        loop {
            remaining(deadline);
            if let Some(exit) = handle.last_exit() {
                assert_eq!(exit.pid, owner);
                if same {
                    assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
                    assert_eq!(exit.committed_state, Some(9));
                    assert_eq!(
                        exit.reason,
                        SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
                    );
                } else {
                    assert_eq!(exit.handler(), "Start");
                    assert_eq!(exit.committed_state, Some(0));
                    assert_eq!(
                        exit.reason,
                        SchedulerExitReason::Managed(ExecutionFault::RequestRejected)
                    );
                }
                break;
            }
            std::thread::yield_now();
        }
    } else {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        loop {
            remaining(deadline);
            let snapshot = handle.snapshot(owner).unwrap();
            if snapshot.owner.is_none()
                && snapshot.queued == 0
                && snapshot.committed_state.unwrap() >= 10000
            {
                check(snapshot.committed_state.unwrap(), mode, same);
                break;
            }
            std::thread::yield_now();
        }
    }
    drop((request, handle, scheduler));
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
    remaining(deadline);
}

fn import_failure(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    if workers == 0 {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 3).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 2, 1).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("StartImportFail").unwrap(),
                &[],
                8192,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(9))
        );
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, owner);
        assert_eq!(exit.handler(), "Done");
        assert_eq!(exit.execution.fault, Some(ExecutionFault::HeapExhausted));
        let child = worker.pids().next().unwrap();
        let request = worker
            .request(
                child,
                &module.handler("Current").unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(5))
        );
    } else {
        let scheduler = Scheduler::new(
            tenant,
            SchedulerConfig {
                workers,
                capacity: 2,
                requests: 3,
                slice_steps: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let owner = handle.spawn_with_heap(module, limits(), 2, 1).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("StartImportFail").unwrap(),
                &[],
                8192,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(9))
        );
        loop {
            remaining(deadline);
            if let Some(exit) = handle.last_exit() {
                assert_eq!(exit.pid, owner);
                assert_eq!(exit.handler(), "Done");
                assert_eq!(
                    exit.reason,
                    SchedulerExitReason::Managed(ExecutionFault::HeapExhausted)
                );
                break;
            }
            std::thread::yield_now();
        }
        drop((request, handle, scheduler));
    }
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
    remaining(deadline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let state = unsafe { ScalarModule::load_accounted(Path::new(&arguments[2]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    direct(&module, &state, &tenant);
    host_worker(&module, &tenant);
    supervised(&module, &tenant);
    for workers in [1, 2] {
        scheduled_host(&module, &tenant, workers);
    }
    for mode in 0..4 {
        for same in [false, true] {
            language_worker(&module, &tenant, mode, same);
            for workers in [1, 2] {
                language_scheduled(&module, &tenant, workers, mode, same);
            }
        }
    }
    for workers in [0, 1, 2] {
        import_failure(&state, &tenant, workers);
    }
    drop((module, state, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Copied record replies verified: 24 bounded language scenarios, host/ingress/supervision copies, persistent roots, GC, failed imports and cleanup; not complete MVP"
    );
}
