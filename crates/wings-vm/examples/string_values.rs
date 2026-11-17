use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, PreemptionToken, RequestOutcome,
    ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig, StackPool,
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
    assert_eq!(process.invoke("Unicode", &[]).unwrap(), Some(207));
    assert_eq!(process.invoke("Literal", &[]).unwrap(), Some(813));
    assert_eq!(process.invoke("Unicode", &[]).unwrap(), Some(207));
    assert_eq!(process.invoke("Copy", &[]).unwrap(), Some(2));
    assert_eq!(process.invoke("Choose", &[1]).unwrap(), Some(2));
    assert_eq!(process.invoke("Choose", &[0]).unwrap(), Some(0));
    assert_eq!(process.invoke("Grow", &[20]).unwrap(), Some(20));
    for length in [0, 1, 255, 256, 257, 32768, 65536] {
        assert_eq!(process.invoke("Fill", &[length]).unwrap(), Some(length));
        assert_eq!(process.invoke("Copy", &[]).unwrap(), Some(length));
        assert_eq!(process.invoke("ByteLength", &[]).unwrap(), Some(length));
    }
    let mut execution = process
        .start_controlled(
            "Copy",
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
    process.invoke("Clear", &[]).unwrap();
    process.invoke("Sweep", &[]).unwrap();
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
        process.invoke("Fill", &[65536]).unwrap();
        let cancellation = CancellationToken::default();
        let mut execution = process
            .start_controlled(
                "Copy",
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
    for failure in ["Invalid", "Double"] {
        let pid = handle.spawn_with_heap(module, limits(), 2, 16).unwrap();
        for (name, arguments, expected) in [
            ("Literal", vec![], 813),
            ("Unicode", vec![], 207),
            ("Copy", vec![], 2),
            ("Fill", vec![65536], 65536),
            ("Copy", vec![], 65536),
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
    for workers in [1, 2] {
        scheduler(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Accounted UTF-8 strings: local/state/functions, one/two workers, bounded GC/cancel/failure and cleanup; not complete MVP"
    );
}
