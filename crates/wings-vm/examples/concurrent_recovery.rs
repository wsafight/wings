use std::path::Path;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use wings_vm::{
    ExecutionFault, IngressRequest, RequestOutcome, ResourceAccount, Resources, RestartPolicy,
    ScalarModule, SchedulerConfig, SchedulerExitReason, SchedulerHandle, SchedulerPid,
    SchedulerRestartStatus, SchedulerSupervisor, SchedulerSupervisorEvent, SchedulerTermination,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 8,
        messages: 64,
        requests: 64,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "concurrent recovery stage deadline");
    remaining
}

fn outcome(request: &mut IngressRequest, deadline: Instant) -> RequestOutcome {
    request.wait(remaining(deadline)).unwrap().unwrap()
}

fn event(supervisor: &mut SchedulerSupervisor, deadline: Instant) -> SchedulerSupervisorEvent {
    loop {
        remaining(deadline);
        if let Some(event) = supervisor.poll() {
            return event;
        }
        std::thread::yield_now();
    }
}

fn value(handle: &SchedulerHandle, pid: SchedulerPid) -> i64 {
    let state = handle.byte_state_snapshot(pid).unwrap().unwrap();
    assert_eq!(state.length, 4096);
    assert!(state.prefix[8..].iter().all(|byte| *byte == 0));
    i64::from_be_bytes(state.prefix[..8].try_into().unwrap())
}

fn verify(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = SchedulerSupervisor::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 17,
            slice_steps: 64,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = supervisor.handle();
    let policy = RestartPolicy::OnFailure {
        max_restarts: 1,
        within: Duration::from_secs(60),
    };
    let child = supervisor
        .spawn_with_heap(module, limits(), 16, 4, policy)
        .unwrap();
    let healthy = supervisor
        .spawn_with_heap(module, limits(), 1, 4, RestartPolicy::Never)
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let sibling = supervisor.current_pid(healthy).unwrap();
    let add = module.handler("Add").unwrap();
    assert_eq!(value(&handle, pid), 0);
    handle
        .send(pid, &module.handler("Hold").unwrap(), &[], u64::MAX)
        .unwrap();
    while handle.snapshot(pid).unwrap().execution.is_none() {
        remaining(deadline);
        std::thread::yield_now();
    }
    let pending = std::thread::scope(|scope| {
        let mut producers = Vec::with_capacity(2);
        for sender in 0..2 {
            let handle = &handle;
            let add = &add;
            producers.push(scope.spawn(move || {
                let mut results = Vec::with_capacity(8);
                for index in 0..8 {
                    let mut request = handle
                        .request(
                            pid,
                            add,
                            &[sender * 8 + index + 1],
                            4096,
                            if index < 2 {
                                Duration::ZERO
                            } else {
                                remaining(deadline)
                            },
                        )
                        .unwrap();
                    if index < 4 {
                        let expected = if index < 2 {
                            RequestOutcome::TimedOut
                        } else {
                            request.cancel().unwrap();
                            RequestOutcome::Cancelled
                        };
                        assert_eq!(outcome(&mut request, deadline), expected);
                        assert!(request.try_take().is_err());
                    }
                    results.push(request);
                }
                results
            }));
        }
        producers
            .into_iter()
            .map(|producer| producer.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(handle.snapshot(pid).unwrap().queued, 16);
    assert_eq!(handle.snapshot(pid).unwrap().resources.requests, 16);
    assert!(handle.byte_state_snapshot(pid).is_err());
    let before = tenant.used();
    assert_eq!(
        handle
            .request(pid, &add, &[999], 4096, remaining(deadline))
            .err()
            .unwrap(),
        "mailbox_full"
    );
    assert_eq!(tenant.used(), before);
    let mut pulse = handle
        .request(sibling, &add, &[7], 4096, remaining(deadline))
        .unwrap();
    assert_eq!(outcome(&mut pulse, deadline), RequestOutcome::Reply(7));
    assert_eq!(value(&handle, sibling), 7);
    let race = Barrier::new(3);
    let cancelled = std::thread::scope(|scope| {
        let mut consumers = Vec::with_capacity(2);
        for mut requests in pending {
            let race = &race;
            consumers.push(scope.spawn(move || {
                race.wait();
                let mut cancelled = 0;
                for request in requests.iter_mut().skip(4) {
                    request.cancel().unwrap();
                    match outcome(request, deadline) {
                        RequestOutcome::Cancelled => cancelled += 1,
                        RequestOutcome::Failed => {}
                        unexpected => panic!("unexpected stop/cancel outcome: {unexpected:?}"),
                    }
                    assert!(request.try_take().is_err());
                }
                cancelled
            }));
        }
        race.wait();
        handle.stop(pid).unwrap();
        consumers
            .into_iter()
            .map(|consumer| consumer.join().unwrap())
            .sum::<usize>()
    });
    assert!(cancelled <= 8);
    let stopped = event(&mut supervisor, deadline);
    assert_eq!(stopped.child, child);
    assert_eq!(stopped.pid, pid);
    assert_eq!(stopped.termination, SchedulerTermination::Stopped);
    assert_eq!(stopped.restart, SchedulerRestartStatus::Stopped);
    assert!(supervisor.current_pid(child).is_err());
    assert!(
        handle
            .request(pid, &add, &[999], 4096, remaining(deadline))
            .is_err()
    );
    assert_eq!(tenant.used().requests, 0);
    let child = supervisor
        .spawn_with_heap(module, limits(), 16, 4, policy)
        .unwrap();
    let mut current = supervisor.current_pid(child).unwrap();
    assert_ne!(current, pid);
    for attempt in 0..2 {
        assert_eq!(value(&handle, current), 0);
        let mut failure = handle
            .request(
                current,
                &module.handler("Fail").unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        failure.cancel().unwrap();
        assert!(matches!(
            outcome(&mut failure, deadline),
            RequestOutcome::Cancelled | RequestOutcome::Failed
        ));
        let failed = event(&mut supervisor, deadline);
        assert_eq!(failed.child, child);
        assert_eq!(failed.pid, current);
        let SchedulerTermination::Failed(exit) = failed.termination else {
            panic!("cancelled result must not retract the failing message")
        };
        assert_eq!(exit.handler(), "Fail");
        assert_eq!(
            exit.reason,
            SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
        );
        let state = exit.byte_state.unwrap();
        assert_eq!(state.length, 4096);
        assert!(state.prefix.iter().all(|byte| *byte == 0));
        assert!(
            handle
                .request(current, &add, &[999], 4096, remaining(deadline))
                .is_err()
        );
        if attempt == 0 {
            let SchedulerRestartStatus::Restarted(next) = failed.restart else {
                panic!("expected one fresh restart")
            };
            assert_ne!(next, current);
            assert_eq!(supervisor.current_pid(child).unwrap(), next);
            current = next;
        } else {
            assert_eq!(failed.restart, SchedulerRestartStatus::IntensityExceeded);
            assert!(supervisor.current_pid(child).is_err());
        }
    }
    assert_eq!(supervisor.current_pid(healthy).unwrap(), sibling);
    assert_eq!(value(&handle, sibling), 7);
    supervisor.stop(healthy).unwrap();
    assert!(supervisor.poll().is_none());
    drop((handle, supervisor));
    remaining(deadline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    for workers in [1, 2] {
        verify(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Concurrent recovery verified: retained expired/cancelled queues, stop races, independent Bytes state, fresh restart, intensity and cleanup; not a performance gate"
    );
}
