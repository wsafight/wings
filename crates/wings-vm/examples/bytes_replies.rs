use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    BytesReply, CancellationToken, ExecutionFault, ExecutionStatus, MessageValue, PreemptionToken,
    RequestOutcome, ResourceAccount, Resources, RestartPolicy, ScalarModule, ScalarSupervisor,
    ScalarWorker, Scheduler, SchedulerConfig, SchedulerExitReason, StackPool, SupervisorEvent,
    WorkerEvent,
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
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "byte reply stage deadline");
    remaining
}

fn reply(outcome: Option<RequestOutcome>) -> BytesReply {
    match outcome {
        Some(RequestOutcome::ReplyBytes(value)) => value,
        other => panic!("expected byte reply: {other:?}"),
    }
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    loop {
        remaining(deadline);
        if worker.tick(1).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
}

fn direct(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut process = module.instantiate_with_heap(&pool, &account, 8).unwrap();
    for length in [0, 1, 255, 256, 257, 65536] {
        assert!(
            process
                .invoke("Make", &[length])
                .unwrap_err()
                .contains("invoke_bytes")
        );
        let retained = process
            .invoke_bytes("Make", &[length], 32768, &CancellationToken::default())
            .unwrap();
        assert_eq!(retained.as_slice(), vec![255; length as usize]);
        let old = process
            .invoke_bytes("Candidate", &[], 32768, &CancellationToken::default())
            .unwrap();
        assert_eq!(old, retained);
        assert_eq!(process.invoke("Marker", &[]).unwrap(), Some(77));
        process.invoke("Clear", &[]).unwrap();
        assert_eq!(retained.as_slice(), vec![255; length as usize]);
    }
    let mut execution = process
        .start_controlled(
            "Make",
            &[65536],
            32768,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut suspensions = 0;
    let retained = loop {
        remaining(deadline);
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspensions += 1,
            ExecutionStatus::CompleteBytes(value) => break value,
            other => panic!("unexpected result: {other:?}"),
        }
    };
    assert!(suspensions >= 514);
    drop(execution);
    drop((process, pool, account));
    assert_eq!(retained.as_slice(), vec![255; 65536]);
    remaining(deadline);
}

fn worker(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
    worker.enable_wait().unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 3, 8).unwrap();
    for length in [0, 1, 65536] {
        let request = worker
            .request(
                owner,
                &module.handler("Make").unwrap(),
                &[length],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            reply(worker.take_reply(request).unwrap()).as_slice(),
            vec![255; length as usize]
        );
    }
    let input = [0, 255, 17, 99];
    let request = worker
        .request_values(
            owner,
            &module.handler("Echo").unwrap(),
            &[MessageValue::Bytes(&input)],
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(reply(worker.take_reply(request).unwrap()).as_slice(), input);
    let sender = worker
        .open_request_ingress(owner, &module.handler("Make").unwrap(), 1, 32768)
        .unwrap();
    let (mut request, _) = sender.request(&[65536], remaining(deadline)).unwrap();
    drain(&mut worker, deadline);
    worker.stop(owner).unwrap();
    let retained = reply(request.try_take().unwrap());
    drop((worker, sender, request));
    assert_eq!(retained.as_slice(), vec![255; 65536]);
    remaining(deadline);
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
    let owner = handle.spawn_with_heap(module, limits(), 3, 8).unwrap();
    for length in [0, 1, 65536] {
        let mut request = handle
            .request(
                owner,
                &module.handler("Make").unwrap(),
                &[length],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            reply(request.wait(remaining(deadline)).unwrap()).as_slice(),
            vec![255; length as usize]
        );
    }
    let input = [0, 255, 17, 99];
    let mut request = handle
        .request_values(
            owner,
            &module.handler("Echo").unwrap(),
            &[MessageValue::Bytes(&input)],
            32768,
            remaining(deadline),
        )
        .unwrap();
    let retained = reply(request.wait(remaining(deadline)).unwrap());
    handle.stop(owner).unwrap();
    drop((handle, scheduler, request));
    assert_eq!(retained.as_slice(), input);
    remaining(deadline);
}

fn supervised(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = ScalarSupervisor::new(tenant, 1, 2).unwrap();
    let child = supervisor
        .spawn_with_heap(module, limits(), 1, 8, RestartPolicy::Never)
        .unwrap();
    let owner = supervisor.current_pid(child).unwrap();
    let request = supervisor
        .request(
            owner,
            &module.handler("Make").unwrap(),
            &[65536],
            32768,
            remaining(deadline),
        )
        .unwrap();
    loop {
        remaining(deadline);
        if supervisor.tick(1).unwrap() == SupervisorEvent::Worker(WorkerEvent::Idle) {
            break;
        }
    }
    let retained = reply(supervisor.take_reply(request).unwrap());
    supervisor.stop(child).unwrap();
    drop(supervisor);
    assert_eq!(retained.as_slice(), vec![255; 65536]);
}

fn language(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    workers: usize,
    length: i64,
    mode: usize,
    same: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let arguments = [
        length,
        if mode == 1 { 0 } else { 60000 },
        i64::from(mode == 2),
        i64::from(mode == 3),
        i64::from(same),
    ];
    if workers == 0 {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 3, 8).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("Start").unwrap(),
                &arguments,
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        let outcome = worker.take_reply(request).unwrap();
        if (same && mode == 3) || (mode == 2 && !same && outcome == Some(RequestOutcome::Failed)) {
            let exit = worker.last_exit().unwrap();
            assert_eq!(exit.pid, owner);
            assert_eq!(
                exit.execution.fault,
                Some(if same {
                    ExecutionFault::DivisionByZero
                } else {
                    ExecutionFault::RequestRejected
                })
            );
            assert!(worker.snapshot(owner).is_err());
        } else {
            assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
            let status = worker
                .request(
                    owner,
                    &module.handler("CompletedStatus").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let size = worker
                .request(
                    owner,
                    &module.handler("CompletedLength").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let edges = worker
                .request(
                    owner,
                    &module.handler("CompletedEdges").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            drain(&mut worker, deadline);
            check(
                worker.take_reply(status).unwrap(),
                worker.take_reply(size).unwrap(),
                worker.take_reply(edges).unwrap(),
                length,
                mode,
                same,
            );
        }
        if !same && mode != 3 {
            let child = worker.pids().find(|pid| *pid != owner).unwrap();
            let request = worker
                .request(
                    child,
                    &module.handler("Current").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            drain(&mut worker, deadline);
            assert_eq!(
                reply(worker.take_reply(request).unwrap()).as_slice(),
                vec![255; length as usize]
            );
        }
    } else {
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
        let owner = handle.spawn_with_heap(module, limits(), 3, 8).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("Start").unwrap(),
                &arguments,
                32768,
                remaining(deadline),
            )
            .unwrap();
        let outcome = request.wait(remaining(deadline)).unwrap();
        let owner_fails =
            (same && mode == 3) || (mode == 2 && !same && outcome == Some(RequestOutcome::Failed));
        if !owner_fails {
            assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        }
        loop {
            remaining(deadline);
            if owner_fails {
                if handle.snapshot(owner).is_err() {
                    let exit = handle.last_exit().unwrap();
                    assert_eq!(exit.pid, owner);
                    assert_eq!(
                        exit.reason,
                        SchedulerExitReason::Managed(if same {
                            ExecutionFault::DivisionByZero
                        } else {
                            ExecutionFault::RequestRejected
                        })
                    );
                    break;
                }
                std::thread::yield_now();
                continue;
            }
            let mut marker = handle
                .request(
                    owner,
                    &module.handler("Completed").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let Some(RequestOutcome::Reply(value)) = marker.wait(remaining(deadline)).unwrap()
            else {
                panic!("missing marker");
            };
            if value == 9 {
                continue;
            }
            let mut status = handle
                .request(
                    owner,
                    &module.handler("CompletedStatus").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let mut size = handle
                .request(
                    owner,
                    &module.handler("CompletedLength").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let mut edges = handle
                .request(
                    owner,
                    &module.handler("CompletedEdges").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            check(
                status.wait(remaining(deadline)).unwrap(),
                size.wait(remaining(deadline)).unwrap(),
                edges.wait(remaining(deadline)).unwrap(),
                length,
                mode,
                same,
            );
            break;
        }
    }
    remaining(deadline);
}

fn import_failure(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    if workers == 0 {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 2, 1).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("StartImportFail").unwrap(),
                &[],
                32768,
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
        assert_eq!(exit.execution.fault, Some(ExecutionFault::HeapExhausted));
    } else {
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
        let owner = handle.spawn_with_heap(module, limits(), 2, 1).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("StartImportFail").unwrap(),
                &[],
                32768,
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
                assert_eq!(
                    exit.reason,
                    SchedulerExitReason::Managed(ExecutionFault::HeapExhausted)
                );
                break;
            }
            std::thread::yield_now();
        }
    }
}

fn check(
    status: Option<RequestOutcome>,
    size: Option<RequestOutcome>,
    edges: Option<RequestOutcome>,
    length: i64,
    mode: usize,
    same: bool,
) {
    let Some(RequestOutcome::Reply(status)) = status else {
        panic!("missing status");
    };
    match mode {
        0 => assert_eq!(status, 0),
        1 => assert_eq!(status, 1),
        2 if same => assert_eq!(status, 2),
        2 => assert!(matches!(status, 0 | 2)),
        3 => assert_eq!(status, 3),
        _ => unreachable!(),
    }
    assert_eq!(
        size,
        Some(RequestOutcome::Reply(if status == 0 { length } else { 0 }))
    );
    assert_eq!(
        edges,
        Some(RequestOutcome::Reply(if status == 0 && length != 0 {
            510
        } else {
            0
        }))
    );
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    direct(&module, &tenant);
    worker(&module, &tenant);
    supervised(&module, &tenant);
    for workers in [1, 2] {
        scheduler(&module, &tenant, workers);
    }
    for length in [0, 1, 65536] {
        for mode in 0..4 {
            for same in [false, true] {
                for workers in 0..=2 {
                    language(&module, &tenant, workers, length, mode, same);
                }
            }
        }
    }
    for workers in 0..=2 {
        import_failure(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Accounted byte replies: 72 language scenarios, bounded export, retained results and cleanup; not complete MVP"
    );
}
