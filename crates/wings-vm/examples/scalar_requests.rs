use std::path::Path;
use std::time::Duration;

use wings_abi::ValueType;
use wings_vm::{
    Pid, RequestId, RequestOutcome, ResourceAccount, ResourceLevel, Resources, ScalarModule,
    ScalarWorker, WorkerEvent,
};

const MAX_STEPS: u64 = 128;
const MAX_TICKS: usize = 128;
const TIMEOUT: Duration = Duration::from_secs(60);

fn limits() -> Resources {
    Resources {
        bytes: 8 * 1024 * 1024,
        processes: 8,
        messages: 64,
        requests: 64,
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

fn deliver(worker: &mut ScalarWorker, pid: Pid, previous: i64, expected: i64) {
    for _ in 0..MAX_TICKS {
        match worker.tick(1).expect("delivery tick") {
            WorkerEvent::Suspended(actual) => {
                assert_eq!(actual, pid);
                assert_eq!(worker.committed_state(pid), Ok(previous));
            }
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                assert_eq!(worker.committed_state(pid), Ok(expected));
                return;
            }
            event => panic!("unexpected delivery event: {event:?}"),
        }
    }
    panic!("request did not complete within bounded ticks");
}

fn assert_stale(worker: &mut ScalarWorker, id: RequestId) {
    assert!(worker.take_reply(id).is_err());
    assert!(worker.cancel_request(id).is_err());
}

fn verify_schema(module: &ScalarModule) {
    let manifest = module.manifest();
    assert_eq!(manifest.process, "Queue");
    assert_eq!(manifest.state, ValueType::Int);
    for (name, parameter, reply) in [
        ("Add", true, None),
        ("Set", true, None),
        ("Fail", false, None),
        ("Get", false, Some(ValueType::Int)),
        ("Bump", true, Some(ValueType::Int)),
        ("Ack", false, Some(ValueType::Unit)),
        ("FailRequest", false, Some(ValueType::Int)),
    ] {
        let handler = module.handler(name).expect("expected Queue handler");
        let expected = if parameter {
            vec![("value".to_string(), ValueType::Int)]
        } else {
            Vec::new()
        };
        assert_eq!(handler.schema().parameters, expected, "{name} parameters");
        assert_eq!(handler.schema().reply, reply, "{name} reply");
    }
}

fn verify_construction() {
    let (vm, tenant) = accounts();
    let process = tenant.process(limits()).expect("process account");
    for capacity in [0, 257] {
        assert!(ScalarWorker::new(&tenant, capacity).is_err());
        assert!(ScalarWorker::with_request_capacity(&tenant, capacity, 1).is_err());
        assert_empty(&vm, &tenant);
    }
    for pending in [0, 257] {
        assert!(ScalarWorker::with_request_capacity(&tenant, 1, pending).is_err());
        assert_empty(&vm, &tenant);
    }
    for invalid in [&vm, &process] {
        assert!(ScalarWorker::new(invalid, 1).is_err());
        assert!(ScalarWorker::with_request_capacity(invalid, 1, 1).is_err());
        assert_empty(&vm, &tenant);
    }
    for (capacity, pending) in [(1, 1), (8, 256)] {
        let mut worker = ScalarWorker::with_request_capacity(&tenant, capacity, pending)
            .expect("valid bounded capacities");
        assert_eq!(worker.process_count(), 0);
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
        assert_eq!(vm.used(), tenant.used());
        assert!(tenant.used().bytes > 0);
        drop(worker);
        assert_empty(&vm, &tenant);
    }
}

fn verify_fifo(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 4).expect("FIFO worker");
    let empty = tenant.used();
    let pid = worker.spawn(module, limits(), 6).expect("actor");
    let baseline = worker.resources(pid).expect("baseline");
    let add = module.handler("Add").expect("Add");
    let set = module.handler("Set").expect("Set");
    let bump = module.handler("Bump").expect("Bump");
    let get = module.handler("Get").expect("Get");
    let ack = module.handler("Ack").expect("Ack");
    worker.send(pid, &add, &[2], MAX_STEPS).expect("Add");
    let mut arguments = [3];
    let first = worker
        .request(pid, &bump, &arguments, MAX_STEPS, TIMEOUT)
        .expect("Bump");
    arguments[0] = 999;
    worker.send(pid, &set, &[9], MAX_STEPS).expect("Set");
    let second = worker
        .request(pid, &get, &[], MAX_STEPS, TIMEOUT)
        .expect("Get");
    let unit = worker
        .request(pid, &ack, &[], MAX_STEPS, TIMEOUT)
        .expect("Ack");
    let last = worker
        .request(pid, &bump, &[-2], MAX_STEPS, TIMEOUT)
        .expect("last Bump");
    assert_eq!(arguments, [999]);
    assert_eq!(worker.queued(pid), Ok(6));
    assert_eq!(worker.committed_state(pid), Ok(0));
    assert_eq!(tenant.used().requests, 4);
    assert_eq!(tenant.used().messages, 6);
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    for id in [first, second, unit, last] {
        assert_eq!(worker.take_reply(id), Ok(None));
    }
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    for (index, (previous, expected, reply)) in [
        (0, 2, None),
        (2, 5, Some((first, 5))),
        (5, 9, None),
        (9, 9, Some((second, 9))),
        (9, 9, Some((unit, 0))),
        (9, 7, Some((last, 7))),
    ]
    .into_iter()
    .enumerate()
    {
        deliver(&mut worker, pid, previous, expected);
        assert_eq!(worker.queued(pid), Ok(5 - index));
        assert_eq!(
            worker.resources(pid).expect("message charge").messages,
            (5 - index) as u64
        );
        if let Some((id, value)) = reply {
            assert_eq!(
                worker.take_reply(id),
                Ok(Some(RequestOutcome::Reply(value)))
            );
            assert_stale(&mut worker, id);
        }
        assert_eq!(vm.used(), tenant.used());
    }
    assert_eq!(worker.resources(pid), Ok(baseline));
    for previous in 7..10 {
        let id = worker
            .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
            .expect("sequential request");
        deliver(&mut worker, pid, previous, previous + 1);
        assert_eq!(
            worker.take_reply(id),
            Ok(Some(RequestOutcome::Reply(previous + 1)))
        );
        assert_eq!(worker.resources(pid), Ok(baseline));
    }
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
    worker.stop(pid).expect("stop FIFO actor");
    assert_eq!(tenant.used(), empty);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_rejections(module: &ScalarModule, foreign: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("rejection worker");
    let pid = worker.spawn(module, limits(), 1).expect("actor");
    for (name, values) in [("Add", &[1][..]), ("Set", &[1][..]), ("Fail", &[][..])] {
        let handler = module.handler(name).expect("one-way handler");
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(
            worker
                .request(pid, &handler, values, MAX_STEPS, TIMEOUT)
                .is_err()
        );
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    for (name, values) in [
        ("Bump", &[1][..]),
        ("Get", &[][..]),
        ("Ack", &[][..]),
        ("FailRequest", &[][..]),
    ] {
        let handler = module.handler(name).expect("request handler");
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(worker.send(pid, &handler, values, MAX_STEPS).is_err());
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    let bump = module.handler("Bump").expect("Bump");
    for values in [&[][..], &[1, 2][..], &[0; 65][..]] {
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(
            worker
                .request(pid, &bump, values, MAX_STEPS, TIMEOUT)
                .is_err()
        );
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    for name in ["Get", "Ack", "FailRequest"] {
        let handler = module.handler(name).expect("zero-argument request");
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(
            worker
                .request(pid, &handler, &[1], MAX_STEPS, TIMEOUT)
                .is_err()
        );
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    let foreign_bump = foreign.handler("Bump").expect("foreign Bump");
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert!(
        worker
            .request(pid, &foreign_bump, &[1], MAX_STEPS, TIMEOUT)
            .is_err()
    );
    assert!(
        worker
            .request(pid, &bump, &[1], MAX_STEPS, Duration::MAX)
            .is_err()
    );
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    let add = module.handler("Add").expect("Add");
    worker
        .send(pid, &add, &[2], MAX_STEPS)
        .expect("fill mailbox");
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert!(
        worker
            .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
            .is_err()
    );
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    deliver(&mut worker, pid, 0, 2);
    let id = worker
        .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
        .expect("request after rejection");
    deliver(&mut worker, pid, 2, 3);
    assert_eq!(worker.take_reply(id), Ok(Some(RequestOutcome::Reply(3))));
    worker.stop(pid).expect("stop actor");
    let before = snapshot(&worker, &[], &vm, &tenant);
    assert!(
        worker
            .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
            .is_err()
    );
    assert_eq!(snapshot(&worker, &[], &vm, &tenant), before);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_pending_capacity(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("default pending capacity");
    let pid = worker.spawn(module, limits(), 2).expect("actor");
    let bump = module.handler("Bump").expect("Bump");
    let first = worker
        .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
        .expect("first request");
    for completed in [false, true] {
        if completed {
            deliver(&mut worker, pid, 0, 1);
            assert_eq!(worker.queued(pid), Ok(0));
            assert_eq!(tenant.used().messages, 0);
            let before = snapshot(&worker, &[pid], &vm, &tenant);
            worker
                .cancel_request(first)
                .expect("completed cancel is a no-op");
            assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
        }
        assert_eq!(tenant.used().requests, 1);
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(
            worker
                .request(pid, &bump, &[2], MAX_STEPS, TIMEOUT)
                .is_err()
        );
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    }
    assert_eq!(worker.take_reply(first), Ok(Some(RequestOutcome::Reply(1))));
    assert_eq!(tenant.used().requests, 0);
    let next = worker
        .request(pid, &bump, &[2], MAX_STEPS, TIMEOUT)
        .expect("reuse pending slot");
    assert!(first != next);
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert_stale(&mut worker, first);
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    assert_eq!(worker.take_reply(next), Ok(None));
    deliver(&mut worker, pid, 1, 3);
    assert_eq!(worker.take_reply(next), Ok(Some(RequestOutcome::Reply(3))));
    assert_stale(&mut worker, next);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_limits(module: &ScalarModule) {
    let bump = module.handler("Bump").expect("Bump");
    for level in [
        ResourceLevel::Vm,
        ResourceLevel::Tenant,
        ResourceLevel::Process,
    ] {
        for deny_requests in [false, true] {
            let mut denied = limits();
            if deny_requests {
                denied.requests = 0;
            } else {
                denied.messages = 0;
            }
            let vm = ResourceAccount::vm(if level == ResourceLevel::Vm {
                denied
            } else {
                limits()
            });
            let tenant = vm
                .tenant(if level == ResourceLevel::Tenant {
                    denied
                } else {
                    limits()
                })
                .expect("limited tenant");
            let mut worker =
                ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("limited worker");
            let empty = tenant.used();
            let pid = worker
                .spawn(
                    module,
                    if level == ResourceLevel::Process {
                        denied
                    } else {
                        limits()
                    },
                    1,
                )
                .expect("limited actor");
            let before = snapshot(&worker, &[pid], &vm, &tenant);
            for _ in 0..2 {
                assert!(
                    worker
                        .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
                        .is_err(),
                    "{level:?}, requests limited: {deny_requests}"
                );
                assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
            }
            assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
            worker.stop(pid).expect("stop limited actor");
            assert_eq!(tenant.used(), empty);
            drop(worker);
            assert_empty(&vm, &tenant);
        }
    }
}

fn verify_zero_timeout(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("timeout worker");
    let pid = worker.spawn(module, limits(), 2).expect("actor");
    let bump = module.handler("Bump").expect("Bump");
    let expired = worker
        .request(pid, &bump, &[4], MAX_STEPS, Duration::ZERO)
        .expect("zero timeout request");
    assert_eq!(
        worker.take_reply(expired),
        Ok(Some(RequestOutcome::TimedOut))
    );
    assert_eq!(worker.committed_state(pid), Ok(0));
    assert_eq!(worker.queued(pid), Ok(1));
    assert_eq!(tenant.used().messages, 1);
    assert_eq!(tenant.used().requests, 0);
    let next = worker
        .request(pid, &bump, &[6], MAX_STEPS, TIMEOUT)
        .expect("reuse timed-out slot");
    assert!(next != expired);
    assert_stale(&mut worker, expired);
    deliver(&mut worker, pid, 0, 4);
    assert_eq!(
        worker.take_reply(next),
        Ok(None),
        "late reply must not overwrite reused slot"
    );
    deliver(&mut worker, pid, 4, 10);
    assert_eq!(worker.take_reply(next), Ok(Some(RequestOutcome::Reply(10))));
    let held = worker
        .request(pid, &bump, &[1], MAX_STEPS, Duration::ZERO)
        .expect("held timeout");
    deliver(&mut worker, pid, 10, 11);
    worker
        .cancel_request(held)
        .expect("timeout takes precedence over cancellation");
    assert_eq!(worker.take_reply(held), Ok(Some(RequestOutcome::TimedOut)));
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_suspended_terminal(module: &ScalarModule) {
    let bump = module.handler("Bump").expect("Bump");
    for cancel in [false, true] {
        let (vm, tenant) = accounts();
        let mut worker =
            ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("suspended worker");
        let pid = worker.spawn(module, limits(), 1).expect("actor");
        let timeout = if cancel {
            TIMEOUT
        } else {
            Duration::from_millis(1)
        };
        let first = worker
            .request(pid, &bump, &[5], MAX_STEPS, timeout)
            .expect("suspended request");
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(pid)));
        assert_eq!(worker.committed_state(pid), Ok(0));
        assert_eq!(worker.queued(pid), Ok(0));
        let outcome = if cancel {
            worker
                .cancel_request(first)
                .expect("cancel suspended request");
            RequestOutcome::Cancelled
        } else {
            std::thread::sleep(Duration::from_millis(5));
            RequestOutcome::TimedOut
        };
        assert_eq!(worker.take_reply(first), Ok(Some(outcome)));
        assert_eq!(tenant.used().requests, 0);
        assert_eq!(tenant.used().messages, 1);
        assert_eq!(worker.committed_state(pid), Ok(0));
        let next = worker
            .request(pid, &bump, &[2], MAX_STEPS, TIMEOUT)
            .expect("reuse while old call is suspended");
        assert!(first != next);
        assert_stale(&mut worker, first);
        deliver(&mut worker, pid, 0, 5);
        assert_eq!(
            worker.take_reply(next),
            Ok(None),
            "late terminal reply must not finish next request"
        );
        deliver(&mut worker, pid, 5, 7);
        assert_eq!(worker.take_reply(next), Ok(Some(RequestOutcome::Reply(7))));
        drop(worker);
        assert_empty(&vm, &tenant);
    }
}

fn verify_worker_ids(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut first = ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("first worker");
    let mut second = ScalarWorker::with_request_capacity(&tenant, 1, 1).expect("second worker");
    let first_pid = first.spawn(module, limits(), 1).expect("first actor");
    let second_pid = second.spawn(module, limits(), 1).expect("second actor");
    let get = module.handler("Get").expect("Get");
    let first_id = first
        .request(first_pid, &get, &[], MAX_STEPS, TIMEOUT)
        .expect("first request");
    let second_id = second
        .request(second_pid, &get, &[], MAX_STEPS, TIMEOUT)
        .expect("second request");
    assert!(first_id != second_id);
    let first_before = snapshot(&first, &[first_pid], &vm, &tenant);
    let second_before = snapshot(&second, &[second_pid], &vm, &tenant);
    assert_stale(&mut first, second_id);
    assert_stale(&mut second, first_id);
    assert!(
        first
            .request(second_pid, &get, &[], MAX_STEPS, TIMEOUT)
            .is_err()
    );
    assert!(
        second
            .request(first_pid, &get, &[], MAX_STEPS, TIMEOUT)
            .is_err()
    );
    assert_eq!(snapshot(&first, &[first_pid], &vm, &tenant), first_before);
    assert_eq!(
        snapshot(&second, &[second_pid], &vm, &tenant),
        second_before
    );
    assert_eq!(first.take_reply(first_id), Ok(None));
    assert_eq!(second.take_reply(second_id), Ok(None));
    deliver(&mut first, first_pid, 0, 0);
    assert_eq!(
        first.take_reply(first_id),
        Ok(Some(RequestOutcome::Reply(0)))
    );
    assert_stale(&mut first, first_id);
    assert_eq!(second.take_reply(second_id), Ok(None));
    deliver(&mut second, second_pid, 0, 0);
    assert_eq!(
        second.take_reply(second_id),
        Ok(Some(RequestOutcome::Reply(0)))
    );
    drop(first);
    drop(second);
    assert_empty(&vm, &tenant);
}

fn verify_exit(module: &ScalarModule) {
    let bump = module.handler("Bump").expect("Bump");
    let get = module.handler("Get").expect("Get");
    let fail = module.handler("Fail").expect("Fail");
    let fail_request = module.handler("FailRequest").expect("FailRequest");
    for scenario in [
        "request failure",
        "budget failure",
        "one-way failure",
        "suspended stop",
    ] {
        let (vm, tenant) = accounts();
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 4).expect("exit worker");
        let empty = tenant.used();
        let pid = worker.spawn(module, limits(), 5).expect("actor");
        let seed = worker
            .request(pid, &bump, &[5], MAX_STEPS, TIMEOUT)
            .expect("seed state");
        deliver(&mut worker, pid, 0, 5);
        assert_eq!(worker.take_reply(seed), Ok(Some(RequestOutcome::Reply(5))));
        if scenario == "one-way failure" {
            worker
                .send(pid, &fail, &[], MAX_STEPS)
                .expect("queued failure");
        }
        let (handler, arguments) = if scenario == "request failure" {
            (&fail_request, &[][..])
        } else {
            (&bump, &[7][..])
        };
        let first = worker
            .request(
                pid,
                handler,
                arguments,
                if scenario == "budget failure" {
                    2
                } else {
                    MAX_STEPS
                },
                TIMEOUT,
            )
            .expect("first pending");
        let second = worker
            .request(pid, &get, &[], MAX_STEPS, TIMEOUT)
            .expect("queued request");
        let expired = worker
            .request(pid, &get, &[], MAX_STEPS, Duration::ZERO)
            .expect("queued timeout");
        let cancelled = worker
            .request(pid, &get, &[], MAX_STEPS, TIMEOUT)
            .expect("queued cancel");
        worker
            .cancel_request(cancelled)
            .expect("cancel queued request");
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(pid)));
        assert_eq!(worker.committed_state(pid), Ok(5));
        assert_eq!(worker.take_reply(first), Ok(None));
        if scenario == "suspended stop" {
            worker
                .stop(pid)
                .expect("stop with active and queued requests");
        } else {
            let mut exited = false;
            for _ in 0..MAX_TICKS {
                match worker.tick(1).expect("failure tick") {
                    WorkerEvent::Suspended(actual) => {
                        assert_eq!(actual, pid);
                        assert_eq!(worker.committed_state(pid), Ok(5));
                    }
                    WorkerEvent::Exited {
                        pid: actual,
                        reason,
                    } => {
                        assert_eq!(actual, pid);
                        let expected_reason = if scenario == "budget failure" {
                            "budget"
                        } else {
                            "division by zero"
                        };
                        assert!(
                            reason.contains(expected_reason),
                            "unexpected exit: {reason}"
                        );
                        exited = true;
                        break;
                    }
                    event => panic!("unexpected failure event: {event:?}"),
                }
            }
            assert!(exited, "failure did not exit within bounded ticks");
        }
        assert_eq!(worker.process_count(), 0);
        assert_eq!(worker.tick(1), Ok(WorkerEvent::Idle));
        assert_eq!(tenant.used().processes, 0);
        assert_eq!(tenant.used().messages, 0);
        assert_eq!(tenant.used().requests, 4);
        assert_eq!(vm.used(), tenant.used());
        let replacement = worker
            .spawn(module, limits(), 1)
            .expect("actor with held failed replies");
        let before = snapshot(&worker, &[replacement], &vm, &tenant);
        assert!(
            worker
                .request(replacement, &get, &[], MAX_STEPS, TIMEOUT)
                .is_err(),
            "failed results still occupy pending slots"
        );
        worker
            .cancel_request(first)
            .expect("failed cancel is a no-op");
        assert_eq!(snapshot(&worker, &[replacement], &vm, &tenant), before);
        worker.stop(replacement).expect("stop blocked replacement");
        for (index, (id, outcome)) in [
            (first, RequestOutcome::Failed),
            (second, RequestOutcome::Failed),
            (expired, RequestOutcome::TimedOut),
            (cancelled, RequestOutcome::Cancelled),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(worker.take_reply(id), Ok(Some(outcome)), "{scenario}");
            assert_stale(&mut worker, id);
            assert_eq!(tenant.used().requests, (3 - index) as u64);
        }
        assert_eq!(tenant.used(), empty);
        let replacement = worker.spawn(module, limits(), 1).expect("actor after exit");
        let id = worker
            .request(replacement, &get, &[], MAX_STEPS, TIMEOUT)
            .expect("request after exit");
        deliver(&mut worker, replacement, 0, 0);
        assert_eq!(worker.take_reply(id), Ok(Some(RequestOutcome::Reply(0))));
        drop(worker);
        assert_empty(&vm, &tenant);
    }
    let (vm, tenant) = accounts();
    let mut worker =
        ScalarWorker::with_request_capacity(&tenant, 1, 2).expect("queued stop worker");
    let empty = tenant.used();
    let pid = worker.spawn(module, limits(), 2).expect("actor");
    let first = worker
        .request(pid, &bump, &[1], MAX_STEPS, TIMEOUT)
        .expect("queued Bump");
    let second = worker
        .request(pid, &get, &[], MAX_STEPS, TIMEOUT)
        .expect("queued Get");
    worker.stop(pid).expect("stop before execution");
    assert_eq!(worker.take_reply(first), Ok(Some(RequestOutcome::Failed)));
    assert_eq!(worker.take_reply(second), Ok(Some(RequestOutcome::Failed)));
    assert_eq!(tenant.used(), empty);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_drop(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 4).expect("drop worker");
    let first = worker.spawn(module, limits(), 1).expect("first actor");
    let second = worker.spawn(module, limits(), 1).expect("second actor");
    let get = module.handler("Get").expect("Get");
    let bump = module.handler("Bump").expect("Bump");
    worker
        .request(first, &get, &[], MAX_STEPS, TIMEOUT)
        .expect("held completed reply");
    assert_eq!(worker.tick(MAX_STEPS), Ok(WorkerEvent::Delivered(first)));
    worker
        .request(first, &bump, &[1], MAX_STEPS, TIMEOUT)
        .expect("first active request");
    let cancelled = worker
        .request(second, &bump, &[2], MAX_STEPS, TIMEOUT)
        .expect("second active request");
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(second)));
    assert_eq!(worker.tick(1), Ok(WorkerEvent::Suspended(first)));
    worker
        .request(first, &bump, &[3], MAX_STEPS, Duration::ZERO)
        .expect("queued expired request");
    worker.cancel_request(cancelled).expect("held cancellation");
    assert_eq!(tenant.used().requests, 4);
    assert_eq!(tenant.used().messages, 3);
    assert_eq!(tenant.used().processes, 2);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(
        arguments.len() == 2 && arguments[0] == "--trusted",
        "usage: scalar_requests --trusted <Queue artifact>"
    );
    let directory = Path::new(&arguments[1]);
    let module = unsafe { ScalarModule::load_trusted(directory) }.expect("trusted Queue artifact");
    let foreign =
        unsafe { ScalarModule::load_trusted(directory) }.expect("separately loaded module");
    verify_schema(&module);
    verify_construction();
    verify_fifo(&module);
    verify_rejections(&module, &foreign);
    verify_pending_capacity(&module);
    verify_limits(&module);
    verify_zero_timeout(&module);
    verify_suspended_terminal(&module);
    verify_worker_ids(&module);
    verify_exit(&module);
    verify_drop(&module);
    println!("scalar requests regression passed");
}
