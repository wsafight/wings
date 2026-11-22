use std::path::Path;
use std::time::Duration;

use wings_vm::{
    IngressWake, Pid, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    WorkerEvent, WorkerWait,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 64,
        requests: 64,
    }
}

fn finish(worker: &mut ScalarWorker, pid: Pid) {
    for _ in 0..128 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(actual) => assert_eq!(actual, pid),
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                return;
            }
            event => panic!("unexpected request event: {event:?}"),
        }
    }
    panic!("bounded request did not finish");
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limits(), 1).unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("bump").unwrap(), 16, 128)
        .unwrap();
    let producers: Vec<_> = (0..4)
        .map(|_| {
            let sender = sender.clone();
            std::thread::spawn(move || {
                (0..4)
                    .map(|_| {
                        let (request, wake) =
                            sender.request(&[1], Duration::from_secs(60)).unwrap();
                        assert_eq!(wake, IngressWake::Notified);
                        request
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut requests: Vec<_> = producers
        .into_iter()
        .flat_map(|producer| producer.join().unwrap())
        .collect();
    assert_eq!(vm.used().requests, 16);
    assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::Ready);
    for _ in 0..16 {
        finish(&mut worker, pid);
    }
    assert_eq!(worker.committed_state(pid).unwrap(), 16);
    assert!(sender.request(&[1], Duration::from_secs(60)).is_err());
    let mut replies: Vec<_> = requests
        .iter_mut()
        .map(|request| match request.try_take().unwrap() {
            Some(RequestOutcome::Reply(value)) => value,
            result => panic!("unexpected result: {result:?}"),
        })
        .collect();
    replies.sort_unstable();
    assert_eq!(replies, (1..=16).collect::<Vec<_>>());
    assert_eq!(vm.used().requests, 0);
    let (mut timed, _) = sender.request(&[4], Duration::ZERO).unwrap();
    assert_eq!(timed.try_take().unwrap(), Some(RequestOutcome::TimedOut));
    finish(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 20);
    let (mut cancelled, _) = sender.request(&[5], Duration::from_secs(60)).unwrap();
    cancelled.cancel().unwrap();
    assert_eq!(
        cancelled.try_take().unwrap(),
        Some(RequestOutcome::Cancelled)
    );
    finish(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 25);
    let (mut pending, _) = sender.request(&[9], Duration::from_secs(60)).unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    drop((worker, sender, requests, timed, cancelled));
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().requests, 1);
    assert_eq!(
        pending.wait(Duration::ZERO).unwrap(),
        Some(RequestOutcome::Failed)
    );
    drop(pending);
    assert_eq!(vm.used(), Resources::default());

    for (name, values, expected) in [
        ("ack", vec![], RequestOutcome::Reply(0)),
        ("echo_bool", vec![1], RequestOutcome::Reply(1)),
        ("fail_request", vec![], RequestOutcome::Failed),
    ] {
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        worker.enable_wait().unwrap();
        let pid = worker.spawn(&module, limits(), 1).unwrap();
        let sender = worker
            .open_request_ingress(pid, &module.handler(name).unwrap(), 1, 128)
            .unwrap();
        let (request, _) = sender.request(&values, Duration::from_secs(60)).unwrap();
        let waiter = std::thread::spawn(move || {
            let mut request = request;
            request.wait(Duration::from_secs(2)).unwrap()
        });
        let mut completed = false;
        for _ in 0..128 {
            if matches!(
                worker.tick(1).unwrap(),
                WorkerEvent::Delivered(_) | WorkerEvent::Exited { .. }
            ) {
                completed = true;
                break;
            }
        }
        assert!(completed);
        assert_eq!(waiter.join().unwrap(), Some(expected));
        drop((sender, worker));
        assert_eq!(vm.used(), Resources::default());
    }
    println!(
        "Ingress requests verified: fixed result capacity, committed scalar replies, timeouts and final lease cleanup"
    );
}
