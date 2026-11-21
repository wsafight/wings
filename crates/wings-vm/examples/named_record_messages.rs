use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    MessageValue, Pid, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    Scheduler, SchedulerConfig, WorkerEvent,
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
    deadline
        .checked_duration_since(Instant::now())
        .expect("named message stage exceeded three seconds")
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    while worker.tick(1).unwrap() != WorkerEvent::Idle {
        remaining(deadline);
    }
}

fn single(module: &ScalarModule, tenant: &ResourceAccount) {
    let baseline = tenant.used();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::new(tenant, 4).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 4, 32).unwrap();
    for (handler, arguments, expected) in [
        ("start", vec![257], 276),
        ("ask", vec![255], 274),
        ("cross", vec![256], 275),
        ("empty_message", vec![], 276),
        ("start", vec![65535], 65554),
    ] {
        let request = worker
            .request(
                pid,
                &module.handler(handler).unwrap(),
                &arguments,
                131072,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert!(matches!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(_))
        ));
        assert_eq!(worker.committed_state(pid).unwrap(), expected, "{handler}");
    }
    let failed = worker
        .request(
            pid,
            &module.handler("published_then_fail").unwrap(),
            &[9],
            131072,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(failed).unwrap(),
        Some(RequestOutcome::Failed)
    );
    let exit = worker.last_exit().unwrap();
    assert_eq!(exit.committed_state, Some(65554));
    assert_eq!(
        exit.execution.fault,
        Some(wings_vm::ExecutionFault::DivisionByZero)
    );
    let survivors: Vec<Pid> = worker.pids().collect();
    assert_eq!(survivors.len(), 2);
    assert!(
        survivors
            .iter()
            .any(|pid| worker.committed_state(*pid).unwrap() == 28)
    );
    drop(worker);
    assert_eq!(tenant.used(), baseline);

    let mut worker = ScalarWorker::new(tenant, 1).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 2, 32).unwrap();
    let text = "x".repeat(257);
    let request = worker
        .request_values(
            pid,
            &module.handler("query").unwrap(),
            &[
                MessageValue::String(&text),
                MessageValue::Bool(true),
                MessageValue::IntList(&[3]),
                MessageValue::Bytes(&[9]),
                MessageValue::Int(7),
                MessageValue::Unit,
            ],
            131072,
            remaining(deadline),
        )
        .unwrap();
    worker.cancel_request(request).unwrap();
    drop(text);
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Cancelled)
    );
    drain(&mut worker, deadline);
    assert_eq!(worker.committed_state(pid).unwrap(), 276);
    let before = tenant.used();
    let values = [1; 33];
    assert!(
        worker
            .send_values(
                pid,
                &module.handler("pair").unwrap(),
                &[
                    MessageValue::IntList(&values),
                    MessageValue::IntList(&values),
                ],
                131072
            )
            .is_err()
    );
    assert_eq!(tenant.used(), before);
    drop(worker);
    assert_eq!(tenant.used(), baseline);

    for (handler, capacity) in [("too_many_bytes", 32), ("empty_message", 1)] {
        let mut worker = ScalarWorker::new(tenant, 1).unwrap();
        let pid = worker
            .spawn_with_heap(module, limits(), 2, capacity)
            .unwrap();
        if capacity == 1 {
            let request = worker
                .request(
                    pid,
                    &module.handler("empty_message").unwrap(),
                    &[],
                    4096,
                    remaining(deadline),
                )
                .unwrap();
            drain(&mut worker, deadline);
            assert_eq!(
                worker.take_reply(request).unwrap(),
                Some(RequestOutcome::Reply(0))
            );
            assert_eq!(worker.committed_state(pid).unwrap(), 1);
        } else {
            let request = worker
                .request(
                    pid,
                    &module.handler(handler).unwrap(),
                    &[],
                    131072,
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
                Some(wings_vm::ExecutionFault::SendRejected)
            );
            assert_eq!(worker.last_exit().unwrap().committed_state, Some(0));
        }
        drop(worker);
        assert_eq!(tenant.used(), baseline);
    }

    for stop in [false, true] {
        let mut worker = ScalarWorker::new(tenant, 1).unwrap();
        let pid = worker
            .spawn_with_heap(module, limits(), 2, if stop { 32 } else { 1 })
            .unwrap();
        let request = worker
            .request_values(
                pid,
                &module.handler("query").unwrap(),
                &[
                    MessageValue::String("中😀"),
                    MessageValue::Bool(true),
                    MessageValue::IntList(&[3]),
                    MessageValue::Bytes(&[9]),
                    MessageValue::Int(7),
                    MessageValue::Unit,
                ],
                131072,
                remaining(deadline),
            )
            .unwrap();
        if stop {
            assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
            worker.stop(pid).unwrap();
        } else {
            drain(&mut worker, deadline);
            assert_eq!(worker.last_exit().unwrap().committed_state, Some(0));
            assert_eq!(
                worker.last_exit().unwrap().execution.fault,
                Some(wings_vm::ExecutionFault::HeapExhausted)
            );
        }
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
        drop(worker);
        assert_eq!(tenant.used(), baseline);
    }
}

fn host_and_scheduler(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let baseline = tenant.used();
    let deadline = Instant::now() + Duration::from_secs(3);
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 4,
            requests: 4,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let pid = handle.spawn_with_heap(module, limits(), 4, 32).unwrap();
    let mut request = handle
        .request_values(
            pid,
            &module.handler("query").unwrap(),
            &[
                MessageValue::String("中😀\0"),
                MessageValue::Bool(true),
                MessageValue::IntList(&[3]),
                MessageValue::Bytes(&[9]),
                MessageValue::Int(7),
                MessageValue::Unit,
            ],
            131072,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        request.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Reply(22))
    );
    drop(request);
    for (handler, length) in [("ask", 257), ("cross", 258)] {
        let mut request = handle
            .request(
                pid,
                &module.handler(handler).unwrap(),
                &[length],
                131072,
                remaining(deadline),
            )
            .unwrap();
        assert!(matches!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(_))
        ));
        loop {
            let mut read = handle
                .request(
                    pid,
                    &module.handler("read").unwrap(),
                    &[],
                    4096,
                    remaining(deadline),
                )
                .unwrap();
            if read.wait(remaining(deadline)).unwrap() == Some(RequestOutcome::Reply(length + 19)) {
                break;
            }
            std::thread::yield_now();
        }
    }
    handle.stop(pid).unwrap();
    drop((handle, scheduler));
    assert_eq!(tenant.used(), baseline);

    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 1,
            requests: 2,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let pid = handle.spawn_with_heap(module, limits(), 2, 1).unwrap();
    let mut request = handle
        .request_values(
            pid,
            &module.handler("query").unwrap(),
            &[
                MessageValue::String("x"),
                MessageValue::Bool(true),
                MessageValue::IntList(&[3]),
                MessageValue::Bytes(&[9]),
                MessageValue::Int(7),
                MessageValue::Unit,
            ],
            131072,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        request.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(handle.last_exit().unwrap().committed_state, Some(0));
    assert_eq!(
        handle.last_exit().unwrap().execution.unwrap().fault,
        Some(wings_vm::ExecutionFault::HeapExhausted)
    );
    drop((request, handle, scheduler));
    assert_eq!(tenant.used(), baseline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    single(&module, &tenant);
    for workers in [1, 2] {
        host_and_scheduler(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Named record messages verified: bounded typed expansion, independent rebuilding, precise roots, one/two workers, request/failure and cleanup; not MVP acceptance"
    );
}
