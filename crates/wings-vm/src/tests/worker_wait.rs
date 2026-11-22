use super::*;

#[test]
fn worker_wait_configuration_and_idle_timeouts_are_accounted_and_bounded() {
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant(limits(16 << 20)).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    assert!(worker.wait(Duration::ZERO).is_err());
    let baseline = vm.used();
    let blocker = tenant
        .reserve(Resources {
            bytes: tenant.limit().bytes - tenant.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    assert!(worker.enable_wait().is_err());
    drop(blocker);
    assert_eq!(vm.used(), baseline);
    worker.enable_wait().unwrap();
    assert_eq!(vm.used().bytes - baseline.bytes, 4096);
    assert!(worker.enable_wait().is_err());
    assert_eq!(vm.used().bytes - baseline.bytes, 4096);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
    assert!(worker.wait(Duration::MAX).is_err());
    let start = std::time::Instant::now();
    assert_eq!(
        worker.wait(Duration::from_millis(2)).unwrap(),
        WorkerWait::TimedOut
    );
    assert!(start.elapsed() >= Duration::from_millis(2));
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_wait_never_executes_or_sleeps_for_queued_or_suspended_work() {
    let module = module();
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant(limits(16 << 20)).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[7], 128)
        .unwrap();
    let before = worker.snapshot(pid).unwrap().execution;
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    assert_eq!(worker.snapshot(pid).unwrap().execution, before);
    assert_eq!(worker.queued(pid).unwrap(), 1);
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    let before = worker.snapshot(pid).unwrap().execution;
    assert_eq!(
        worker.wait(Duration::from_secs(1)).unwrap(),
        WorkerWait::Ready
    );
    assert_eq!(worker.snapshot(pid).unwrap().execution, before);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn due_request_is_terminated_without_running_queued_target_or_overwriting_result() {
    let module = module();
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant(limits(16 << 20)).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    let ticket = worker
        .request(
            pid,
            &module.handler("Bump").unwrap(),
            &[7],
            128,
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(
        worker.wait(Duration::from_secs(1)).unwrap(),
        WorkerWait::Ready
    );
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    assert_eq!(worker.queued(pid).unwrap(), 1);
    for _ in 0..8 {
        worker.tick(128).unwrap();
    }
    assert_eq!(worker.committed_state(pid).unwrap(), 7);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}
