use std::path::Path;

use wings_vm::{
    HandlerId, Pid, ResourceAccount, Resources, ScalarModule, ScalarWorker, WorkerEvent,
};

const MAX_STEPS: u64 = 128;
const MAX_TICKS: usize = 128;

fn limits() -> Resources {
    Resources {
        bytes: 8 * 1024 * 1024,
        processes: 8,
        messages: 64,
        requests: 0,
    }
}

fn accounts() -> (ResourceAccount, ResourceAccount) {
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).expect("tenant");
    (vm, tenant)
}

fn assert_empty(vm: &ResourceAccount, tenant: &ResourceAccount) {
    assert_eq!(tenant.used(), Resources::default(), "tenant leak");
    assert_eq!(vm.used(), Resources::default(), "VM leak");
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    count: usize,
    actors: Vec<(usize, i64, Resources)>,
    tenant: Resources,
    vm: Resources,
}

fn snapshot(
    worker: &ScalarWorker,
    pids: &[Pid],
    vm: &ResourceAccount,
    tenant: &ResourceAccount,
) -> Snapshot {
    Snapshot {
        count: worker.process_count(),
        actors: pids
            .iter()
            .map(|&pid| {
                (
                    worker.queued(pid).expect("queue"),
                    worker.committed_state(pid).expect("state"),
                    worker.resources(pid).expect("resources"),
                )
            })
            .collect(),
        tenant: tenant.used(),
        vm: vm.used(),
    }
}

fn finish(worker: &mut ScalarWorker, pid: Pid, previous: i64, expected: i64) {
    for _ in 0..MAX_TICKS {
        match worker.tick(1).expect("tick") {
            WorkerEvent::Suspended(actual) => {
                assert_eq!(actual, pid);
                assert_eq!(worker.committed_state(pid), Ok(previous));
            }
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                assert_eq!(worker.committed_state(pid), Ok(expected));
                return;
            }
            event => panic!("unexpected event while delivering: {event:?}"),
        }
    }
    panic!("message did not complete within bounded ticks");
}

fn assert_invalid(worker: &mut ScalarWorker, pid: Pid, add: &HandlerId) {
    assert!(worker.send(pid, add, &[1], MAX_STEPS).is_err());
    assert!(worker.stop(pid).is_err());
    assert!(worker.queued(pid).is_err());
    assert!(worker.resources(pid).is_err());
    assert!(worker.committed_state(pid).is_err());
}

fn verify_fifo(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 2).expect("worker");
    let empty = tenant.used();
    let add = module.handler("Add").expect("Add");
    let set = module.handler("Set").expect("Set");
    let first = worker.spawn(module, limits(), 3).expect("first actor");
    let second = worker.spawn(module, limits(), 3).expect("second actor");
    let pids = [first, second];
    let baseline = pids.map(|pid| worker.resources(pid).expect("baseline"));
    let mut arguments = [10];
    worker
        .send(first, &set, &arguments, MAX_STEPS)
        .expect("Set");
    arguments[0] = 999;
    worker.send(first, &add, &[3], MAX_STEPS).expect("Add");
    worker.send(first, &set, &[7], MAX_STEPS).expect("Set");
    worker.send(second, &add, &[2], MAX_STEPS).expect("Add");
    worker.send(second, &set, &[20], MAX_STEPS).expect("Set");
    worker.send(second, &add, &[-4], MAX_STEPS).expect("Add");
    assert_eq!(arguments, [999]);
    assert_eq!(tenant.used().messages, 6);
    let expected = [[10, 13, 7], [2, 20, 16]];
    let mut delivered = [0; 2];
    let mut committed = [0; 2];
    let mut suspended = [false; 2];
    let mut complete = false;
    let mut previous = None;
    for _ in 0..MAX_TICKS {
        let both_runnable = delivered.iter().all(|&count| count < 3);
        let event = worker.tick(1).expect("round-robin tick");
        let actual = match event {
            WorkerEvent::Suspended(pid) | WorkerEvent::Delivered(pid) => pid,
            event => panic!("unexpected FIFO event: {event:?}"),
        };
        let actor = pids
            .iter()
            .position(|&pid| pid == actual)
            .expect("known actor");
        if both_runnable {
            assert_ne!(previous, Some(actor), "runnable actor must get its turn");
        }
        previous = Some(actor);
        let pid = pids[actor];
        match event {
            WorkerEvent::Suspended(actual) => {
                assert_eq!(actual, pid, "round-robin suspension");
                suspended[actor] = true;
                assert_eq!(worker.committed_state(pid), Ok(committed[actor]));
                assert_eq!(worker.queued(pid), Ok(2 - delivered[actor]));
                assert_eq!(
                    worker.resources(pid).expect("active charge").messages,
                    (3 - delivered[actor]) as u64
                );
            }
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid, "round-robin delivery");
                committed[actor] = expected[actor][delivered[actor]];
                delivered[actor] += 1;
                assert_eq!(worker.committed_state(pid), Ok(committed[actor]));
                assert_eq!(worker.queued(pid), Ok(3 - delivered[actor]));
                assert_eq!(
                    worker.resources(pid).expect("message release").messages,
                    (3 - delivered[actor]) as u64
                );
            }
            event => panic!("unexpected FIFO event: {event:?}"),
        }
        for (other, &other_pid) in pids.iter().enumerate() {
            assert_eq!(worker.committed_state(other_pid), Ok(committed[other]));
        }
        assert_eq!(vm.used(), tenant.used());
        assert_eq!(
            tenant.used().messages,
            (6 - delivered.iter().sum::<usize>()) as u64
        );
        if delivered == [3, 3] {
            complete = true;
            break;
        }
    }
    assert!(complete, "FIFO did not drain within bounded ticks");
    assert_eq!(suspended, [true, true]);
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
    for (index, pid) in pids.into_iter().enumerate() {
        assert_eq!(worker.resources(pid), Ok(baseline[index]));
        worker.stop(pid).expect("stop drained actor");
    }
    assert_eq!(worker.process_count(), 0);
    assert_eq!(tenant.used(), empty);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_rejections(module: &ScalarModule, foreign: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("worker");
    let pid = worker.spawn(module, limits(), 1).expect("actor");
    let add = module.handler("Add").expect("Add");
    let get = module.handler("Get").expect("Get");
    let foreign_add = foreign.handler("Add").expect("foreign Add");
    for (handler, values, reason) in [
        (&add, &[][..], "argument"),
        (&add, &[1, 2][..], "argument"),
        (&get, &[][..], "one-way"),
        (&foreign_add, &[1][..], "different module"),
    ] {
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(
            worker
                .send(pid, handler, values, MAX_STEPS)
                .unwrap_err()
                .contains(reason)
        );
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    worker.send(pid, &add, &[5], MAX_STEPS).expect("fill queue");
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert_eq!(
        worker.send(pid, &add, &[9], MAX_STEPS),
        Err("mailbox_full".into())
    );
    assert!(worker.tick(0).is_err());
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(pid)));
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert!(worker.tick(0).is_err());
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    finish(&mut worker, pid, 0, 5);
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_stop_and_pids(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("worker");
    let mut other = ScalarWorker::new(&tenant, 1).expect("other worker");
    let empty = tenant.used();
    let add = module.handler("Add").expect("Add");
    let old = worker.spawn(module, limits(), 1).expect("actor");
    let outsider = other.spawn(module, limits(), 1).expect("outsider");
    worker.send(old, &add, &[1], MAX_STEPS).expect("start");
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(old)));
    worker
        .send(old, &add, &[2], MAX_STEPS)
        .expect("refill while suspended");
    assert_eq!(worker.queued(old), Ok(1));
    assert_eq!(worker.resources(old).expect("two messages").messages, 2);
    let before = snapshot(&worker, &[old], &vm, &tenant);
    assert_eq!(
        worker.send(old, &add, &[3], MAX_STEPS),
        Err("mailbox_full".into())
    );
    assert_invalid(&mut worker, outsider, &add);
    assert_eq!(snapshot(&worker, &[old], &vm, &tenant), before);
    assert_eq!(other.committed_state(outsider), Ok(0));
    worker.stop(old).expect("stop suspended with full mailbox");
    assert_eq!(worker.process_count(), 0);
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
    let replacement = worker.spawn(module, limits(), 1).expect("reuse slot");
    assert_ne!(old, replacement);
    let before = snapshot(&worker, &[replacement], &vm, &tenant);
    assert_invalid(&mut worker, old, &add);
    assert_invalid(&mut other, replacement, &add);
    assert_eq!(snapshot(&worker, &[replacement], &vm, &tenant), before);
    worker
        .send(replacement, &add, &[4], MAX_STEPS)
        .expect("new generation");
    finish(&mut worker, replacement, 0, 4);
    worker.stop(replacement).expect("stop replacement");
    other.stop(outsider).expect("stop outsider");
    assert_eq!(tenant.used(), empty);
    drop(worker);
    drop(other);
    assert_empty(&vm, &tenant);
}

fn verify_failure_and_drop(module: &ScalarModule) {
    let add = module.handler("Add").expect("Add");
    let fail = module.handler("Fail").expect("Fail");
    for (handler, arguments, budget, reason) in [
        (&fail, &[][..], MAX_STEPS, "division by zero"),
        (&add, &[7][..], 0, "budget"),
    ] {
        let (vm, tenant) = accounts();
        let mut worker = ScalarWorker::new(&tenant, 1).expect("worker");
        let empty = tenant.used();
        let pid = worker.spawn(module, limits(), 2).expect("actor");
        worker.send(pid, &add, &[5], MAX_STEPS).expect("seed state");
        finish(&mut worker, pid, 0, 5);
        worker
            .send(pid, handler, arguments, budget)
            .expect("failing message");
        worker
            .send(pid, &add, &[1], MAX_STEPS)
            .expect("discarded message");
        let mut exited = false;
        for _ in 0..MAX_TICKS {
            match worker.tick(1).expect("failure tick") {
                WorkerEvent::Suspended(actual) => {
                    assert_eq!(actual, pid);
                    assert_eq!(worker.committed_state(pid), Ok(5));
                    assert_eq!(
                        worker.resources(pid).expect("in-flight messages").messages,
                        2
                    );
                }
                WorkerEvent::Exited {
                    pid: actual,
                    reason: actual_reason,
                } => {
                    assert_eq!(actual, pid);
                    assert!(
                        actual_reason.contains(reason),
                        "unexpected exit reason: {actual_reason}"
                    );
                    exited = true;
                    break;
                }
                event => panic!("unexpected failure event: {event:?}"),
            }
        }
        assert!(exited, "failure did not exit within bounded ticks");
        assert_eq!(worker.process_count(), 0);
        assert_eq!(tenant.used(), empty);
        assert_eq!(vm.used(), empty);
        assert_invalid(&mut worker, pid, &add);
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
        let replacement = worker.spawn(module, limits(), 1).expect("spawn after exit");
        assert_ne!(replacement, pid);
        worker
            .send(replacement, &add, &[3], MAX_STEPS)
            .expect("recovered stack");
        finish(&mut worker, replacement, 0, 3);
        drop(worker);
        assert_empty(&vm, &tenant);
    }
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 2).expect("drop worker");
    let first = worker.spawn(module, limits(), 1).expect("first");
    let second = worker.spawn(module, limits(), 1).expect("second");
    for pid in [first, second] {
        worker
            .send(pid, &add, &[1], MAX_STEPS)
            .expect("queued message");
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(pid)));
        worker
            .send(pid, &add, &[2], MAX_STEPS)
            .expect("pending message");
    }
    assert_eq!(tenant.used().messages, 4);
    assert_eq!(tenant.used().processes, 2);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_limits(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 2).expect("limits worker");
    let empty = tenant.used();
    let add = module.handler("Add").expect("Add");

    let before = snapshot(&worker, &[], &vm, &tenant);
    assert!(
        worker
            .spawn(
                module,
                Resources {
                    processes: 0,
                    ..limits()
                },
                1,
            )
            .is_err()
    );
    assert_eq!(snapshot(&worker, &[], &vm, &tenant), before);

    let no_messages = worker
        .spawn(
            module,
            Resources {
                messages: 0,
                ..limits()
            },
            1,
        )
        .expect("actor with no message allowance");
    let before = snapshot(&worker, &[no_messages], &vm, &tenant);
    assert!(worker.send(no_messages, &add, &[1], MAX_STEPS).is_err());
    assert_eq!(snapshot(&worker, &[no_messages], &vm, &tenant), before);
    worker
        .stop(no_messages)
        .expect("stop message-limited actor");
    assert_eq!(tenant.used(), empty);

    let small_stack = worker
        .spawn(
            module,
            Resources {
                bytes: 64 * 1024,
                ..limits()
            },
            1,
        )
        .expect("64 KiB can hold actor and mailbox");
    let healthy = worker.spawn(module, limits(), 1).expect("healthy actor");
    worker
        .send(small_stack, &add, &[3], MAX_STEPS)
        .expect("queue within small account");
    worker
        .send(healthy, &add, &[7], MAX_STEPS)
        .expect("queue healthy actor");
    let before = snapshot(&worker, &[small_stack, healthy], &vm, &tenant);
    assert!(worker.tick(1).is_err(), "stack transfer must exceed 64 KiB");
    assert_eq!(
        snapshot(&worker, &[small_stack, healthy], &vm, &tenant),
        before
    );
    assert_eq!(worker.queued(small_stack), Ok(1));
    assert_eq!(worker.tick(MAX_STEPS), Ok(WorkerEvent::Delivered(healthy)));
    assert_eq!(worker.committed_state(healthy), Ok(7));
    assert_eq!(worker.queued(healthy), Ok(0));
    assert_eq!(
        worker.resources(healthy).expect("healthy charge").messages,
        0
    );
    assert_eq!(worker.committed_state(small_stack), Ok(0));
    assert_eq!(worker.queued(small_stack), Ok(1));
    assert_eq!(worker.resources(small_stack), Ok(before.actors[0].2));
    assert_eq!(tenant.used().messages, 1);
    assert_eq!(vm.used(), tenant.used());
    worker.stop(small_stack).expect("stop stack-limited actor");
    worker.stop(healthy).expect("stop healthy actor");
    assert_eq!(worker.process_count(), 0);
    assert_eq!(tenant.used(), empty);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(
        arguments.len() == 2 && arguments[0] == "--trusted",
        "usage: scalar_worker --trusted <Queue artifact>"
    );
    let directory = Path::new(&arguments[1]);
    let module = unsafe { ScalarModule::load_trusted(directory) }.expect("trusted Queue artifact");
    let foreign =
        unsafe { ScalarModule::load_trusted(directory) }.expect("separately loaded module");
    verify_fifo(&module);
    verify_rejections(&module, &foreign);
    verify_stop_and_pids(&module);
    verify_failure_and_drop(&module);
    verify_limits(&module);
    println!("scalar worker regression passed");
}
