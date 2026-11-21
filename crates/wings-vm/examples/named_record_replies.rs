use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, MessageValue, RecordReply, RequestOutcome, ResourceAccount, Resources,
    ScalarModule, ScalarWorker, Scheduler, SchedulerConfig, StackPool, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let value = deadline.saturating_duration_since(Instant::now());
    assert!(!value.is_zero(), "named record reply stage deadline");
    value
}

fn check_reply(reply: &RecordReply, value: i64) {
    assert_eq!(reply.schema().name, "Result");
    assert_eq!(
        reply.values().collect::<Vec<_>>(),
        vec![
            MessageValue::Int(value),
            MessageValue::String("aaa"),
            MessageValue::Bool(true),
            MessageValue::IntList(&[value]),
            MessageValue::Bytes(&[255, 255]),
            MessageValue::Int(value + 1),
            MessageValue::Unit,
        ]
    );
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    loop {
        remaining(deadline);
        if worker.tick(1).unwrap() == WorkerEvent::Idle {
            return;
        }
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
    let mut process = module.instantiate_with_heap(&pool, &account, 16).unwrap();
    let reply = process
        .invoke_record("value", &[7], 8192, &CancellationToken::default())
        .unwrap();
    check_reply(&reply, 7);
    let retained = reply.clone();
    check_reply(&retained, 7);
    drop(reply);
    let empty = process
        .invoke_record("empty_value", &[], 8192, &CancellationToken::default())
        .unwrap();
    assert_eq!(empty.schema().name, "Empty");
    assert_eq!(empty.values().count(), 0);
    drop(empty);

    let boundary = process
        .invoke_record("boundary", &[], 100_000, &CancellationToken::default())
        .unwrap();
    let values = boundary.values().collect::<Vec<_>>();
    assert!(matches!(values[1], MessageValue::String(value) if value.len() == 65535));
    assert!(matches!(values[4], MessageValue::Bytes(value) if value == [255]));
    assert!(
        process
            .invoke_record("too_large", &[], 100_000, &CancellationToken::default())
            .is_err()
    );
    check_reply(&retained, 7);
    drop((retained, boundary, process, pool, account));
    assert_eq!(tenant.used(), baseline);
    remaining(deadline);
}

fn host_worker(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let baseline = tenant.used();
    let mut worker = ScalarWorker::with_request_capacity(tenant, 4, 8).unwrap();
    let owner = worker.spawn_with_heap(module, limits(), 4, 16).unwrap();
    let request = worker
        .request(
            owner,
            &module.handler("value").unwrap(),
            &[17],
            8192,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    let reply = match worker.take_reply(request).unwrap() {
        Some(RequestOutcome::ReplyRecord(reply)) => reply,
        other => panic!("expected named record reply, got {other:?}"),
    };
    check_reply(&reply, 17);
    let failed = worker
        .request(
            owner,
            &module.handler("too_large").unwrap(),
            &[],
            100_000,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(failed).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(worker.last_exit().unwrap().committed_state, Some(1));
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(wings_vm::ExecutionFault::ReplyRejected)
    );
    drop(worker);
    check_reply(&reply, 17);
    drop(reply);
    assert_eq!(tenant.used(), baseline);
}

fn language_worker(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    for (timeout, cancel, fail, expected) in [
        (1000, false, false, 281),
        (0, false, false, -1),
        (1000, true, false, -2),
        (1000, false, true, -3),
    ] {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 4, 8).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 4, 16).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("start").unwrap(),
                &[7, timeout, i64::from(cancel), i64::from(fail)],
                16384,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(10))
        );
        let read = worker
            .request(
                owner,
                &module.handler("read").unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(read).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
    }
}

fn language_scheduler(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    for (timeout, cancel, fail, expected) in [
        (1000, false, false, 281),
        (0, false, false, -1),
        (1000, true, false, -2),
        (1000, false, true, -3),
    ] {
        let scheduler = Scheduler::new(
            tenant,
            SchedulerConfig {
                workers,
                capacity: 4,
                requests: 8,
                slice_steps: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let owner = handle.spawn_with_heap(module, limits(), 4, 16).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("start").unwrap(),
                &[7, timeout, i64::from(cancel), i64::from(fail)],
                16384,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(10))
        );
        loop {
            let mut read = handle
                .request(
                    owner,
                    &module.handler("read").unwrap(),
                    &[],
                    4096,
                    remaining(deadline),
                )
                .unwrap();
            let result = read.wait(remaining(deadline)).unwrap();
            if result == Some(RequestOutcome::Reply(expected)) {
                break;
            }
            assert_eq!(result, Some(RequestOutcome::Reply(10)));
            std::thread::yield_now();
        }
        drop((request, handle, scheduler));
    }
}

fn import_failure(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let baseline = tenant.used();
    if workers == 0 {
        let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 4).unwrap();
        let owner = worker.spawn_with_heap(module, limits(), 2, 3).unwrap();
        let request = worker
            .request(
                owner,
                &module.handler("start").unwrap(),
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
        assert_eq!(exit.handler(), "done");
        assert_eq!(
            exit.execution.fault,
            Some(wings_vm::ExecutionFault::HeapExhausted)
        );
        assert!(exit.committed_state.is_none());
        let target = worker.pids().next().unwrap();
        let read = worker
            .request(
                target,
                &module.handler("read").unwrap(),
                &[],
                8192,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(read).unwrap(),
            Some(RequestOutcome::Reply(12))
        );
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
        let owner = handle.spawn_with_heap(module, limits(), 2, 3).unwrap();
        let mut request = handle
            .request(
                owner,
                &module.handler("start").unwrap(),
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
                assert_eq!(exit.handler(), "done");
                assert_eq!(
                    exit.reason,
                    wings_vm::SchedulerExitReason::Managed(wings_vm::ExecutionFault::HeapExhausted)
                );
                break;
            }
            std::thread::yield_now();
        }
        drop((request, handle, scheduler));
    }
    assert_eq!(tenant.used(), baseline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let state = unsafe { ScalarModule::load_accounted(Path::new(&arguments[2]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    direct(&module, &tenant);
    host_worker(&module, &tenant);
    language_worker(&module, &tenant);
    for workers in [1, 2] {
        language_scheduler(&module, &tenant, workers);
    }
    for workers in [0, 1, 2] {
        import_failure(&state, &tenant, workers);
    }
    drop((module, state, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Named record replies verified: copied nested values, worker and scheduler completions, timeout, cancel, failure and cleanup; not complete MVP"
    );
}
