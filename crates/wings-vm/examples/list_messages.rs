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
        requests: 16,
    }
}

fn drive(worker: &mut ScalarWorker, count: usize) {
    for _ in 0..count {
        worker.tick(1).unwrap();
    }
}

fn read(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid, name: &str) -> i64 {
    let request = worker
        .request(
            pid,
            &module.handler(name).unwrap(),
            &[],
            8192,
            Duration::from_secs(60),
        )
        .unwrap();
    drive(worker, 4096);
    match worker.take_reply(request).unwrap() {
        Some(RequestOutcome::Reply(value)) => value,
        other => panic!("unexpected read reply: {other:?}"),
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    for (entry, values, child_value, parent_value, heap_records) in [
        ("Start", vec![7, 0], Some(7), Some(-1), 16),
        ("Start", vec![9, 1], Some(9), None, 16),
        ("Self", vec![11], None, Some(11), 16),
        ("Ask", vec![13], Some(-1), Some(27), 16),
        ("Mixed", vec![17], Some(55), Some(-1), 16),
        ("Mixed", vec![19], Some(61), Some(-1), 9),
        ("Empty", vec![], None, Some(-1), 2),
        ("Fill", vec![64, 0], Some(64), Some(64), 96),
        ("Fill", vec![32, 1], Some(32), Some(32), 96),
    ] {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker
            .spawn_with_heap(&module, limits(), 2, heap_records)
            .unwrap();
        let request = worker
            .request(
                parent,
                &module.handler(entry).unwrap(),
                &values,
                32768,
                Duration::from_secs(60),
            )
            .unwrap();
        drive(&mut worker, 16384);
        let outcome = worker.take_reply(request).unwrap().unwrap();
        if parent_value.is_none() {
            assert_eq!(outcome, RequestOutcome::Failed);
        } else {
            assert!(
                matches!(outcome, RequestOutcome::Reply(_)),
                "{entry}: {outcome:?}"
            );
        }
        if let Some(value) = parent_value {
            assert_eq!(read(&mut worker, &module, parent, "Read"), value, "{entry}");
        }
        if let Some(value) = child_value {
            let child = worker.pids().find(|pid| *pid != parent).unwrap();
            assert_eq!(read(&mut worker, &module, child, "Read"), value, "{entry}");
            if entry == "Start" || entry == "Fill" {
                let next = if entry == "Fill" {
                    value - 1
                } else {
                    value + 1
                };
                assert_eq!(read(&mut worker, &module, child, "Next"), next);
            }
        }
        assert_eq!(vm.used().messages, 0);
        assert_eq!(vm.used().requests, 0);
        drop(worker);
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
    for (count, twice) in [(65, 0), (33, 1)] {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker.spawn_with_heap(&module, limits(), 2, 96).unwrap();
        let request = worker
            .request(
                parent,
                &module.handler("Fill").unwrap(),
                &[count, twice],
                32768,
                Duration::from_secs(60),
            )
            .unwrap();
        let mut rejected = false;
        for _ in 0..16384 {
            if let WorkerEvent::Exited { pid, reason } = worker.tick(1).unwrap() {
                assert_eq!(pid, parent);
                rejected |= reason.contains("send rejected");
            }
        }
        assert!(rejected);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
        let child = worker.pids().next().unwrap();
        assert_eq!(read(&mut worker, &module, child, "Read"), -1);
        drop(worker);
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
    for entry in ["Start", "Mixed", "Fill"] {
        for boundary in 0..160 {
            let vm = ResourceAccount::vm(limits());
            let tenant = vm.tenant_accounted(limits()).unwrap();
            let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
            let pid = worker.spawn_with_heap(&module, limits(), 2, 48).unwrap();
            let request = worker
                .request(
                    pid,
                    &module.handler(entry).unwrap(),
                    if entry == "Mixed" { &[7] } else { &[16, 0] },
                    32768,
                    Duration::from_secs(60),
                )
                .unwrap();
            drive(&mut worker, boundary);
            if boundary % 2 == 0 {
                worker.stop(pid).unwrap();
            }
            let _ = worker.take_reply(request).unwrap();
            drop(worker);
            drop(tenant);
            assert_eq!(vm.used(), Resources::default());
        }
    }
    let mut observed_source_copy = false;
    let mut observed_target_import = false;
    for boundary in 0..320 {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker.spawn_with_heap(&module, limits(), 2, 96).unwrap();
        let prepare = worker
            .request(
                parent,
                &module.handler("Prepare").unwrap(),
                &[64],
                32768,
                Duration::from_secs(60),
            )
            .unwrap();
        drive(&mut worker, 4096);
        assert_eq!(
            worker.take_reply(prepare).unwrap(),
            Some(RequestOutcome::Reply(64))
        );
        let request = worker
            .request(
                parent,
                &module.handler("Copy").unwrap(),
                &[],
                4096,
                Duration::from_secs(60),
            )
            .unwrap();
        drive(&mut worker, boundary);
        let parent_snapshot = worker.snapshot(parent).unwrap();
        observed_source_copy |= parent_snapshot.active_handler == Some("Copy")
            && parent_snapshot
                .execution
                .is_some_and(|execution| execution.consumed_steps() > 16)
            && worker.pids().filter(|pid| *pid != parent).all(|pid| {
                let snapshot = worker.snapshot(pid).unwrap();
                snapshot.active_handler.is_none() && snapshot.queued == 0
            });
        let child = worker.pids().find(|pid| *pid != parent);
        if let Some(child) = child {
            let snapshot = worker.snapshot(child).unwrap();
            let importing = snapshot.active_handler == Some("Store")
                && snapshot
                    .heap
                    .is_some_and(|heap| (2..65).contains(&heap.allocated));
            observed_target_import |= importing;
            if boundary % 2 == 0 && importing {
                worker.stop(child).unwrap();
            }
        }
        if boundary % 3 == 0 {
            worker.stop(parent).unwrap();
        }
        let _ = worker.take_reply(request).unwrap();
        drop(worker);
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
    assert!(observed_source_copy && observed_target_import);
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
    worker.enable_wait().unwrap();
    let parent = worker.spawn_with_heap(&module, limits(), 2, 7).unwrap();
    let baseline = vm.used();
    for name in ["Store", "Compute"] {
        let handler = module.handler(name).unwrap();
        for argument in [1, 2i64 << 32, -1] {
            assert!(worker.send(parent, &handler, &[argument], 8192).is_err());
            assert!(
                worker
                    .request(parent, &handler, &[argument], 8192, Duration::from_secs(60))
                    .is_err()
            );
        }
        assert_eq!(vm.used(), baseline);
        let mut probe = ScalarWorker::new(&tenant, 1).unwrap();
        probe.enable_wait().unwrap();
        let target = probe.spawn_with_heap(&module, limits(), 1, 7).unwrap();
        let sender = if handler.schema().reply.is_some() {
            probe
                .open_request_ingress(target, &handler, 1, 8192)
                .unwrap()
        } else {
            probe.open_ingress(target, &handler, 1, 8192).unwrap()
        };
        let registered = vm.used();
        for argument in [1, 2i64 << 32, -1] {
            assert!(sender.send(&[argument]).is_err());
            assert!(
                sender
                    .request(&[argument], Duration::from_secs(60))
                    .is_err()
            );
            assert_eq!(vm.used(), registered);
        }
        drop((sender, probe));
    }
    assert_eq!(vm.used(), baseline);
    worker
        .request(
            parent,
            &module.handler("Mixed").unwrap(),
            &[7],
            8192,
            Duration::from_secs(60),
        )
        .unwrap();
    let mut heap_failure = false;
    for _ in 0..4096 {
        if let WorkerEvent::Exited { pid, reason } = worker.tick(1).unwrap() {
            assert_ne!(pid, parent);
            heap_failure |= reason.contains("heap exhausted");
        }
    }
    assert!(heap_failure);
    drop(worker);
    drop(tenant);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "List messages verified: bounded source copies, canonical payloads, target roots, async requests and cancellation cleanup"
    );
}
