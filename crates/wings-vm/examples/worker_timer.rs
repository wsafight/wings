use std::path::Path;
use std::time::{Duration, Instant};

use wings_abi::ValueType;
use wings_vm::{
    Pid, RequestId, RequestOutcome, ResourceAccount, ResourceLevel, Resources, ScalarModule,
    ScalarWorker, WorkerEvent,
};

const WORK_STEPS: u64 = 387;
const MAX_ATTEMPTS: usize = 128;
const MAX_TICKS: usize = 128;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const LONG_SLICE: Duration = Duration::from_secs(1);
const SHORT_SLICE: Duration = Duration::from_nanos(1);

fn limits() -> Resources {
    Resources {
        bytes: 8 * 1024 * 1024,
        processes: 8,
        messages: 8,
        requests: 8,
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

fn total(seed: i64) -> i64 {
    128 * seed + 8_128
}

fn enqueue(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid, seed: i64) -> RequestId {
    worker
        .request(
            pid,
            &module.handler("Work").expect("Work"),
            &[seed],
            WORK_STEPS,
            REQUEST_TIMEOUT,
        )
        .expect("Work request")
}

fn assert_reply(worker: &mut ScalarWorker, pid: Pid, id: RequestId, state: i64, reply: i64) {
    assert_eq!(worker.committed_state(pid), Ok(state));
    assert_eq!(worker.queued(pid), Ok(0));
    assert_eq!(worker.resources(pid).expect("message charge").messages, 0);
    assert_eq!(worker.resources(pid).expect("pending charge").requests, 1);
    assert_eq!(
        worker.take_reply(id),
        Ok(Some(RequestOutcome::Reply(reply)))
    );
    assert_eq!(worker.resources(pid).expect("taken charge").requests, 0);
}

fn verify_schema(module: &ScalarModule) {
    assert_eq!(module.manifest().process, "Timer");
    assert_eq!(module.manifest().state, ValueType::Int);
    let work = module.handler("Work").expect("Work");
    assert_eq!(
        work.schema().parameters,
        [("seed".to_string(), ValueType::Int)]
    );
    assert_eq!(work.schema().reply, Some(ValueType::Int));
    for name in ["Get", "Fail"] {
        let handler = module.handler(name).expect("request handler");
        assert!(handler.schema().parameters.is_empty());
        assert_eq!(handler.schema().reply, Some(ValueType::Int));
    }
}

fn verify_reservation_failure(module: &ScalarModule) {
    for level in [ResourceLevel::Vm, ResourceLevel::Tenant] {
        let (vm, tenant) = accounts();
        let mut worker = ScalarWorker::new(&tenant, 1).expect("limited worker");
        let pid = worker.spawn(module, limits(), 1).expect("actor");
        let id = enqueue(&mut worker, module, pid, 1);
        let account = if level == ResourceLevel::Vm {
            &vm
        } else {
            &tenant
        };
        let fill = account
            .reserve(Resources {
                bytes: account.limit().bytes - account.used().bytes,
                ..Resources::default()
            })
            .expect("bounded accounting-only reservation");
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(worker.enable_timer().is_err(), "{level:?} timer limit");
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
        assert!(worker.tick_timed(u64::MAX, LONG_SLICE).is_err());
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
        drop(fill);
        worker
            .enable_timer()
            .expect("enable after reservation rollback");
        assert_eq!(worker.tick(u64::MAX), Ok(WorkerEvent::Delivered(pid)));
        assert_reply(&mut worker, pid, id, total(1), total(1));
        drop(worker);
        assert_empty(&vm, &tenant);
    }
}

fn verify_errors_and_steps(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("worker");
    let pid = worker.spawn(module, limits(), 1).expect("actor");
    let id = enqueue(&mut worker, module, pid, 2);
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert!(worker.tick_timed(u64::MAX, LONG_SLICE).is_err());
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    let process_charge = worker.resources(pid).expect("process charge");
    let before_timer = tenant.used();
    worker.enable_timer().expect("enable timer");
    let timer_bytes = tenant.used().bytes - before_timer.bytes;
    assert!(timer_bytes >= 256 * 1024 + 4096);
    assert_eq!(
        tenant.used(),
        Resources {
            bytes: before_timer.bytes + timer_bytes,
            ..before_timer
        }
    );
    assert_eq!(vm.used(), tenant.used());
    assert_eq!(worker.resources(pid), Ok(process_charge));
    println!(
        "worker timer reserved tenant bytes: {timer_bytes} (conservative accounting, not OS physical memory)"
    );

    for suspended in [false, true] {
        if suspended {
            assert_eq!(worker.tick(128), Ok(WorkerEvent::Suspended(pid)));
            assert_eq!(worker.committed_state(pid), Ok(0));
            assert_eq!(worker.take_reply(id), Ok(None));
        }
        let before = snapshot(&worker, &[pid], &vm, &tenant);
        assert!(worker.enable_timer().is_err());
        assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
        for (steps, duration) in [
            (0, LONG_SLICE),
            (u64::MAX, Duration::ZERO),
            (0, Duration::ZERO),
            (u64::MAX, Duration::MAX),
        ] {
            assert!(worker.tick_timed(steps, duration).is_err());
            assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
            assert_eq!(worker.take_reply(id), Ok(None));
        }
    }
    assert_eq!(worker.tick(u64::MAX), Ok(WorkerEvent::Delivered(pid)));
    assert_reply(&mut worker, pid, id, total(2), total(2));
    let id = enqueue(&mut worker, module, pid, 3);
    assert_eq!(worker.tick(WORK_STEPS), Ok(WorkerEvent::Delivered(pid)));
    assert_reply(&mut worker, pid, id, total(2) + total(3), total(3));
    let id = enqueue(&mut worker, module, pid, 4);
    assert_eq!(
        worker.tick_timed(u64::MAX, LONG_SLICE),
        Ok(WorkerEvent::Delivered(pid))
    );
    assert_reply(
        &mut worker,
        pid,
        id,
        total(2) + total(3) + total(4),
        total(4),
    );
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_timed_resume(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("timed worker");
    worker.enable_timer().expect("enable timer");
    let pid = worker.spawn(module, limits(), 1).expect("actor");
    let baseline = worker.resources(pid).expect("baseline");
    let mut state = 0;
    let mut observations = 0;
    for attempt in 0..MAX_ATTEMPTS {
        let seed = attempt as i64 - 64;
        let id = enqueue(&mut worker, module, pid, seed);
        match worker
            .tick_timed(u64::MAX, SHORT_SLICE)
            .expect("short timed tick")
        {
            WorkerEvent::Suspended(actual) => {
                assert_eq!(actual, pid);
                observations += 1;
                assert_eq!(worker.committed_state(pid), Ok(state));
                assert_eq!(worker.queued(pid), Ok(0));
                assert_eq!(worker.take_reply(id), Ok(None));
                let active = worker.resources(pid).expect("live stack charge");
                assert_eq!(active.messages, 1);
                assert_eq!(active.requests, 1);
                assert!(active.bytes > baseline.bytes + 256 * 1024);
                assert_eq!(
                    worker.tick(u64::MAX),
                    Ok(WorkerEvent::Delivered(pid)),
                    "ordinary tick must resume without a stale timer flag"
                );
            }
            WorkerEvent::Delivered(actual) => assert_eq!(actual, pid),
            event => panic!("unexpected timed event: {event:?}"),
        }
        state += total(seed);
        assert_reply(&mut worker, pid, id, state, total(seed));
        assert_eq!(worker.resources(pid), Ok(baseline));
        assert_eq!(vm.used(), tenant.used());
    }
    println!(
        "worker timer observed Suspended in {observations}/{MAX_ATTEMPTS} attempts (deadline path only; no scheduling-latency or hard-real-time claim)"
    );
    assert!(
        observations > 0,
        "no timer suspension observed in 128 bounded attempts; report this result instead of increasing retries"
    );
    let get = worker
        .request(
            pid,
            &module.handler("Get").expect("Get"),
            &[],
            2,
            REQUEST_TIMEOUT,
        )
        .expect("Get state");
    assert_eq!(worker.tick(u64::MAX), Ok(WorkerEvent::Delivered(pid)));
    assert_reply(&mut worker, pid, get, state, state);
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_budget(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("budget worker");
    worker.enable_timer().expect("enable timer");
    let empty = tenant.used();
    let pid = worker.spawn(module, limits(), 1).expect("actor");
    let work = module.handler("Work").expect("Work");
    let id = worker
        .request(pid, &work, &[1], WORK_STEPS - 1, REQUEST_TIMEOUT)
        .expect("insufficient total budget");
    let mut exited = false;
    let mut suspensions = 0;
    for _ in 0..MAX_TICKS {
        match worker.tick_timed(128, LONG_SLICE).expect("budget tick") {
            WorkerEvent::Suspended(actual) => {
                assert_eq!(actual, pid);
                suspensions += 1;
                assert_eq!(worker.committed_state(pid), Ok(0));
                assert_eq!(worker.take_reply(id), Ok(None));
            }
            WorkerEvent::Exited {
                pid: actual,
                reason,
            } => {
                assert_eq!(actual, pid);
                assert!(reason.contains("budget"), "unexpected exit: {reason}");
                exited = true;
                break;
            }
            event => panic!("total budget was not preserved: {event:?}"),
        }
    }
    assert!(
        exited && suspensions > 0,
        "budget failure must follow bounded suspension/resume"
    );
    assert_eq!(worker.process_count(), 0);
    assert_eq!(tenant.used().messages, 0);
    assert_eq!(tenant.used().processes, 0);
    assert_eq!(tenant.used().requests, 1);
    assert_eq!(worker.take_reply(id), Ok(Some(RequestOutcome::Failed)));
    assert_eq!(tenant.used(), empty);
    assert_eq!(worker.tick(u64::MAX), Ok(WorkerEvent::Idle));
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_start_failure(module: &ScalarModule) {
    let (vm, tenant) = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).expect("start-failure worker");
    worker.enable_timer().expect("enable timer");
    let pid = worker
        .spawn(
            module,
            Resources {
                bytes: 64 * 1024,
                ..limits()
            },
            1,
        )
        .expect("small actor");
    let id = enqueue(&mut worker, module, pid, 1);
    let before = snapshot(&worker, &[pid], &vm, &tenant);
    assert!(worker.tick_timed(u64::MAX, SHORT_SLICE).is_err());
    assert_eq!(snapshot(&worker, &[pid], &vm, &tenant), before);
    assert_eq!(worker.take_reply(id), Ok(None));
    worker.stop(pid).expect("stop small actor");
    assert_eq!(worker.take_reply(id), Ok(Some(RequestOutcome::Failed)));
    let healthy = worker.spawn(module, limits(), 1).expect("healthy actor");
    let id = enqueue(&mut worker, module, healthy, 2);
    assert_eq!(
        worker.tick(u64::MAX),
        Ok(WorkerEvent::Delivered(healthy)),
        "failed timed start must disarm its flag"
    );
    assert_reply(&mut worker, healthy, id, total(2), total(2));
    drop(worker);
    assert_empty(&vm, &tenant);
}

fn verify_drop(module: &ScalarModule) {
    for suspended in [false, true] {
        let (vm, tenant) = accounts();
        let mut worker = ScalarWorker::new(&tenant, 1).expect("drop worker");
        worker.enable_timer().expect("enable timer");
        assert_eq!(
            worker.tick_timed(u64::MAX, Duration::from_secs(3600)),
            Ok(WorkerEvent::Idle)
        );
        if suspended {
            let pid = worker.spawn(module, limits(), 1).expect("drop actor");
            enqueue(&mut worker, module, pid, 1);
            assert_eq!(
                worker.tick_timed(1, LONG_SLICE),
                Ok(WorkerEvent::Suspended(pid))
            );
            assert_eq!(worker.committed_state(pid), Ok(0));
            assert_eq!(tenant.used().messages, 1);
            assert_eq!(tenant.used().requests, 1);
        }
        let started = Instant::now();
        drop(worker);
        println!(
            "worker timer Drop completed: suspended={suspended}, elapsed={:?} (diagnostic only)",
            started.elapsed()
        );
        assert_empty(&vm, &tenant);
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(
        arguments.len() == 2 && arguments[0] == "--trusted",
        "usage: worker_timer --trusted <Timer artifact>"
    );
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }
        .expect("trusted Timer artifact");
    verify_schema(&module);
    verify_reservation_failure(&module);
    verify_errors_and_steps(&module);
    verify_timed_resume(&module);
    verify_budget(&module);
    verify_start_failure(&module);
    verify_drop(&module);
    println!(
        "worker timer regression passed: bounded deadline-path checks, not a latency benchmark"
    );
}
