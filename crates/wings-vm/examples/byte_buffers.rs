use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, PreemptionToken, RequestOutcome,
    ResourceAccount, Resources, RestartPolicy, ScalarModule, SchedulerConfig,
    SchedulerRestartStatus, SchedulerSupervisor, StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 16,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
    for length in [0, 256, 4096, 65536] {
        assert_eq!(
            process.invoke("fill", &[length, 255]).unwrap(),
            Some(length)
        );
        assert_eq!(process.invoke("length", &[]).unwrap(), Some(length));
        if length != 0 {
            assert_eq!(
                process.invoke("set", &[length - 1, 17]).unwrap(),
                Some(255 * 256 + 17)
            );
            assert_eq!(process.invoke("read", &[length - 1]).unwrap(), Some(17));
            assert_eq!(process.invoke("branch", &[1]).unwrap(), Some(262));
            assert_eq!(process.invoke("branch", &[0]).unwrap(), Some(510));
        }
    }
    assert_eq!(process.invoke("loop", &[20]).unwrap(), Some(20));
    process.invoke("fill", &[65536, 0]).unwrap();
    let mut previous = 0;
    for number in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(
            process.invoke("set_word", &[65528, number]).unwrap(),
            Some(previous)
        );
        assert_eq!(process.invoke("read_word", &[65528]).unwrap(), Some(number));
        assert_eq!(process.invoke("read", &[0]).unwrap(), Some(0));
        previous = number;
    }
    let mut execution = process
        .start_controlled(
            "set_word",
            &[65528, -7],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut suspensions = 0;
    loop {
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspensions += 1,
            ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
            ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
            ExecutionStatus::CompleteString(value) => panic!("unexpected string reply: {value:?}"),
            ExecutionStatus::CompleteRecord(value) => panic!("unexpected record reply: {value:?}"),
            ExecutionStatus::Complete(reply) => {
                assert_eq!(reply, Some(i64::MAX));
                break;
            }
        }
    }
    assert!(suspensions >= 256);
    drop(execution);
    assert_eq!(process.invoke("read_word", &[65528]).unwrap(), Some(-7));
    drop(process);
    assert_eq!(vm.used(), baseline);
    let mut observed_unpublished_bytes = false;
    for boundary in 0..256 {
        for cancel in [false, true] {
            let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
            let cancellation = CancellationToken::default();
            let mut execution = process
                .start_controlled(
                    "fill",
                    &[65536, 7],
                    4096,
                    &cancellation,
                    &PreemptionToken::default(),
                )
                .unwrap();
            for _ in 0..boundary {
                assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
            }
            observed_unpublished_bytes |=
                account.used().bytes > 65536 && execution.snapshot().heap.unwrap().allocated == 1;
            if cancel {
                cancellation.cancel();
                assert!(execution.resume(1).is_err());
                assert_eq!(execution.snapshot().fault, Some(ExecutionFault::Cancelled));
            }
            drop(execution);
            drop(process);
            assert_eq!(vm.used(), baseline);
        }
    }
    assert!(observed_unpublished_bytes);
    for (handler, arguments) in [
        ("fill", vec![-1, 0]),
        ("fill", vec![65537, 0]),
        ("fill", vec![1, 256]),
        ("set", vec![-1, 3]),
        ("set", vec![4, 3]),
        ("set", vec![0, -1]),
        ("read", vec![4]),
        ("read_word", vec![0]),
        ("set_word", vec![0, i64::MAX]),
        ("set_word", vec![i64::MAX, i64::MIN]),
    ] {
        let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
        process.invoke("fill", &[4, 8]).unwrap();
        let mut execution = process
            .start_controlled(
                handler,
                &arguments,
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(execution.resume(4096).is_err());
        assert_eq!(
            execution.snapshot().fault,
            Some(ExecutionFault::InvalidBytes)
        );
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    for budget in [0, 4, 64, 128, 255] {
        let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
        process.invoke("fill", &[65536, 9]).unwrap();
        let mut execution = process
            .start_controlled(
                "set",
                &[128, 7],
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(execution.resume(4096).is_err());
        assert_eq!(
            execution.snapshot().fault,
            Some(ExecutionFault::BudgetExhausted)
        );
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    for budget in [0, 4, 64, 128, 255] {
        let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
        process.invoke("fill", &[65536, 0]).unwrap();
        let mut execution = process
            .start_controlled(
                "set_word",
                &[65528, i64::MIN],
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(execution.resume(4096).is_err());
        assert_eq!(
            execution.snapshot().fault,
            Some(ExecutionFault::BudgetExhausted)
        );
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool, account));
    let mut supervisor = SchedulerSupervisor::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
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
            4,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let timeout = Duration::from_secs(5);
    let mut fill = handle
        .request(
            pid,
            &module.handler("fill").unwrap(),
            &[65536, 9],
            4096,
            timeout,
        )
        .unwrap();
    assert_eq!(
        fill.wait(timeout).unwrap(),
        Some(RequestOutcome::Reply(65536))
    );
    for _ in 0..4 {
        let mut read = handle
            .request(
                pid,
                &module.handler("read").unwrap(),
                &[65535],
                4096,
                timeout,
            )
            .unwrap();
        assert_eq!(read.wait(timeout).unwrap(), Some(RequestOutcome::Reply(9)));
    }
    assert_eq!(handle.snapshot(pid).unwrap().migrations, 4);
    let mut failed = handle
        .request(pid, &module.handler("fail").unwrap(), &[], 4096, timeout)
        .unwrap();
    assert_eq!(failed.wait(timeout).unwrap(), Some(RequestOutcome::Failed));
    let deadline = Instant::now() + timeout;
    let event = loop {
        if let Some(event) = supervisor.poll() {
            break event;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    };
    let SchedulerRestartStatus::Restarted(replacement) = event.restart else {
        panic!("missing replacement")
    };
    let mut length = handle
        .request(
            replacement,
            &module.handler("length").unwrap(),
            &[],
            4096,
            timeout,
        )
        .unwrap();
    assert_eq!(
        length.wait(timeout).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    drop((length, fill, failed, handle, supervisor, module, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Byte buffers verified: accounted immutable 0..64 KiB values, chunked copying, roots/GC, cancellation, migration and fresh supervised heaps"
    );
}
