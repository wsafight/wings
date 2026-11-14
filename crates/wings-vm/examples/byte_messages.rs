use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    ExecutionFault, Pid, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    Scheduler, SchedulerConfig, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 32,
        requests: 16,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "byte message stage deadline");
    remaining
}

fn drain(worker: &mut ScalarWorker, deadline: Instant, slice: u64) {
    loop {
        remaining(deadline);
        if worker.tick(slice).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
}

fn request(
    worker: &mut ScalarWorker,
    module: &ScalarModule,
    pid: Pid,
    name: &str,
    arguments: &[i64],
    deadline: Instant,
) -> RequestOutcome {
    let id = worker
        .request(
            pid,
            &module.handler(name).unwrap(),
            arguments,
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(worker, deadline, 1);
    worker.take_reply(id).unwrap().unwrap()
}

fn score(length: i64, value: i64) -> i64 {
    length * 512 + if length == 0 { 0 } else { value * 2 }
}

fn single(module: &ScalarModule) {
    for (entry, arguments, parent_result, child_result, heap) in [
        (
            "Start",
            vec![0, 7, 0],
            Some(score(0, 7)),
            Some(score(0, 7)),
            10,
        ),
        (
            "Start",
            vec![65536, 255, 0],
            Some(score(65536, 255)),
            Some(score(65536, 255)),
            10,
        ),
        (
            "Start",
            vec![65536, 201, 1],
            None,
            Some(score(65536, 201)),
            10,
        ),
        ("Self", vec![65536, 17], Some(score(65536, 17)), None, 10),
        ("Self", vec![1, 255], Some(score(1, 255)), None, 10),
        (
            "Ask",
            vec![65536, 91, 60000],
            Some(score(65536, 91)),
            Some(score(65536, 91)),
            10,
        ),
        (
            "Ask",
            vec![65536, 77, 0],
            Some(score(65536, 77)),
            Some(score(65536, 77)),
            10,
        ),
        (
            "Mixed",
            vec![32768, 13],
            Some(score(32768, 13)),
            Some(score(8, 0)),
            10,
        ),
        (
            "Repeat",
            vec![32768],
            Some(score(32768, 23)),
            Some(score(8, 0)),
            10,
        ),
        ("Mixed", vec![0, 13], Some(0), Some(score(8, 0)), 10),
        (
            "Mixed",
            vec![65536, 13],
            Some(score(65536, 13)),
            Some(score(8, 0)),
            10,
        ),
        ("Repeat", vec![0], Some(0), Some(score(8, 0)), 10),
        ("Over", vec![], None, Some(score(8, 0)), 10),
        ("Repeat", vec![32769], None, Some(score(8, 0)), 10),
        ("Repeat", vec![32768], Some(score(32768, 23)), None, 2),
    ] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker.spawn_with_heap(module, limits(), 2, heap).unwrap();
        for name in ["Store", "Compute", "StorePair", "StoreMixed"] {
            assert!(
                worker
                    .send(parent, &module.handler(name).unwrap(), &[0], 4096)
                    .is_err()
            );
        }
        let reply = request(&mut worker, module, parent, entry, &arguments, deadline);
        assert_eq!(
            reply,
            parent_result.map_or(RequestOutcome::Failed, RequestOutcome::Reply),
            "{entry} {arguments:?}"
        );
        if parent_result.is_none() {
            let exit = worker.last_exit().unwrap();
            assert_eq!(exit.handler(), entry);
            assert_eq!(
                exit.execution.fault,
                Some(if entry == "Start" {
                    ExecutionFault::DivisionByZero
                } else {
                    ExecutionFault::SendRejected
                })
            );
            let state = exit.byte_state.unwrap();
            assert_eq!(state.length, 8);
            assert_eq!(state.prefix, [0; 32]);
        }
        if let Some(expected) = child_result {
            let child = worker.pids().find(|pid| *pid != parent);
            if let Some(child) = child {
                if matches!(entry, "Mixed" | "Repeat") && parent_result.is_some() {
                    let expected = if entry == "Mixed" {
                        score(arguments[0], arguments[1])
                            + score(65536 - arguments[0], arguments[1] + 1)
                            + 16
                    } else {
                        score(arguments[0], 23) * 2
                    };
                    assert_eq!(
                        request(&mut worker, module, child, "Result", &[], deadline),
                        RequestOutcome::Reply(expected)
                    );
                } else {
                    assert_eq!(
                        request(&mut worker, module, child, "Read", &[], deadline),
                        RequestOutcome::Reply(expected)
                    );
                }
            } else {
                panic!("accepted child must remain alive")
            }
        } else if entry == "Self" {
            assert_eq!(
                request(&mut worker, module, parent, "Read", &[], deadline),
                RequestOutcome::Reply(score(arguments[0], arguments[1]))
            );
        }
        if entry == "Ask" {
            let expected = if arguments[2] == 0 {
                1000000001
            } else {
                score(arguments[0], arguments[1]) + 1
            };
            assert_eq!(
                request(&mut worker, module, parent, "Result", &[], deadline),
                RequestOutcome::Reply(expected)
            );
        }
        if entry == "Repeat" && heap == 2 {
            let exit = worker.last_exit().unwrap();
            assert_eq!(exit.handler(), "StorePair");
            assert_eq!(exit.execution.fault, Some(ExecutionFault::HeapExhausted));
            assert_eq!(exit.byte_state.unwrap().prefix[..8], [0; 8]);
            assert_eq!(worker.process_count(), 1);
        }
        assert_eq!(tenant.used().messages, 0);
        assert_eq!(tenant.used().requests, 0);
        drop((worker, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

fn scheduled(module: &ScalarModule) {
    for workers in [1, 2] {
        for (length, value, timeout) in [
            (0, 0, 60000),
            (1, 255, 60000),
            (65536, 207, 60000),
            (65536, 33, 0),
        ] {
            let deadline = Instant::now() + Duration::from_secs(3);
            let vm = ResourceAccount::vm(limits());
            let tenant = vm.tenant_accounted(limits()).unwrap();
            let scheduler = Scheduler::new(
                &tenant,
                SchedulerConfig {
                    workers,
                    capacity: 2,
                    requests: 8,
                    slice_steps: 1,
                    ..SchedulerConfig::default()
                },
            )
            .unwrap();
            let handle = scheduler.handle();
            let parent = handle.spawn_with_heap(module, limits(), 2, 10).unwrap();
            let mut pending = handle
                .request(
                    parent,
                    &module.handler("Ask").unwrap(),
                    &[length, value, timeout],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            assert_eq!(
                pending.wait(remaining(deadline)).unwrap(),
                Some(RequestOutcome::Reply(score(length, value)))
            );
            drop(pending);
            let expected = if timeout == 0 {
                1000000001
            } else {
                score(length, value) + 1
            };
            loop {
                let mut result = handle
                    .request(
                        parent,
                        &module.handler("Result").unwrap(),
                        &[],
                        4096,
                        remaining(deadline),
                    )
                    .unwrap();
                if result.wait(remaining(deadline)).unwrap()
                    == Some(RequestOutcome::Reply(expected))
                {
                    break;
                }
                std::thread::yield_now();
            }
            drop((handle, scheduler, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

fn cancellation(module: &ScalarModule) {
    let mut source_copy = false;
    let mut target_import = false;
    for boundary in 0..128 {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker.spawn_with_heap(module, limits(), 2, 10).unwrap();
        let id = worker
            .request(
                parent,
                &module.handler("Start").unwrap(),
                &[65536, 17, 0],
                32768,
                Duration::from_secs(60),
            )
            .unwrap();
        worker.tick(1).unwrap();
        let initial = worker.snapshot(parent).unwrap();
        let control_bytes = initial.resources.bytes - initial.heap.unwrap().charged_bytes;
        for _ in 0..boundary * 8 {
            worker.tick(1).unwrap();
        }
        let pids: Vec<_> = worker.pids().collect();
        if let Some(child) = pids.iter().copied().find(|pid| *pid != parent) {
            let parent_snapshot = worker.snapshot(parent).unwrap();
            source_copy |= parent_snapshot.execution.is_some()
                && parent_snapshot.heap.unwrap().allocated >= 2
                && parent_snapshot.resources.bytes - parent_snapshot.heap.unwrap().charged_bytes
                    >= control_bytes + 65536
                && worker.queued(child).unwrap() == 0;
            let child_snapshot = worker.snapshot(child).unwrap();
            target_import |= child_snapshot.execution.is_some()
                && child_snapshot.execution.unwrap().heap.unwrap().allocated == 1
                && child_snapshot.resources.bytes - child_snapshot.heap.unwrap().charged_bytes
                    >= control_bytes + 131072;
        }
        if boundary % 2 == 0 {
            for pid in pids {
                worker.stop(pid).unwrap();
            }
        }
        let _ = worker.take_reply(id).unwrap();
        drop((worker, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
    assert!(
        source_copy && target_import,
        "boundary sweep must reach source and target live work"
    );
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    single(&module);
    scheduled(&module);
    cancellation(&module);
    println!(
        "Byte messages verified: exact 64 KiB aggregate, independent copies, mixed roots, timeout retention, target failure, single/multi worker and bounded cancellation cleanup; not complete MVP"
    );
}
