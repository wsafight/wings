use std::path::Path;
use std::time::Duration;
use wings_vm::{
    Pid, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 32,
        requests: 8,
    }
}

fn read(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid) -> i64 {
    let request = worker
        .request(
            pid,
            &module.handler("read").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..32 {
        worker.tick(128).unwrap();
        if let Some(RequestOutcome::Reply(value)) = worker.take_reply(request).unwrap() {
            return value;
        }
    }
    panic!("read did not finish");
}

fn verify_early_completion_lifetime(module: &ScalarModule) {
    for stop_owner in [false, true] {
        for stop_target in [false, true] {
            let vm = ResourceAccount::vm(limits());
            let tenant = vm.tenant_accounted(limits()).unwrap();
            let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 2).unwrap();
            worker.enable_wait().unwrap();
            let owner = worker.spawn(module, limits(), 2).unwrap();
            let request = worker
                .request(
                    owner,
                    &module.handler("start").unwrap(),
                    &[0, 0, 0],
                    256,
                    Duration::from_secs(60),
                )
                .unwrap();
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
            let target = worker.pids().find(|pid| *pid != owner).unwrap();
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(owner));
            assert_eq!(worker.queued(target).unwrap(), 1);
            worker.wait(Duration::ZERO).unwrap();
            assert_eq!(worker.queued(owner).unwrap(), 1);
            assert_eq!(worker.resources(owner).unwrap().requests, 2);
            let baseline = vm.used();
            assert!(
                worker
                    .request(
                        target,
                        &module.handler("read").unwrap(),
                        &[],
                        128,
                        Duration::ZERO
                    )
                    .is_err()
            );
            assert_eq!(vm.used(), baseline);
            if stop_owner {
                worker.stop(owner).unwrap();
                assert_eq!(
                    worker.take_reply(request).unwrap(),
                    Some(RequestOutcome::Failed)
                );
                assert_eq!(vm.used().requests, 1);
                assert_eq!(vm.used().messages, 1);
            }
            if stop_target {
                worker.stop(target).unwrap();
                assert_eq!(vm.used().requests, u64::from(!stop_owner));
            }
            for _ in 0..64 {
                worker.tick(128).unwrap();
            }
            if !stop_owner {
                assert_eq!(
                    worker.take_reply(request).unwrap(),
                    Some(RequestOutcome::Reply(9))
                );
                assert_eq!(worker.committed_state(owner).unwrap(), 100);
            }
            if !stop_target {
                assert_eq!(worker.committed_state(target).unwrap(), 7);
            }
            assert_eq!(vm.used().requests, 0);
            assert_eq!(vm.used().messages, 0);
            drop((worker, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    verify_early_completion_lifetime(&module);
    for (timeout, cancel, fail, expected) in [
        (60000, 0, 0, 7),
        (0, 0, 0, 100),
        (60000, 1, 0, 200),
        (60000, 0, 1, 300),
    ] {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 3, 4).unwrap();
        let pid = worker.spawn(&module, limits(), 2).unwrap();
        let reply = worker
            .request(
                pid,
                &module.handler("start").unwrap(),
                &[timeout, cancel, fail],
                256,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        let child = worker.pids().find(|child| *child != pid).unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.queued(child).unwrap(), 1);
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(child));
        for _ in 0..64 {
            worker.tick(128).unwrap();
        }
        assert_eq!(
            worker.take_reply(reply).unwrap(),
            Some(RequestOutcome::Reply(9))
        );
        assert_eq!(read(&mut worker, &module, pid), expected);
        if fail == 0 {
            assert_eq!(read(&mut worker, &module, child), 7);
        }
        assert_eq!(vm.used().requests, 0);
        assert_eq!(vm.used().messages, 0);
        assert!(
            worker
                .send(pid, &module.handler("completed").unwrap(), &[1, 0, 3], 128)
                .is_err()
        );
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
    for remaining in 0..24 {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 3, 4).unwrap();
        let pid = worker.spawn(&module, limits(), 2).unwrap();
        worker
            .request(
                pid,
                &module.handler("start").unwrap(),
                &[60000, 0, 0],
                256,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        for _ in 0..remaining {
            worker.tick(1).unwrap();
        }
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 4).unwrap();
    let pid = worker.spawn(&module, limits(), 1).unwrap();
    let reply = worker
        .request(
            pid,
            &module.handler("self_msg").unwrap(),
            &[],
            256,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..64 {
        worker.tick(128).unwrap();
    }
    assert_eq!(
        worker.take_reply(reply).unwrap(),
        Some(RequestOutcome::Reply(9))
    );
    assert_eq!(read(&mut worker, &module, pid), 12);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Request events verified: call-point request, asynchronous replies, timeout/cancel/failure, self request and cleanup"
    );
}
