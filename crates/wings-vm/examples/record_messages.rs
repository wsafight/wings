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

fn read(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid) -> i64 {
    let request = worker
        .request(
            pid,
            &module.handler("read").unwrap(),
            &[],
            4096,
            Duration::from_secs(60),
        )
        .unwrap();
    drive(worker, 512);
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
        ("start", vec![7, 0], Some(7), Some(0), 16),
        ("start", vec![9, 1], Some(9), None, 16),
        ("self_msg", vec![11], None, Some(11), 16),
        ("ask", vec![13], Some(0), Some(14), 16),
        ("pair", vec![17], Some(35), Some(0), 16),
        ("pair", vec![19], Some(39), Some(0), 4),
    ] {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let parent = worker
            .spawn_with_heap(&module, limits(), 2, heap_records)
            .unwrap();
        let request = worker
            .request(
                parent,
                &module.handler(entry).unwrap(),
                &values,
                4096,
                Duration::from_secs(60),
            )
            .unwrap();
        drive(&mut worker, 1024);
        let outcome = worker.take_reply(request).unwrap().unwrap();
        if parent_value.is_none() {
            assert_eq!(outcome, RequestOutcome::Failed);
        } else {
            assert!(matches!(outcome, RequestOutcome::Reply(_)));
        }
        if let Some(value) = parent_value {
            assert_eq!(read(&mut worker, &module, parent), value);
        }
        if let Some(value) = child_value {
            let child = worker.pids().find(|pid| *pid != parent).unwrap();
            assert_eq!(read(&mut worker, &module, child), value);
        }
        assert_eq!(vm.used().messages, 0);
        assert_eq!(vm.used().requests, 0);
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
    for boundary in 0..96 {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
        let pid = worker.spawn_with_heap(&module, limits(), 2, 4).unwrap();
        worker
            .request(
                pid,
                &module.handler("pair").unwrap(),
                &[7],
                4096,
                Duration::from_secs(60),
            )
            .unwrap();
        drive(&mut worker, boundary);
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn_with_heap(&module, limits(), 2, 2).unwrap();
    let baseline = vm.used();
    let store = module.handler("store").unwrap();
    assert!(worker.send(pid, &store, &[1], 4096).is_err());
    assert!(
        worker
            .request(
                pid,
                &module.handler("compute").unwrap(),
                &[1],
                4096,
                Duration::from_secs(60)
            )
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    for name in ["store", "compute"] {
        let mut probe = ScalarWorker::new(&tenant, 1).unwrap();
        probe.enable_wait().unwrap();
        let target = probe.spawn_with_heap(&module, limits(), 1, 2).unwrap();
        let handler = module.handler(name).unwrap();
        let sender = if handler.schema().reply.is_some() {
            probe
                .open_request_ingress(target, &handler, 1, 4096)
                .unwrap()
        } else {
            probe.open_ingress(target, &handler, 1, 4096).unwrap()
        };
        let registered = vm.used();
        assert!(sender.send(&[1]).is_err());
        assert!(sender.request(&[1], Duration::from_secs(60)).is_err());
        assert_eq!(vm.used(), registered);
        drop((sender, probe));
        assert_eq!(vm.used(), baseline);
    }
    worker
        .request(
            pid,
            &module.handler("pair").unwrap(),
            &[7],
            4096,
            Duration::from_secs(60),
        )
        .unwrap();
    let mut heap_failure = false;
    for _ in 0..512 {
        if let WorkerEvent::Exited { reason, .. } = worker.tick(1).unwrap() {
            heap_failure |= reason.contains("heap exhausted");
        }
    }
    assert!(heap_failure);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    let mut no_heap = ScalarWorker::new(&tenant, 2).unwrap();
    let pid = no_heap.spawn(&module, limits(), 1).unwrap_err();
    assert!(pid.contains("heap"));
    drop(no_heap);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Record messages verified: source copies, target roots, async arguments, failure effects and bounded cleanup"
    );
}
