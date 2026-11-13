use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent, WorkerWait,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let limits = Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 16,
        requests: 8,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 3, 4).unwrap();
    assert!(worker.wait(Duration::ZERO).is_err());
    let baseline = vm.used();
    worker.enable_wait().unwrap();
    assert_eq!(vm.used().bytes - baseline.bytes, 4096);
    assert!(worker.enable_wait().is_err());
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
    let start = Instant::now();
    assert_eq!(
        worker.wait(Duration::from_millis(2)).unwrap(),
        WorkerWait::TimedOut
    );
    assert!(start.elapsed() >= Duration::from_millis(2));
    assert!(worker.wait(Duration::MAX).is_err());
    let pid = worker.spawn(&module, limits, 1).unwrap();
    let ticket = worker
        .request(
            pid,
            &module.handler("Start").unwrap(),
            &[0, 0, 0],
            256,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(
        worker.wait(Duration::from_secs(1)).unwrap(),
        WorkerWait::Ready
    );
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    let child = worker.pids().find(|child| *child != pid).unwrap();
    let before = worker.snapshot(pid).unwrap().execution;
    assert_eq!(
        worker.wait(Duration::from_secs(1)).unwrap(),
        WorkerWait::Ready
    );
    assert_eq!(worker.snapshot(pid).unwrap().execution, before);
    assert_eq!(worker.queued(pid).unwrap(), 1);
    assert_eq!(worker.queued(child).unwrap(), 1);
    for _ in 0..64 {
        assert_eq!(
            worker.wait(Duration::ZERO).unwrap(),
            if worker.pids().any(|current| {
                worker.queued(current).unwrap() > 0
                    || worker
                        .snapshot(current)
                        .unwrap()
                        .execution
                        .is_some_and(|execution| {
                            execution.phase == wings_vm::ExecutionPhase::Suspended
                        })
            }) {
                WorkerWait::Ready
            } else {
                WorkerWait::TimedOut
            }
        );
        worker.tick(128).unwrap();
    }
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Reply(9))
    );
    assert_eq!(worker.committed_state(pid).unwrap(), 100);
    assert_eq!(worker.committed_state(child).unwrap(), 7);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Worker wait verified: accounted OS timeout, readiness, request expiry, completion admission and cleanup"
    );
}
