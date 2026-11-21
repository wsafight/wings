use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionFault, Pid, PreemptionToken, RequestId, RequestOutcome,
    ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 16,
        requests: 4,
    }
}

fn request(
    worker: &mut ScalarWorker,
    module: &ScalarModule,
    pid: Pid,
    name: &str,
    arguments: &[i64],
    budget: u64,
) -> RequestId {
    worker
        .request(
            pid,
            &module.handler(name).unwrap(),
            arguments,
            budget,
            Duration::from_secs(60),
        )
        .unwrap()
}

fn deliver(worker: &mut ScalarWorker, pid: Pid) {
    for _ in 0..256 {
        match worker.tick(128).unwrap() {
            WorkerEvent::Suspended(current) => assert_eq!(current, pid),
            WorkerEvent::Delivered(current) => {
                assert_eq!(current, pid);
                return;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("delivery did not finish");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let roots = unsafe { ScalarModule::load_trusted(Path::new(&arguments[2])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn(&module, limits(), 4).unwrap();
    let reply = request(&mut worker, &module, pid, "start", &[1], 128);
    worker
        .send(pid, &module.handler("add").unwrap(), &[2], 128)
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    let snapshot = worker.snapshot(pid).unwrap();
    assert_eq!(snapshot.queued, 2);
    assert_eq!(snapshot.committed_state, Some(0));
    assert_eq!(worker.take_reply(reply).unwrap(), None);
    deliver(&mut worker, pid);
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(9))
    );
    deliver(&mut worker, pid);
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 921);
    let reply = request(&mut worker, &module, pid, "flags", &[], 128);
    deliver(&mut worker, pid);
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(921))
    );
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 7);
    worker
        .send(pid, &module.handler("ping").unwrap(), &[], 128)
        .unwrap();
    deliver(&mut worker, pid);
    deliver(&mut worker, pid);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Idle);
    let reply = request(&mut worker, &module, pid, "wide_start", &[], 512);
    deliver(&mut worker, pid);
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(7))
    );
    deliver(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 2080);
    worker.stop(pid).unwrap();

    for (name, arguments, budget, fault) in [
        ("fail", vec![0], 128, ExecutionFault::DivisionByZero),
        ("budget", vec![], 3, ExecutionFault::BudgetExhausted),
        ("twice", vec![], 128, ExecutionFault::SendRejected),
    ] {
        let pid = worker.spawn(&module, limits(), 1).unwrap();
        let reply = request(&mut worker, &module, pid, name, &arguments, budget);
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.snapshot(pid).unwrap().queued, 1);
        let mut exited = false;
        for _ in 0..8 {
            if matches!(worker.tick(128).unwrap(), WorkerEvent::Exited { .. }) {
                exited = true;
                break;
            }
        }
        assert!(exited);
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.execution.fault, Some(fault));
        assert_eq!(exit.committed_state, Some(0));
        assert_eq!(exit.queued, 1);
        assert_eq!(exit.resources_before_cleanup.messages, 2);
        assert_eq!(
            worker.take_reply(reply).unwrap(),
            Some(RequestOutcome::Failed)
        );
        assert_eq!(vm.used().messages, 0);
    }

    let pid = worker.spawn(&module, limits(), 1).unwrap();
    let reply = request(&mut worker, &module, pid, "start", &[1], 128);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    worker.stop(pid).unwrap();
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Idle);
    assert_eq!(vm.used().messages, 0);

    let pid = worker.spawn_with_heap(&roots, limits(), 2, 3).unwrap();
    let reply = request(&mut worker, &roots, pid, "start", &[], 128);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    assert_eq!(worker.snapshot(pid).unwrap().queued, 1);
    deliver(&mut worker, pid);
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(9))
    );
    deliver(&mut worker, pid);
    let reply = request(&mut worker, &roots, pid, "read", &[], 128);
    deliver(&mut worker, pid);
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(12))
    );
    worker.stop(pid).unwrap();
    drop(worker);
    assert_eq!(vm.used(), Resources::default());

    for level in 0..3 {
        let mut outer = limits();
        let mut middle = limits();
        let mut inner = limits();
        match level {
            0 => outer.messages = 1,
            1 => middle.messages = 1,
            _ => inner.messages = 1,
        }
        let vm = ResourceAccount::vm(outer);
        let tenant = vm.tenant(middle).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let pid = worker.spawn(&module, inner, 2).unwrap();
        let reply = request(&mut worker, &module, pid, "start", &[1], 128);
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.snapshot(pid).unwrap().queued, 0);
        assert_eq!(vm.used().messages, 1);
        assert!(matches!(
            worker.tick(128).unwrap(),
            WorkerEvent::Exited { .. }
        ));
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::SendRejected)
        );
        assert_eq!(
            worker.take_reply(reply).unwrap(),
            Some(RequestOutcome::Failed)
        );
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }

    for level in 0..3 {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let pid = worker.spawn(&module, limits(), 2).unwrap();
        let reply = request(&mut worker, &module, pid, "start", &[1], 128);
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
        let used = worker.resources(pid).unwrap().bytes;
        if level == 2 {
            worker.stop(pid).unwrap();
            assert_eq!(
                worker.take_reply(reply).unwrap(),
                Some(RequestOutcome::Failed)
            );
            let mut bounded = limits();
            bounded.bytes = used;
            let pid = worker.spawn(&module, bounded, 2).unwrap();
            let reply = request(&mut worker, &module, pid, "start", &[1], 128);
            assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.queued(pid).unwrap(), 0);
            assert!(matches!(
                worker.tick(128).unwrap(),
                WorkerEvent::Exited { .. }
            ));
            assert_eq!(
                worker.take_reply(reply).unwrap(),
                Some(RequestOutcome::Failed)
            );
        } else {
            let account = if level == 0 { &vm } else { &tenant };
            let reservation = account
                .reserve(Resources {
                    bytes: account.limit().bytes - account.used().bytes,
                    ..Resources::default()
                })
                .unwrap();
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.queued(pid).unwrap(), 0);
            assert!(matches!(
                worker.tick(128).unwrap(),
                WorkerEvent::Exited { .. }
            ));
            assert_eq!(
                worker.take_reply(reply).unwrap(),
                Some(RequestOutcome::Failed)
            );
            drop(reservation);
        }
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::SendRejected)
        );
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }

    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn(&module, limits(), 1).unwrap();
    request(&mut worker, &module, pid, "start", &[1], 128);
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    let mut process = module.instantiate().unwrap();
    let mut execution = process
        .start_controlled(
            "start",
            &[1],
            128,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    assert!(execution.resume(128).is_err());
    assert_eq!(
        execution.snapshot().fault,
        Some(ExecutionFault::InvalidControl)
    );
    println!(
        "Self-send verified: call-point FIFO, no replay/rollback, quotas, roots, cancellation and cleanup"
    );
}
