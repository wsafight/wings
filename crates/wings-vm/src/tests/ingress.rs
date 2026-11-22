use super::*;

fn deliver(worker: &mut ScalarWorker, pid: Pid) {
    for _ in 0..32 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(actual) => assert_eq!(actual, pid),
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                return;
            }
            event => panic!("unexpected ingress delivery: {event:?}"),
        }
    }
    panic!("bounded ingress delivery did not finish");
}

#[test]
fn registration_rejections_do_not_change_queues_or_accounting() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    let add = module.handler("Add").unwrap();
    let baseline = vm.used();
    assert!(worker.open_ingress(pid, &add, 1, 128).is_err());
    assert_eq!(vm.used(), baseline);
    worker.enable_wait().unwrap();
    let baseline = vm.used();
    for capacity in [0, 65, usize::MAX] {
        assert!(worker.open_ingress(pid, &add, capacity, 128).is_err());
    }
    assert!(worker.open_ingress(pid, &add, 1, 0).is_err());
    assert!(
        worker
            .open_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
            .is_err()
    );
    assert!(
        worker
            .open_ingress(pid, &module.handler("Ack").unwrap(), 1, 128)
            .is_err()
    );
    assert!(
        worker
            .open_ingress(pid, &super::module().handler("Add").unwrap(), 1, 128)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    let sender = worker.open_ingress(pid, &add, 1, 128).unwrap();
    let registered = vm.used();
    assert!(registered.bytes > baseline.bytes);
    assert!(worker.open_ingress(pid, &add, 1, 128).is_err());
    assert!(sender.send(&[]).is_err());
    assert!(sender.send(&[1, 2]).is_err());
    assert_eq!(vm.used(), registered);
    drop((worker, sender));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn queued_ingress_keeps_prepaid_message_when_mailbox_is_full() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let add = module.handler("Add").unwrap();
    let sender = worker.open_ingress(pid, &add, 2, 128).unwrap();
    let baseline = vm.used();
    worker.send(pid, &add, &[10], 128).unwrap();
    assert_eq!(sender.send(&[2]).unwrap(), IngressWake::Notified);
    sender.send(&[3]).unwrap();
    let full = vm.used();
    assert_eq!(full.messages, 3);
    assert!(sender.send(&[99]).is_err());
    assert_eq!(vm.used(), full);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 10);
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 12);
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 15);
    assert_eq!(vm.used().messages, baseline.messages);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
    drop((worker, sender));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn quota_failure_and_scalar_validation_return_every_reservation() {
    for name in ["Bool", "Unit", "Add"] {
        let (vm, tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
        worker.enable_wait().unwrap();
        let sender = worker
            .open_ingress(pid, &module.handler(name).unwrap(), 1, 128)
            .unwrap();
        let baseline = vm.used();
        if name != "Add" {
            assert!(sender.send(&[2]).is_err());
            assert_eq!(vm.used(), baseline);
        }
        let blocker = tenant
            .reserve(Resources {
                messages: tenant.limit().messages,
                ..Resources::default()
            })
            .unwrap();
        assert!(sender.send(&[0]).is_err());
        drop(blocker);
        assert_eq!(vm.used(), baseline);
        sender.send(&[0]).unwrap();
        deliver(&mut worker, pid);
        drop((worker, sender));
        assert_eq!(vm.used(), Resources::default());
    }
    let (vm, tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let baseline = vm.used();
    let blocker = tenant
        .reserve(Resources {
            bytes: tenant.limit().bytes - tenant.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    assert!(
        worker
            .open_ingress(pid, &module.handler("Add").unwrap(), 64, 128)
            .is_err()
    );
    drop(blocker);
    assert_eq!(vm.used(), baseline);
}

#[test]
fn stop_and_slot_reuse_never_redirect_old_senders_and_retain_control_leases() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_ingress(pid, &module.handler("Add").unwrap(), 2, 128)
        .unwrap();
    sender.send(&[1]).unwrap();
    sender.send(&[2]).unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    worker.stop(pid).unwrap();
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    assert!(sender.send(&[3]).unwrap_err().contains("closed"));
    let replacement = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    assert_ne!(replacement, pid);
    assert!(sender.send(&[3]).is_err());
    assert_eq!(worker.queued(replacement).unwrap(), 0);
    drop(worker);
    assert_eq!(vm.used().messages, 0);
    assert!(vm.used().bytes >= 4096);
    let clone = sender.clone();
    drop(sender);
    assert!(vm.used().bytes >= 4096);
    assert!(clone.send(&[3]).is_err());
    std::thread::spawn(move || drop(clone)).join().unwrap();
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn failed_handler_closes_ingress_and_discards_remaining_prepaid_messages() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_ingress(pid, &module.handler("Fail").unwrap(), 2, 128)
        .unwrap();
    sender.send(&[]).unwrap();
    sender.send(&[]).unwrap();
    assert!(
        matches!(worker.tick(128).unwrap(), WorkerEvent::Exited { pid: actual, .. } if actual == pid)
    );
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    assert!(sender.send(&[]).is_err());
    drop((worker, sender));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn concurrent_producers_wake_waiting_worker_and_preserve_all_accepted_messages() {
    let mut limit = limits(16 << 20);
    limit.messages = 64;
    let vm = ResourceAccount::vm(limit);
    let tenant = vm.tenant(limit).unwrap();
    let module = module();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limit, 1).unwrap();
    let sender = worker
        .open_ingress(pid, &module.handler("Add").unwrap(), 32, 128)
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let sender = sender.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..8 {
                    assert_eq!(sender.send(&[1]).unwrap(), IngressWake::Notified);
                }
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        worker.wait(Duration::from_secs(2)).unwrap(),
        WorkerWait::Ready
    );
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(vm.used().messages, 32);
    for _ in 0..32 {
        deliver(&mut worker, pid);
    }
    assert_eq!(worker.committed_state(pid).unwrap(), 32);
    assert_eq!(vm.used().messages, 0);
    drop((worker, sender));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn racing_close_and_publish_cannot_leak_or_reopen_a_route() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_ingress(pid, &module.handler("Add").unwrap(), 1, 128)
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let producer = sender.clone();
    let signal = barrier.clone();
    let handle = std::thread::spawn(move || {
        signal.wait();
        for _ in 0..1024 {
            let _ = producer.send(&[1]);
        }
    });
    barrier.wait();
    drop(worker);
    handle.join().unwrap();
    assert!(sender.send(&[1]).is_err());
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    drop(sender);
    assert_eq!(vm.used(), Resources::default());
}
