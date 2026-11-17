use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    ExecutionFault, IngressRequest, RequestOutcome, ResourceAccount, Resources, RestartPolicy,
    ScalarModule, Scheduler, SchedulerConfig, SchedulerExitReason, SchedulerRestartStatus,
    SchedulerSupervisor, SchedulerTermination,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 32,
        messages: 128,
        requests: 64,
    }
}

fn reply(mut request: IngressRequest) -> i64 {
    match request.wait(Duration::from_secs(5)).unwrap() {
        Some(RequestOutcome::Reply(value)) => value,
        outcome => panic!("unexpected scheduler reply: {outcome:?}"),
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let loops = unsafe { ScalarModule::load_accounted(Path::new(&arguments[2]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    for workers in [1, 2, 3] {
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers,
                capacity: 16,
                requests: 16,
                slice_steps: 1,
                quantum: Duration::from_millis(1),
                yield_samples: 0,
                lock_statistics: false,
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let begin = module.handler("Begin").unwrap();
        let read = module.handler("Read").unwrap();
        let parents: Vec<_> = (0..6)
            .map(|_| handle.spawn_with_heap(&module, limits(), 8, 16).unwrap())
            .collect();
        for (index, pid) in parents.iter().copied().enumerate() {
            assert_eq!(
                reply(
                    handle
                        .request(pid, &begin, &[index as i64], 4096, Duration::from_secs(5))
                        .unwrap()
                ),
                index as i64
            );
        }
        for (index, pid) in parents.iter().copied().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let value = reply(
                    handle
                        .request(pid, &read, &[], 4096, Duration::from_secs(5))
                        .unwrap(),
                );
                if value == 3 * index as i64 + 6 {
                    break;
                }
                assert_eq!(value, -1);
                assert!(Instant::now() < deadline);
            }
            let before = handle.snapshot(pid).unwrap();
            assert!(before.execution.unwrap().heap.unwrap().collections > 0);
            for _ in 0..3 {
                assert_eq!(
                    reply(
                        handle
                            .request(pid, &read, &[], 4096, Duration::from_secs(5))
                            .unwrap()
                    ),
                    3 * index as i64 + 6
                );
            }
            let after = handle.snapshot(pid).unwrap();
            assert_eq!(
                after.migrations - before.migrations,
                if workers == 1 { 0 } else { 3 }
            );
        }
        let busy = handle.spawn(&loops, limits(), 1).unwrap();
        handle
            .send(busy, &loops.handler("Forever").unwrap(), &[], u64::MAX)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.snapshot(busy).unwrap().execution.is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let owner = handle.snapshot(busy).unwrap().owner;
        for pid in &parents {
            reply(
                handle
                    .request(*pid, &read, &[], 4096, Duration::from_secs(5))
                    .unwrap(),
            );
        }
        assert_eq!(handle.snapshot(busy).unwrap().owner, owner);
        handle.stop(busy).unwrap();
        let paused: Vec<_> = (0..3)
            .map(|_| {
                let pid = handle.spawn(&loops, limits(), 1).unwrap();
                handle
                    .send(pid, &loops.handler("Forever").unwrap(), &[], u64::MAX)
                    .unwrap();
                pid
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while paused
            .iter()
            .any(|pid| handle.snapshot(*pid).unwrap().execution.is_none())
        {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let owners: Vec<_> = paused
            .iter()
            .map(|pid| handle.snapshot(*pid).unwrap().owner)
            .collect();
        assert!(owners.iter().all(Option::is_some));
        assert_eq!(
            reply(
                handle
                    .request(parents[0], &read, &[], 4096, Duration::from_secs(5))
                    .unwrap()
            ),
            6
        );
        for (pid, owner) in paused.iter().zip(owners) {
            assert_eq!(handle.snapshot(*pid).unwrap().owner, owner);
            handle.stop(*pid).unwrap();
        }
        assert_eq!(handle.last_exit(), None);
        let failing = handle.spawn(&loops, limits(), 2).unwrap();
        assert_eq!(
            reply(
                handle
                    .request(
                        failing,
                        &loops.handler("Count").unwrap(),
                        &[10],
                        4096,
                        Duration::from_secs(5)
                    )
                    .unwrap()
            ),
            10
        );
        let last_worker = handle.snapshot(failing).unwrap().last_worker.unwrap();
        let mut failure = handle
            .request(
                failing,
                &loops.handler("Fail").unwrap(),
                &[20],
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            failure.wait(Duration::from_secs(5)).unwrap(),
            Some(RequestOutcome::Failed)
        );
        let diagnostic = handle.last_exit().unwrap();
        assert_eq!(diagnostic.pid, failing);
        assert_eq!(diagnostic.handler(), "Fail");
        assert_eq!(diagnostic.worker, (last_worker + 1) % workers);
        assert_eq!(diagnostic.committed_state, Some(10));
        assert_eq!(
            diagnostic.reason,
            SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
        );
        assert!(handle.snapshot(failing).is_err());
        assert_eq!(
            reply(
                handle
                    .request(parents[0], &read, &[], 4096, Duration::from_secs(5))
                    .unwrap()
            ),
            6
        );
        assert_eq!(handle.last_exit(), Some(diagnostic));
        drop((handle, scheduler));
    }
    let mut supervisor = SchedulerSupervisor::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 4,
            requests: 4,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = supervisor.handle();
    let child = supervisor
        .spawn_with_heap(
            &module,
            limits(),
            4,
            8,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    for attempt in 0..2 {
        let pid = supervisor.current_pid(child).unwrap();
        assert_eq!(
            reply(
                handle
                    .request(
                        pid,
                        &module.handler("Read").unwrap(),
                        &[],
                        4096,
                        Duration::from_secs(5)
                    )
                    .unwrap()
            ),
            -1
        );
        let mut failed = handle
            .request(
                pid,
                &module.handler("Fail").unwrap(),
                &[],
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            failed.wait(Duration::from_secs(5)).unwrap(),
            Some(RequestOutcome::Failed)
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let event = loop {
            if let Some(event) = supervisor.poll() {
                break event;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(event.pid, pid);
        assert!(matches!(event.termination, SchedulerTermination::Failed(_)));
        if attempt == 0 {
            let SchedulerRestartStatus::Restarted(replacement) = event.restart else {
                panic!("missing replacement")
            };
            assert_ne!(pid, replacement);
        } else {
            assert_eq!(event.restart, SchedulerRestartStatus::IntensityExceeded);
        }
    }
    drop((supervisor, handle, module, loops, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Multi-worker scheduler verified: N > M actors, live-stack ownership, boundary migration, copied list/record messages, request events, GC and cleanup"
    );
}
