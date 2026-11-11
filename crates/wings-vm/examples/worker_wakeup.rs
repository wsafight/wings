use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use wings_vm::{WorkerNotifier, WorkerParker};

fn transferable<Value: Send>() {}

fn shareable<Value: Send + Sync>() {}

fn main() {
    transferable::<WorkerParker>();
    shareable::<WorkerNotifier>();
    let mut parker = WorkerParker::new().expect("create waiting endpoint");
    let notifier = parker.notifier();
    assert!(!parker.wait(Duration::ZERO).expect("empty poll"));
    let started = Instant::now();
    assert!(!parker.wait(Duration::from_millis(5)).expect("timeout"));
    assert!(started.elapsed() >= Duration::from_millis(5));
    assert!(parker.wait(Duration::MAX).is_err());
    notifier.notify().expect("before long deadline");
    assert!(
        parker
            .wait(Duration::from_secs(30 * 24 * 60 * 60))
            .expect("long finite deadline")
    );
    for _ in 0..512 {
        notifier.notify().expect("early notification");
    }
    assert!(parker.wait(Duration::ZERO).expect("pending notification"));
    assert!(
        !parker
            .wait(Duration::ZERO)
            .expect("coalesced notifications")
    );

    let producer = notifier.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        producer.notify().expect("remote notification");
    });
    assert!(parker.wait(Duration::from_secs(2)).expect("remote wakeup"));
    thread.join().expect("notification thread");
    assert!(!parker.wait(Duration::ZERO).expect("consumed notification"));

    let (commands, receiver) = sync_channel(1);
    let producer = notifier.clone();
    let thread = std::thread::spawn(move || {
        for _ in 0..256 {
            receiver.recv().expect("notification command");
            producer.notify().expect("racing notification");
        }
    });
    for _ in 0..256 {
        commands.send(()).expect("wake command");
        assert!(parker.wait(Duration::from_secs(2)).expect("racing wait"));
        assert!(!parker.wait(Duration::ZERO).expect("single consume"));
    }
    thread.join().expect("racing producer");

    let mut threads = Vec::new();
    for _ in 0..4 {
        let producer = notifier.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..4096 {
                producer.notify().expect("concurrent notification");
            }
        }));
    }
    for thread in threads {
        thread.join().expect("concurrent producer");
    }
    assert!(parker.wait(Duration::ZERO).expect("producer fan-in"));
    assert!(!parker.wait(Duration::ZERO).expect("fan-in consumed"));

    notifier.notify().expect("before waiter transfer");
    let mut parker = std::thread::spawn(move || {
        assert!(parker.wait(Duration::from_secs(2)).expect("moved waiter"));
        parker
    })
    .join()
    .expect("waiter transfer");
    assert!(!parker.wait(Duration::ZERO).expect("returned waiter"));
    drop(parker);
    notifier.notify().expect("notifier keeps endpoint alive");
    let remaining = notifier.clone();
    drop(notifier);
    std::thread::spawn(move || remaining.notify().expect("last notifier owns endpoint"))
        .join()
        .expect("last notifier thread");

    for _ in 0..128 {
        let mut parker = WorkerParker::new().expect("repeated creation");
        let notifier = parker.notifier();
        notifier.notify().expect("before producer drop");
        drop(notifier);
        assert!(parker.wait(Duration::ZERO).expect("waiter owns endpoint"));
    }
    println!(
        "Worker wakeup verified: timeouts, coalescing, early notification, races and endpoint lifetime"
    );
}
