use std::path::Path;
use std::time::Duration;

use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }
        .expect("source record fixture");
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).expect("tenant");
    let mut worker = ScalarWorker::new(&tenant, 2).expect("worker");
    let baseline = vm.used();
    let first = worker
        .spawn_with_heap(&module, limits(), 2, 2)
        .expect("first heap actor");
    let second = worker
        .spawn_with_heap(&module, limits(), 2, 2)
        .expect("second heap actor");
    let work = module.handler("work").expect("work");
    let request = worker
        .request(first, &work, &[11], 128, Duration::from_secs(60))
        .expect("request first");
    let other = worker
        .request(second, &work, &[22], 128, Duration::from_secs(60))
        .expect("request second");
    let mut deliveries = 0;
    for _ in 0..128 {
        match worker.tick(1).expect("tick") {
            WorkerEvent::Suspended(_) => {}
            WorkerEvent::Delivered(_) => deliveries += 1,
            event => panic!("unexpected event {event:?}"),
        }
        if deliveries == 2 {
            break;
        }
    }
    assert_eq!(deliveries, 2);
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Reply(11))
    );
    assert_eq!(
        worker.take_reply(other).unwrap(),
        Some(RequestOutcome::Reply(22))
    );
    let full = worker
        .request(
            first,
            &module.handler("full").expect("full"),
            &[],
            128,
            Duration::from_secs(60),
        )
        .expect("full request");
    assert!(
        matches!(worker.tick(128).expect("heap exhaustion"), WorkerEvent::Exited { pid, reason } if pid == first && reason.contains("heap exhausted"))
    );
    assert_eq!(
        worker.take_reply(full).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(worker.committed_state(second).unwrap(), 22);
    let pending = worker
        .request(second, &work, &[44], 128, Duration::from_secs(60))
        .expect("pending");
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(second));
    worker.stop(second).expect("stop suspended heap actor");
    assert_eq!(
        worker.take_reply(pending).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(vm.used(), baseline);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Language records verified: source AOT, isolated worker heaps, request/reply, GC, failures and stop cleanup"
    );
}
