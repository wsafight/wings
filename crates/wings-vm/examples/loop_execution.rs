use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, RequestOutcome, ResourceAccount,
    Resources, ScalarModule, ScalarWorker, StackPool, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let account = tenant.process(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    let mut execution = process
        .start_controlled(
            "Heap",
            &[30],
            1024,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut completed = false;
    for _ in 0..1024 {
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => {}
            ExecutionStatus::Complete(reply) => {
                assert_eq!(reply, Some(30));
                completed = true;
                break;
            }
        }
    }
    assert!(completed);
    drop(execution);
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(30));
    assert!(process.heap_stats().unwrap().collections >= 10);
    drop(process);
    assert_eq!(vm.used(), baseline);
    for cancel in [false, true] {
        let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
        let cancellation = CancellationToken::default();
        let mut execution = process
            .start_controlled(
                "Heap",
                &[3000],
                65536,
                &cancellation,
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..25 {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        if cancel {
            cancellation.cancel();
            assert!(execution.resume(1).unwrap_err().contains("cancelled"));
        }
        drop(execution);
        assert!(
            process
                .invoke("Get", &[])
                .unwrap_err()
                .contains("terminated")
        );
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
    let baseline = vm.used();
    let first = worker.spawn(&module, limits(), 2).unwrap();
    let second = worker.spawn(&module, limits(), 2).unwrap();
    let timeout = Duration::from_secs(60);
    let long = worker
        .request(
            first,
            &module.handler("Count").unwrap(),
            &[100],
            4096,
            timeout,
        )
        .unwrap();
    let quick = worker
        .request(second, &module.handler("Get").unwrap(), &[], 128, timeout)
        .unwrap();
    let mut quick_done = false;
    let mut long_done = false;
    for tick in 0..2048 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(_) => {}
            WorkerEvent::Delivered(pid) if pid == second => {
                assert!(tick < 8);
                assert!(!long_done);
                quick_done = true;
            }
            WorkerEvent::Delivered(pid) if pid == first => {
                assert!(quick_done);
                long_done = true;
                break;
            }
            event => panic!("unexpected event {event:?}"),
        }
    }
    assert!(quick_done && long_done);
    assert_eq!(
        worker.take_reply(quick).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    assert_eq!(
        worker.take_reply(long).unwrap(),
        Some(RequestOutcome::Reply(100))
    );
    let failing = worker
        .request(
            first,
            &module.handler("Fail").unwrap(),
            &[110],
            4096,
            timeout,
        )
        .unwrap();
    for _ in 0..25 {
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(first));
        assert_eq!(worker.committed_state(first).unwrap(), 100);
    }
    assert!(
        matches!(worker.tick(4096).unwrap(), WorkerEvent::Exited { pid, reason } if pid == first && reason.contains("division by zero"))
    );
    assert_eq!(
        worker.take_reply(failing).unwrap(),
        Some(RequestOutcome::Failed)
    );
    worker.enable_timer().unwrap();
    let timed = worker
        .request(
            second,
            &module.handler("Count").unwrap(),
            &[1_000_000],
            20_000_000,
            timeout,
        )
        .unwrap();
    assert_eq!(
        worker.tick_timed(100_000, Duration::from_nanos(1)).unwrap(),
        WorkerEvent::Suspended(second)
    );
    assert_eq!(worker.committed_state(second).unwrap(), 0);
    worker.stop(second).unwrap();
    assert_eq!(
        worker.take_reply(timed).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert!(vm.used().bytes > baseline.bytes);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Loop execution verified: backedge polls, live roots, no replay, bounded rotation, timer, failure commit and cleanup"
    );
}
