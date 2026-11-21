use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionStatus, MessageValue, PreemptionToken, RequestOutcome,
    ResourceAccount, Resources, ScalarModule, ScalarWorker, Scheduler, SchedulerConfig, StackPool,
    WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .expect("string equality stage deadline")
}

fn direct(module: &ScalarModule, tenant: &ResourceAccount) {
    let baseline = tenant.used();
    let deadline = Instant::now() + Duration::from_secs(3);
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut process = module.instantiate_with_heap(&pool, &account, 16).unwrap();
    for length in [0, 1, 255, 256, 257, 65535, 65536] {
        remaining(deadline);
        assert_eq!(process.invoke("fill", &[length]).unwrap(), Some(length));
        assert_eq!(process.invoke("equal", &[length]).unwrap(), Some(1));
        assert_eq!(process.invoke("different", &[length]).unwrap(), Some(0));
        assert_eq!(process.invoke("self_msg", &[]).unwrap(), Some(1));
        assert_eq!(
            process
                .invoke("equal", &[if length == 0 { 1 } else { length - 1 }])
                .unwrap(),
            Some(0)
        );
    }
    for handler in ["unicode", "not_normalized", "last_byte"] {
        assert_eq!(process.invoke(handler, &[]).unwrap(), Some(1));
    }
    assert_eq!(process.invoke("fill", &[65536]).unwrap(), Some(65536));
    let mut execution = process
        .start_controlled(
            "equal",
            &[65536],
            32768,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut suspensions = 0;
    loop {
        remaining(deadline);
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspensions += 1,
            ExecutionStatus::Complete(reply) => {
                assert_eq!(reply, Some(1));
                break;
            }
            other => panic!("unexpected comparison result: {other:?}"),
        }
    }
    assert!(suspensions >= 256);
    drop(execution);
    assert_eq!(process.invoke("read", &[]).unwrap(), Some(65536));
    drop((process, pool, account));
    assert_eq!(tenant.used(), baseline);

    for cancel in [false, true] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let account = tenant.process_accounted(limits()).unwrap();
        let pool = StackPool::new_accounted(tenant, 1, 0)
            .unwrap()
            .for_process(&account)
            .unwrap();
        let mut process = module.instantiate_with_heap(&pool, &account, 8).unwrap();
        process.invoke("fill", &[65536]).unwrap();
        let token = CancellationToken::new_accounted(&account).unwrap();
        let mut execution = process
            .start_controlled(
                "self_msg",
                &[],
                if cancel { 32768 } else { 128 },
                &token,
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..32 {
            remaining(deadline);
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        if cancel {
            token.cancel();
        }
        loop {
            remaining(deadline);
            match execution.resume(1) {
                Ok(ExecutionStatus::Suspended) => {}
                Err(error) => {
                    assert!(
                        error.contains(if cancel { "cancelled" } else { "budget" }),
                        "{error}"
                    );
                    break;
                }
                other => panic!("comparison unexpectedly completed: {other:?}"),
            }
        }
        drop(execution);
        drop((token, process, pool, account));
        assert_eq!(tenant.used(), baseline);
    }
}

fn messages(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let baseline = tenant.used();
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
    for (first, second, expected) in [
        ("中😀\0".into(), "中😀\0".into(), 1),
        ("é".into(), "e\u{301}".into(), 0),
        ("a".repeat(65536), String::new(), 0),
        ("a".repeat(32768), "a".repeat(32768), 1),
    ] {
        let mut request = handle
            .request_values(
                pid,
                &module.handler("compare").unwrap(),
                &[MessageValue::String(&first), MessageValue::String(&second)],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            request.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
    }
    let mut failed = handle
        .request(
            pid,
            &module.handler("failed_candidate").unwrap(),
            &[],
            32768,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        failed.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(
        handle.last_exit().unwrap().execution.unwrap().fault,
        Some(wings_vm::ExecutionFault::DivisionByZero)
    );
    drop(failed);
    drop((handle, scheduler));
    assert_eq!(tenant.used(), baseline);
}

fn single(module: &ScalarModule, tenant: &ResourceAccount) {
    let baseline = tenant.used();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::new(tenant, 2).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 2, 8).unwrap();
    let request = worker
        .request_values(
            pid,
            &module.handler("compare").unwrap(),
            &[MessageValue::String("中😀"), MessageValue::String("中😀")],
            32768,
            remaining(deadline),
        )
        .unwrap();
    while worker.tick(1).unwrap() != WorkerEvent::Idle {
        remaining(deadline);
    }
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Reply(1))
    );
    drop(worker);
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
    single(&module, &tenant);
    for workers in [1, 2] {
        messages(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "String content equality verified: bounded byte comparison, exact UTF-8 contents, GC roots, one/two workers, cancel/budget and cleanup; not MVP acceptance"
    );
}
