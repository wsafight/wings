use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, RequestOutcome, ResourceAccount,
    Resources, ScalarModule, ScalarWorker, StackPool, WorkerEvent,
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
        .expect("record state fixture");
    assert!(
        module
            .instantiate()
            .err()
            .unwrap()
            .contains("accounted heap")
    );
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let account = tenant.process(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    assert_eq!(process.invoke("get", &[]).unwrap(), Some(7));
    for value in 0..128 {
        let mut execution = process
            .start_controlled(
                "replace",
                &[value],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        let mut complete = false;
        for _ in 0..128 {
            match execution.resume(1).unwrap() {
                ExecutionStatus::Suspended => {}
                ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
                ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
                ExecutionStatus::CompleteString(value) => {
                    panic!("unexpected string reply: {value:?}")
                }
                ExecutionStatus::CompleteRecord(value) => {
                    panic!("unexpected record reply: {value:?}")
                }
                ExecutionStatus::Complete(reply) => {
                    assert_eq!(reply, None);
                    complete = true;
                    break;
                }
            }
        }
        assert!(complete);
        drop(execution);
        assert_eq!(process.invoke("get", &[]).unwrap(), Some(value));
        assert_eq!(process.heap_stats().unwrap().allocated, 1);
    }
    drop(process);
    assert_eq!(vm.used(), baseline);
    let mut single = module.instantiate_with_heap(&bound, &account, 1).unwrap();
    assert_eq!(single.invoke("get", &[]).unwrap(), Some(7));
    assert!(
        single
            .invoke("replace", &[9])
            .unwrap_err()
            .contains("heap exhausted")
    );
    assert_eq!(single.heap_stats().unwrap().allocated, 1);
    drop(single);
    assert_eq!(vm.used(), baseline);
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    let mut execution = process
        .start_controlled(
            "set",
            &[10],
            128,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    for _ in 0..5 {
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    }
    drop(execution);
    assert!(
        process
            .invoke("get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    drop(process);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());

    let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
    let baseline = vm.used();
    assert!(worker.spawn(&module, limits(), 2).is_err());
    assert_eq!(vm.used(), baseline);
    let first = worker.spawn_with_heap(&module, limits(), 2, 2).unwrap();
    let second = worker.spawn_with_heap(&module, limits(), 2, 2).unwrap();
    assert!(
        worker
            .committed_state(first)
            .unwrap_err()
            .contains("not exposed")
    );
    let set = module.handler("set").unwrap();
    let first_reply = worker
        .request(first, &set, &[11], 128, Duration::from_secs(60))
        .unwrap();
    let second_reply = worker
        .request(second, &set, &[22], 128, Duration::from_secs(60))
        .unwrap();
    let mut delivered = 0;
    for _ in 0..128 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(_) => {}
            WorkerEvent::Delivered(_) => delivered += 1,
            event => panic!("unexpected event {event:?}"),
        }
        if delivered == 2 {
            break;
        }
    }
    assert_eq!(delivered, 2);
    assert_eq!(
        worker.take_reply(first_reply).unwrap(),
        Some(RequestOutcome::Reply(11))
    );
    assert_eq!(
        worker.take_reply(second_reply).unwrap(),
        Some(RequestOutcome::Reply(22))
    );
    let fail = worker
        .request(
            first,
            &module.handler("fail").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        matches!(worker.tick(128).unwrap(), WorkerEvent::Exited { pid, reason } if pid == first && reason.contains("division by zero"))
    );
    assert_eq!(
        worker.take_reply(fail).unwrap(),
        Some(RequestOutcome::Failed)
    );
    let get = worker
        .request(
            second,
            &module.handler("get").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Delivered(second));
    assert_eq!(
        worker.take_reply(get).unwrap(),
        Some(RequestOutcome::Reply(22))
    );
    worker
        .send(second, &module.handler("replace").unwrap(), &[33], 128)
        .unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(second));
    worker.stop(second).unwrap();
    assert_eq!(vm.used(), baseline);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Persistent records verified: committed/candidate roots, repeated GC, final checkpoint, isolated workers, failures and cleanup"
    );
}
