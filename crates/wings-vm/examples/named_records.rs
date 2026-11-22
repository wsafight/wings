use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, RequestOutcome, ResourceAccount,
    Resources, RestartPolicy, RestartStatus, ScalarModule, ScalarSupervisor, ScalarWorker,
    Scheduler, SchedulerConfig, StackPool, SupervisorEvent, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .expect("named record phase exceeded three seconds")
}

fn synchronous(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = tenant.used();
    for capacity in [1, 6] {
        assert!(
            module
                .instantiate_with_heap(&bound, &account, capacity)
                .is_err()
        );
        assert_eq!(tenant.used(), baseline);
    }
    let mut process = module.instantiate_with_heap(&bound, &account, 16).unwrap();
    assert_eq!(process.invoke("read", &[]).unwrap(), Some(33));
    assert_eq!(process.invoke("select", &[0]).unwrap(), Some(8));
    assert_eq!(process.invoke("select", &[1]).unwrap(), Some(7));
    assert_eq!(process.invoke("read", &[]).unwrap(), Some(33));
    for expected in 8..40 {
        let mut execution = process
            .start_controlled(
                "advance",
                &[],
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        loop {
            remaining(deadline);
            match execution.resume(1).unwrap() {
                ExecutionStatus::Suspended => {}
                ExecutionStatus::Complete(value) => {
                    assert_eq!(value, Some(expected));
                    break;
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        drop(execution);
        assert_eq!(
            process.invoke("read", &[]).unwrap(),
            Some(33 + (expected - 7) * 2)
        );
        assert_eq!(process.heap_stats().unwrap().allocated, 7);
    }
    assert_eq!(process.invoke("count", &[43]).unwrap(), Some(43));
    assert_eq!(process.invoke("empty_value", &[]).unwrap(), Some(1));
    assert_eq!(process.invoke("wide_value", &[]).unwrap(), Some(3));
    drop(process);
    assert_eq!(tenant.used(), baseline);
    let mut process = module.instantiate_with_heap(&bound, &account, 7).unwrap();
    assert!(
        process
            .invoke("advance", &[])
            .unwrap_err()
            .contains("heap exhausted")
    );
    assert_eq!(process.heap_stats().unwrap().allocated, 7);
    drop(process);
    for budget in 0..32 {
        let mut process = module.instantiate_with_heap(&bound, &account, 16).unwrap();
        let mut execution = process
            .start_controlled(
                "fail",
                &[],
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        let error = loop {
            remaining(deadline);
            match execution.resume(1) {
                Ok(ExecutionStatus::Suspended) => {}
                Err(error) => break error,
                other => panic!("failed record candidate returned: {other:?}"),
            }
        };
        assert!(error.contains("budget exhausted") || error.contains("division by zero"));
        drop(execution);
        drop(process);
        assert_eq!(tenant.used(), baseline);
    }
    for slice in 1..24 {
        let mut process = module.instantiate_with_heap(&bound, &account, 16).unwrap();
        let cancellation = CancellationToken::new_accounted(&account).unwrap();
        let mut execution = process
            .start_controlled(
                "advance",
                &[],
                4096,
                &cancellation,
                &PreemptionToken::default(),
            )
            .unwrap();
        if execution.resume(slice).unwrap() == ExecutionStatus::Suspended {
            cancellation.cancel();
            assert!(execution.resume(4096).unwrap_err().contains("cancelled"));
        }
        drop(execution);
        drop((process, cancellation));
        assert_eq!(tenant.used(), baseline);
        remaining(deadline);
    }
}

fn worker(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::new(tenant, 2).unwrap();
    let first = worker.spawn_with_heap(module, limits(), 2, 16).unwrap();
    let healthy = worker.spawn_with_heap(module, limits(), 2, 16).unwrap();
    for (owner, name, expected) in [
        (first, "advance", Some(RequestOutcome::Reply(8))),
        (first, "fail", Some(RequestOutcome::Failed)),
        (healthy, "read", Some(RequestOutcome::Reply(33))),
    ] {
        let request = worker
            .request(
                owner,
                &module.handler(name).unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        loop {
            remaining(deadline);
            if worker.tick(1).unwrap() == WorkerEvent::Idle {
                break;
            }
        }
        assert_eq!(worker.take_reply(request).unwrap(), expected);
    }
    worker.stop(healthy).unwrap();
}

fn scheduler(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
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
    let first = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
    let healthy = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
    for (owner, name, expected) in [
        (first, "advance", RequestOutcome::Reply(8)),
        (first, "fail", RequestOutcome::Failed),
        (healthy, "read", RequestOutcome::Reply(33)),
    ] {
        let mut request = handle
            .request(
                owner,
                &module.handler(name).unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(request.wait(remaining(deadline)).unwrap(), Some(expected));
    }
    handle.stop(healthy).unwrap();
}

fn supervised(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = ScalarSupervisor::new(tenant, 1, 4).unwrap();
    let child = supervisor
        .spawn_with_heap(
            module,
            limits(),
            2,
            16,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    for attempt in 0..2 {
        let owner = supervisor.current_pid(child).unwrap();
        for (name, expected) in [("read", 33), ("advance", 8)] {
            let request = supervisor
                .request(
                    owner,
                    &module.handler(name).unwrap(),
                    &[],
                    4096,
                    remaining(deadline),
                )
                .unwrap();
            loop {
                remaining(deadline);
                match supervisor.tick(1).unwrap() {
                    SupervisorEvent::Worker(WorkerEvent::Suspended(_)) => {}
                    SupervisorEvent::Worker(WorkerEvent::Delivered(actual)) => {
                        assert_eq!(actual, owner);
                        break;
                    }
                    other => panic!("unexpected supervised event: {other:?}"),
                }
            }
            assert_eq!(
                supervisor.take_reply(request).unwrap(),
                Some(RequestOutcome::Reply(expected))
            );
        }
        let request = supervisor
            .request(
                owner,
                &module.handler("fail").unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        loop {
            remaining(deadline);
            match supervisor.tick(1).unwrap() {
                SupervisorEvent::Worker(WorkerEvent::Suspended(_)) => {}
                SupervisorEvent::Exited {
                    child: actual_child,
                    pid,
                    reason,
                    restart,
                } => {
                    assert_eq!((actual_child, pid), (child, owner));
                    assert!(reason.contains("division by zero"));
                    if attempt == 0 {
                        let next = supervisor.current_pid(child).unwrap();
                        assert_ne!(next, owner);
                        assert_eq!(restart, RestartStatus::Restarted(next));
                        assert!(
                            supervisor
                                .send(owner, &module.handler("advance").unwrap(), &[], 4096)
                                .is_err()
                        );
                    } else {
                        assert_eq!(restart, RestartStatus::IntensityExceeded);
                    }
                    break;
                }
                other => panic!("unexpected supervised failure: {other:?}"),
            }
        }
        assert_eq!(
            supervisor.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let baseline = vm.used();
    synchronous(&module, &tenant);
    assert_eq!(vm.used(), baseline);
    worker(&module, &tenant);
    assert_eq!(vm.used(), baseline);
    supervised(&module, &tenant);
    assert_eq!(vm.used(), baseline);
    for workers in [1, 2] {
        scheduler(&module, &tenant, workers);
        assert_eq!(vm.used(), baseline);
    }
    drop(tenant);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Named records verified: nested types, GC, slices, cancellation, quotas, isolated workers and cleanup"
    );
}
