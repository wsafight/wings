use std::path::Path;
use std::time::{Duration, Instant};

use wings_abi::ValueType;
use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionPhase, ExecutionStatus, IntListReply, MessageValue,
    PreemptionToken, RequestOutcome, ResourceAccount, Resources, RestartPolicy, ScalarModule,
    ScalarSupervisor, ScalarTask, ScalarWorker, Scheduler, SchedulerConfig, SchedulerExitReason,
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
    assert!(!left.is_zero(), "list reply stage deadline");
    left
}

fn expected(seed: i64, length: usize) -> Vec<i64> {
    (0..length).map(|index| seed + index as i64).collect()
}

fn reply(outcome: Option<RequestOutcome>) -> IntListReply {
    match outcome {
        Some(RequestOutcome::ReplyList(values)) => values,
        other => panic!("expected owned list reply: {other:?}"),
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
    let shared = StackPool::new_accounted(tenant, 1, 0).unwrap();
    let pool = shared.for_process(&account).unwrap();
    let mut process = module.instantiate_with_heap(&pool, &account, 140).unwrap();
    for (seed, length) in [(i64::MIN, 1), (i64::MAX, 1), (17, 0), (17, 1), (17, 64)] {
        assert!(
            process
                .invoke("make", &[seed, length])
                .unwrap_err()
                .contains("invoke_list")
        );
        let result = process
            .invoke_list(
                "make",
                &[seed, length],
                32768,
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result.as_slice(), expected(seed, length as usize));
        process.invoke("clear", &[]).unwrap();
        assert_eq!(result.as_slice(), expected(seed, length as usize));
    }
    process
        .invoke_list("make", &[17, 64], 32768, &CancellationToken::default())
        .unwrap();
    let candidate = process
        .invoke_list("candidate", &[], 32768, &CancellationToken::default())
        .unwrap();
    assert_eq!(candidate.as_slice(), expected(17, 64));
    assert_eq!(process.invoke("marker", &[]).unwrap(), Some(77));
    let result = process
        .invoke_list("make", &[0, 65], 32768, &CancellationToken::default())
        .unwrap_err();
    assert!(result.contains("exceeds 64"));
    drop(process);
    let process = module.instantiate_with_heap(&pool, &account, 140).unwrap();
    let mut task = ScalarTask::new(process);
    task.start(
        &module.handler("make").unwrap(),
        &[17, 64],
        32768,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        &pool,
    )
    .unwrap();
    let mut suspended = 0;
    let retained = loop {
        remaining(deadline);
        match task.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspended += 1,
            ExecutionStatus::CompleteList(values) => break values,
            other => panic!("expected task list reply: {other:?}"),
        }
    };
    assert!(suspended >= 65);
    assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Complete);
    assert_eq!(retained.as_slice(), expected(17, 64));
    drop((candidate, task, pool, shared, account));
    assert_eq!(retained.as_slice(), expected(17, 64));
    drop(retained);
    remaining(deadline);
}

fn worker_host(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
    worker.enable_wait().unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 3, 140).unwrap();
    for length in [0, 1, 64] {
        let request = worker
            .request(
                owner,
                &module.handler("make").unwrap(),
                &[17, length],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        let list = reply(worker.take_reply(request).unwrap());
        assert_eq!(list.as_slice(), expected(17, length as usize));
    }
    let input = [i64::MIN, i64::MAX, 0];
    let request = worker
        .request_values(
            owner,
            &module.handler("echo").unwrap(),
            &[MessageValue::IntList(&input)],
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(reply(worker.take_reply(request).unwrap()).as_slice(), input);
    let request = worker
        .request(
            owner,
            &module.handler("make").unwrap(),
            &[17, 64],
            32768,
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    assert_eq!(worker.resources(owner).unwrap().requests, 1);
    drain(&mut worker, deadline);
    let request = worker
        .request(
            owner,
            &module.handler("current").unwrap(),
            &[],
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    let retained = reply(worker.take_reply(request).unwrap());
    assert_eq!(retained.as_slice(), expected(17, 64));
    let request = worker
        .request(
            owner,
            &module.handler("make").unwrap(),
            &[91, 1],
            32768,
            remaining(deadline),
        )
        .unwrap();
    worker.cancel_request(request).unwrap();
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Cancelled)
    );
    drain(&mut worker, deadline);
    let request = worker
        .request(
            owner,
            &module.handler("current").unwrap(),
            &[],
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(reply(worker.take_reply(request).unwrap()).as_slice(), &[91]);
    let request = worker
        .request(
            owner,
            &module.handler("fail").unwrap(),
            &[],
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(ExecutionFault::DivisionByZero)
    );
    let owner = worker.spawn_with_heap(module, limits(), 2, 140).unwrap();
    let sender = worker
        .open_request_ingress(owner, &module.handler("make").unwrap(), 2, 32768)
        .unwrap();
    let (mut request, _) = sender.request(&[37, 64], remaining(deadline)).unwrap();
    assert_eq!(request.reply_type(), ValueType::ListRef);
    drain(&mut worker, deadline);
    drop(worker);
    let list = reply(request.try_take().unwrap());
    assert_eq!(list.as_slice(), expected(37, 64));
    assert_eq!(retained.as_slice(), expected(17, 64));
    assert!(request.try_take().is_err());
    remaining(deadline);
}

fn supervised(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = ScalarSupervisor::new(tenant, 1, 3).unwrap();
    let child = supervisor
        .spawn_with_heap(module, limits(), 2, 140, RestartPolicy::Never)
        .unwrap();
    let owner = supervisor.current_pid(child).unwrap();
    let request = supervisor
        .request_values(
            owner,
            &module.handler("make").unwrap(),
            &[MessageValue::Int(17), MessageValue::Int(64)],
            32768,
            remaining(deadline),
        )
        .unwrap();
    loop {
        remaining(deadline);
        match supervisor.tick(1).unwrap() {
            SupervisorEvent::Worker(WorkerEvent::Idle) => break,
            SupervisorEvent::Worker(WorkerEvent::Suspended(_) | WorkerEvent::Delivered(_)) => {}
            other => panic!("unexpected list reply supervisor event: {other:?}"),
        }
    }
    let retained = reply(supervisor.take_reply(request).unwrap());
    supervisor.stop(child).unwrap();
    drop(supervisor);
    assert_eq!(retained.as_slice(), expected(17, 64));
    remaining(deadline);
}

fn scheduler_host(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
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
    let owner = handle.spawn_with_heap(module, limits(), 2, 140).unwrap();
    for length in [0, 1, 64] {
        let mut request = handle
            .request(
                owner,
                &module.handler("make").unwrap(),
                &[17, length],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(request.reply_type(), ValueType::ListRef);
        assert_eq!(
            reply(request.wait(remaining(deadline)).unwrap()).as_slice(),
            expected(17, length as usize)
        );
    }
    let input = [i64::MIN, i64::MAX, 0];
    let mut request = handle
        .request_values(
            owner,
            &module.handler("echo").unwrap(),
            &[MessageValue::IntList(&input)],
            32768,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        reply(request.wait(remaining(deadline)).unwrap()).as_slice(),
        input
    );
    let mut request = handle
        .request(
            owner,
            &module.handler("make").unwrap(),
            &[37, 64],
            32768,
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(
        request.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    let mut retained = handle
        .request(
            owner,
            &module.handler("current").unwrap(),
            &[],
            32768,
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
    let list = reply(retained.wait(remaining(deadline)).unwrap());
    drop((request, retained, handle, scheduler));
    assert_eq!(list.as_slice(), expected(37, 64));
    remaining(deadline);
}

fn args(length: usize, mode: usize, same: bool) -> [i64; 5] {
    [
        length as i64,
        if mode == 1 { 0 } else { 60000 },
        i64::from(mode == 2),
        i64::from(mode == 3),
        i64::from(same),
    ]
}

fn check(status: i64, list: &IntListReply, length: usize, mode: usize, same: bool) {
    match mode {
        0 => assert_eq!(status, 0),
        1 => assert_eq!(status, 1),
        2 if same => assert_eq!(status, 2),
        2 => assert!(matches!(status, 0 | 2)),
        3 => assert_eq!(status, 3),
        _ => unreachable!(),
    }
    assert_eq!(
        list.as_slice(),
        if status == 0 {
            expected(17, length)
        } else {
            vec![]
        }
    );
}

fn language_worker(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    length: usize,
    mode: usize,
    same: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 3, 140).unwrap();
    assert!(
        worker
            .send(owner, &module.handler("done").unwrap(), &[1, 0, 0], 128)
            .is_err()
    );
    let request = worker
        .request(
            owner,
            &module.handler("start").unwrap(),
            &args(length, mode, same),
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
        if same {
            assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        }
    } else {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        let status = worker
            .request(
                owner,
                &module.handler("completed_status").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        let list = worker
            .request(
                owner,
                &module.handler("completed_values").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        let Some(RequestOutcome::Reply(status)) = worker.take_reply(status).unwrap() else {
            panic!("missing status");
        };
        check(
            status,
            &reply(worker.take_reply(list).unwrap()),
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
                &module.handler("current").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            reply(worker.take_reply(request).unwrap()).as_slice(),
            expected(17, length)
        );
    }
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
}

fn language_scheduler(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    workers: usize,
    length: usize,
    mode: usize,
    same: bool,
) {
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
    let owner = handle.spawn_with_heap(module, limits(), 3, 140).unwrap();
    let mut request = handle
        .request(
            owner,
            &module.handler("start").unwrap(),
            &args(length, mode, same),
            32768,
            remaining(deadline),
        )
        .unwrap();
    let outcome = request.wait(remaining(deadline)).unwrap();
    if (same && mode == 3) || (mode == 2 && !same && outcome == Some(RequestOutcome::Failed)) {
        loop {
            remaining(deadline);
            if let Some(exit) = handle.last_exit() {
                assert_eq!(exit.pid, owner);
                assert_eq!(
                    exit.reason,
                    SchedulerExitReason::Managed(if same {
                        ExecutionFault::DivisionByZero
                    } else {
                        ExecutionFault::RequestRejected
                    })
                );
                if same {
                    assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
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
            if snapshot.owner.is_none() && snapshot.queued == 0 {
                let mut marker = handle
                    .request(
                        owner,
                        &module.handler("marker").unwrap(),
                        &[],
                        32768,
                        remaining(deadline),
                    )
                    .unwrap();
                if marker.wait(remaining(deadline)).unwrap() == Some(RequestOutcome::Reply(999)) {
                    break;
                }
            }
            std::thread::yield_now();
        }
        let mut status = handle
            .request(
                owner,
                &module.handler("completed_status").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        let Some(RequestOutcome::Reply(status)) = status.wait(remaining(deadline)).unwrap() else {
            panic!("missing status");
        };
        let mut list = handle
            .request(
                owner,
                &module.handler("completed_values").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        check(
            status,
            &reply(list.wait(remaining(deadline)).unwrap()),
            length,
            mode,
            same,
        );
    }
    drop((request, handle, scheduler));
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
    remaining(deadline);
}

fn import_failure(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    if workers == 0 {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 3, 3).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("start_import_fail").unwrap(),
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
        assert_eq!(exit.handler(), "done");
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
        let owner = handle.spawn_with_heap(module, limits(), 3, 3).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("start_import_fail").unwrap(),
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
                assert_eq!(exit.handler(), "done");
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
    remaining(deadline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    direct(&module, &tenant);
    worker_host(&module, &tenant);
    supervised(&module, &tenant);
    for workers in [1, 2] {
        scheduler_host(&module, &tenant, workers);
    }
    for length in [0, 1, 64] {
        for mode in 0..4 {
            for same in [false, true] {
                language_worker(&module, &tenant, length, mode, same);
                for workers in [1, 2] {
                    language_scheduler(&module, &tenant, workers, length, mode, same);
                }
            }
        }
    }
    for workers in [0, 1, 2] {
        import_failure(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Accounted list replies verified: 72 bounded language scenarios, exact copies, exit roots, GC, host/ingress/supervision ownership, failed imports and cleanup; not complete MVP"
    );
}
