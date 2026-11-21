use std::path::Path;
use std::time::Duration;

use wings_vm::{
    IngressWake, Pid, ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent,
    WorkerWait,
};

fn finish(worker: &mut ScalarWorker, pid: Pid) {
    for _ in 0..128 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(actual) => assert_eq!(actual, pid),
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                return;
            }
            event => panic!("unexpected ingress event: {event:?}"),
        }
    }
    panic!("ingress message exceeded the bounded tick count");
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let limit = Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 64,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limit);
    let tenant = vm.tenant(limit).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limit, 1).unwrap();
    let sender = worker
        .open_ingress(pid, &module.handler("set").unwrap(), 2, 128)
        .unwrap();
    worker
        .send(pid, &module.handler("set").unwrap(), &[10], 128)
        .unwrap();
    let producer = sender.clone();
    std::thread::spawn(move || {
        assert_eq!(producer.send(&[20]).unwrap(), IngressWake::Notified);
        producer.send(&[30]).unwrap();
        assert!(producer.send(&[99]).is_err());
    })
    .join()
    .unwrap();
    assert_eq!(vm.used().messages, 3);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    for expected in [10, 20, 30] {
        finish(&mut worker, pid);
        assert_eq!(worker.committed_state(pid).unwrap(), expected);
    }
    let producer = sender.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        producer.send(&[40]).unwrap();
    });
    for _ in 0..4 {
        if worker.wait(Duration::from_secs(1)).unwrap() == WorkerWait::Ready {
            break;
        }
    }
    handle.join().unwrap();
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    finish(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 40);
    sender.send(&[50]).unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    drop(worker);
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    assert!(vm.used().bytes >= 4096);
    assert!(sender.send(&[60]).is_err());
    std::thread::spawn(move || drop(sender)).join().unwrap();
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Worker ingress verified: cross-thread FIFO, bounded admission, OS wake and close leases"
    );
}
