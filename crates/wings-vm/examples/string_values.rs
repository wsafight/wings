use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, MessageValue, PreemptionToken,
    RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker, Scheduler,
    SchedulerConfig, StackPool, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn direct(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let baseline = tenant.used();
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    assert!(module.instantiate_in_pool(&pool).is_err());
    let mut process = module.instantiate_with_heap(&pool, &account, 16).unwrap();
    assert_eq!(process.invoke("unicode", &[]).unwrap(), Some(207));
    assert_eq!(process.invoke("literal", &[]).unwrap(), Some(813));
    assert_eq!(process.invoke("unicode", &[]).unwrap(), Some(207));
    assert_eq!(process.invoke("copy", &[]).unwrap(), Some(2));
    assert_eq!(process.invoke("choose", &[1]).unwrap(), Some(2));
    assert_eq!(process.invoke("choose", &[0]).unwrap(), Some(0));
    assert_eq!(process.invoke("grow", &[20]).unwrap(), Some(20));
    for length in [0, 1, 255, 256, 257, 32768, 65536] {
        assert_eq!(process.invoke("fill", &[length]).unwrap(), Some(length));
        assert_eq!(process.invoke("copy", &[]).unwrap(), Some(length));
        assert_eq!(process.invoke("byte_length", &[]).unwrap(), Some(length));
    }
    let mut execution = process
        .start_controlled(
            "copy",
            &[],
            32768,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut suspensions = 0;
    loop {
        assert!(Instant::now() < deadline);
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspensions += 1,
            ExecutionStatus::Complete(value) => {
                assert_eq!(value, Some(65536));
                break;
            }
            other => panic!("unexpected string result: {other:?}"),
        }
    }
    assert!(suspensions >= 1024);
    drop(execution);
    process.invoke("clear", &[]).unwrap();
    process.invoke("sweep", &[]).unwrap();
    assert_eq!(process.heap_stats().unwrap().allocated, 1);
    drop((process, pool, account));
    assert_eq!(tenant.used(), baseline);

    for cancel in [false, true] {
        let account = tenant.process_accounted(limits()).unwrap();
        let pool = StackPool::new_accounted(tenant, 1, 0)
            .unwrap()
            .for_process(&account)
            .unwrap();
        let mut process = module.instantiate_with_heap(&pool, &account, 16).unwrap();
        process.invoke("fill", &[65536]).unwrap();
        let cancellation = CancellationToken::default();
        let mut execution = process
            .start_controlled(
                "copy",
                &[],
                32768,
                &cancellation,
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..300 {
            assert!(Instant::now() < deadline);
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        if cancel {
            cancellation.cancel();
            assert!(execution.resume(1).is_err());
            assert_eq!(execution.snapshot().fault, Some(ExecutionFault::Cancelled));
        }
        drop(execution);
        drop((process, pool, account));
        assert_eq!(tenant.used(), baseline);
    }
}

fn scheduler(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let remaining = || deadline.checked_duration_since(Instant::now()).unwrap();
    let baseline = tenant.used();
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 2,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    for failure in ["invalid", "double"] {
        let pid = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
        for (name, arguments, expected) in [
            ("literal", vec![], 813),
            ("unicode", vec![], 207),
            ("copy", vec![], 2),
            ("fill", vec![65536], 65536),
            ("copy", vec![], 65536),
        ] {
            let mut request = handle
                .request(
                    pid,
                    &module.handler(name).unwrap(),
                    &arguments,
                    32768,
                    remaining(),
                )
                .unwrap();
            assert_eq!(
                request.wait(remaining()).unwrap(),
                Some(RequestOutcome::Reply(expected))
            );
        }
        let mut request = handle
            .request(
                pid,
                &module.handler(failure).unwrap(),
                &[],
                32768,
                remaining(),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining()).unwrap(),
            Some(RequestOutcome::Failed)
        );
        let after = handle.last_exit().unwrap();
        assert_eq!(after.state_type, wings_abi::ValueType::StringRef);
        assert_eq!(
            after.execution.unwrap().fault,
            Some(ExecutionFault::InvalidString)
        );
        assert!(handle.snapshot(pid).is_err());
    }
    drop((handle, scheduler));
    assert_eq!(tenant.used(), baseline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    direct(&module, &tenant);
    message_single(&module, &tenant);
    for workers in [1, 2] {
        scheduler(&module, &tenant, workers);
        message_scheduler(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Accounted UTF-8 strings: local/state/functions, one/two workers, bounded GC/cancel/failure and cleanup; not complete MVP"
    );
    println!(
        "Accounted String messages: shared 64 KiB limit, independent typed copies, one/two workers, timeout/cancel/GC/stop and cleanup; not complete MVP"
    );
}

fn remaining(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "String message stage deadline");
    remaining
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    while worker.tick(1).unwrap() != WorkerEvent::Idle {
        remaining(deadline);
    }
}

fn message_single(module: &ScalarModule, tenant: &ResourceAccount) {
    let baseline = tenant.used();
    for text in [
        String::new(),
        "中😀\0".into(),
        "a".repeat(255) + "中😀",
        "a".repeat(65536),
        "😀".repeat(16384),
    ] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 2).unwrap();
        let pid = worker.spawn_with_heap(module, limits(), 2, 16).unwrap();
        let mut source = text.clone();
        let request = worker
            .request_values(
                pid,
                &module.handler("echo").unwrap(),
                &[MessageValue::String(&source)],
                32768,
                remaining(deadline),
            )
            .unwrap();
        source.clear();
        drain(&mut worker, deadline);
        let retained = match worker.take_reply(request).unwrap() {
            Some(RequestOutcome::ReplyBytes(value)) => value,
            other => panic!("expected copied text: {other:?}"),
        };
        assert_eq!(retained.as_slice(), text.as_bytes());
        for terminal in [0, 1] {
            let request = worker
                .request_values(
                    pid,
                    &module.handler("count_text").unwrap(),
                    &[MessageValue::String(&text)],
                    32768,
                    if terminal == 0 {
                        Duration::ZERO
                    } else {
                        remaining(deadline)
                    },
                )
                .unwrap();
            if terminal == 1 {
                worker.cancel_request(request).unwrap();
            }
            assert_eq!(
                worker.take_reply(request).unwrap(),
                Some(if terminal == 0 {
                    RequestOutcome::TimedOut
                } else {
                    RequestOutcome::Cancelled
                })
            );
            drain(&mut worker, deadline);
            let read = worker
                .request(
                    pid,
                    &module.handler("read").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            drain(&mut worker, deadline);
            assert_eq!(
                worker.take_reply(read).unwrap(),
                Some(RequestOutcome::Reply(text.chars().count() as i64))
            );
        }
        worker.stop(pid).unwrap();
        drop(worker);
        assert_eq!(retained.as_slice(), text.as_bytes());
        drop(retained);
        assert_eq!(tenant.used(), baseline);
    }
    for length in [0, 256, 65536] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 2).unwrap();
        let parent = worker.spawn_with_heap(module, limits(), 2, 16).unwrap();
        let request = worker
            .request(
                parent,
                &module.handler("send_text").unwrap(),
                &[length],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(length))
        );
        let child = worker.pids().find(|pid| *pid != parent).unwrap();
        let read = worker
            .request(
                child,
                &module.handler("read").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(read).unwrap(),
            Some(RequestOutcome::Reply(length))
        );
        drop(worker);
        assert_eq!(tenant.used(), baseline);
    }
    for failure in ["over_text", "send_then_fail"] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut worker = ScalarWorker::new(tenant, 2).unwrap();
        let pid = worker.spawn_with_heap(module, limits(), 2, 16).unwrap();
        worker
            .send(pid, &module.handler(failure).unwrap(), &[], 32768)
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(if failure == "over_text" {
                ExecutionFault::SendRejected
            } else {
                ExecutionFault::DivisionByZero
            })
        );
        if failure == "send_then_fail" {
            let child = worker.pids().next().unwrap();
            let read = worker
                .request(
                    child,
                    &module.handler("read").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            drain(&mut worker, deadline);
            assert_eq!(
                worker.take_reply(read).unwrap(),
                Some(RequestOutcome::Reply(9))
            );
        }
        drop(worker);
        assert_eq!(tenant.used(), baseline);
    }
}

fn message_scheduler(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let baseline = tenant.used();
    for text in [
        String::new(),
        "中😀\0".into(),
        "a".repeat(255) + "中😀",
        "😀".repeat(16384),
    ] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let scheduler = Scheduler::new(
            tenant,
            SchedulerConfig {
                workers,
                capacity: 2,
                requests: 2,
                slice_steps: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let pid = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
        let data = vec![255; 65536 - text.len()];
        let mut request = handle
            .request_values(
                pid,
                &module.handler("mixed").unwrap(),
                &[
                    MessageValue::String(&text),
                    MessageValue::Bytes(&data),
                    MessageValue::Int(7),
                    MessageValue::IntList(&[3]),
                    MessageValue::String(""),
                ],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(
                text.chars().count() as i64 * 1000000
                    + text.len() as i64 * 1000
                    + data.len() as i64
                    + 10
            ))
        );
        drop(request);
        assert!(
            handle
                .send_values(
                    pid,
                    &module.handler("store").unwrap(),
                    &[MessageValue::Bytes(text.as_bytes())],
                    32768
                )
                .is_err()
        );
        assert!(
            handle
                .send(pid, &module.handler("store").unwrap(), &[0], 32768)
                .is_err()
        );
        let mut request = handle
            .request_values(
                pid,
                &module.handler("echo").unwrap(),
                &[MessageValue::String(&text)],
                32768,
                remaining(deadline),
            )
            .unwrap();
        let retained = match request.wait(remaining(deadline)).unwrap() {
            Some(RequestOutcome::ReplyBytes(value)) => value,
            other => panic!("expected String bytes: {other:?}"),
        };
        drop(request);
        let mut source = text.clone();
        handle
            .send_values(
                pid,
                &module.handler("self_text").unwrap(),
                &[MessageValue::String(&source)],
                32768,
            )
            .unwrap();
        source.clear();
        let mut read = handle
            .request(
                pid,
                &module.handler("read").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            read.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(text.chars().count() as i64))
        );
        drop(read);
        handle.stop(pid).unwrap();
        drop((handle, scheduler));
        assert_eq!(retained.as_slice(), text.as_bytes());
        drop(retained);
        assert_eq!(tenant.used(), baseline);
    }
    for timeout in [0, 1000] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let scheduler = Scheduler::new(
            tenant,
            SchedulerConfig {
                workers,
                capacity: 2,
                requests: 2,
                slice_steps: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let pid = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
        let mut request = handle
            .request_values(
                pid,
                &module.handler("ask_text").unwrap(),
                &[MessageValue::String("中😀"), MessageValue::Int(timeout)],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(2))
        );
        drop(request);
        loop {
            let mut read = handle
                .request(
                    pid,
                    &module.handler("read").unwrap(),
                    &[],
                    32768,
                    remaining(deadline),
                )
                .unwrap();
            let outcome = read.wait(remaining(deadline)).unwrap();
            if outcome == Some(RequestOutcome::Reply(if timeout == 0 { 0 } else { 2 })) {
                break;
            }
            drop(read);
            std::thread::sleep(Duration::from_millis(1));
        }
        drop((handle, scheduler));
        assert_eq!(tenant.used(), baseline);
    }
}
