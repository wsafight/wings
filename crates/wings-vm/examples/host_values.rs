use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    MessageValue, RequestOutcome, ResourceAccount, Resources, RestartPolicy, ScalarModule,
    ScalarSupervisor, ScalarWorker, Scheduler, SchedulerConfig, SupervisorEvent, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let left = deadline.saturating_duration_since(Instant::now());
    assert!(!left.is_zero(), "host values stage deadline");
    left
}

fn values<'value>(
    first: &'value [u8],
    second: &'value [u8],
    integers: &'value [i64],
) -> [MessageValue<'value>; 6] {
    [
        MessageValue::Bytes(first),
        MessageValue::Int(7),
        MessageValue::IntList(integers),
        MessageValue::Bytes(second),
        MessageValue::Bool(true),
        MessageValue::Unit,
    ]
}

fn score(first: &[u8], second: &[u8], integers: &[i64]) -> i64 {
    let bytes = |data: &[u8]| {
        data.len() as i64 * 512 + data.first().map_or(0, |value| i64::from(*value) * 2)
    };
    bytes(first) + bytes(second) + 8 + integers.iter().take(2).sum::<i64>()
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    while worker.tick(1).unwrap() != WorkerEvent::Idle {
        remaining(deadline);
    }
}

fn single(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 2).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 2, 68).unwrap();
    for (first_length, second_length, list_length) in
        [(0, 0, 0), (0, 65536, 1), (32768, 32768, 2), (65536, 0, 64)]
    {
        let mut first = vec![17; first_length];
        let mut second = vec![23; second_length];
        let mut integers = vec![3; list_length];
        let expected = score(&first, &second, &integers);
        worker
            .send_values(
                pid,
                &module.handler("store").unwrap(),
                &values(&first, &second, &integers),
                32768,
            )
            .unwrap();
        first.fill(99);
        second.fill(99);
        integers.fill(99);
        let read = worker
            .request_values(
                pid,
                &module.handler("get").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        drain(&mut worker, deadline);
        assert_eq!(
            worker.take_reply(read).unwrap(),
            Some(RequestOutcome::Reply(expected)),
            "input sizes {first_length}/{second_length}/{list_length}, exit {:?}",
            worker.last_exit()
        );
        let expected = score(&first, &second, &integers);
        let query = worker
            .request_values(
                pid,
                &module.handler("compute").unwrap(),
                &values(&first, &second, &integers),
                32768,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(
            worker.take_reply(query).unwrap(),
            Some(RequestOutcome::TimedOut)
        );
        assert_eq!(worker.queued(pid).unwrap(), 1);
        assert_eq!(worker.resources(pid).unwrap().requests, 1);
        drain(&mut worker, deadline);
        assert!(worker.take_reply(query).is_err());
        assert_eq!(worker.committed_state(pid).unwrap(), expected);
        let query = worker
            .request_values(
                pid,
                &module.handler("compute").unwrap(),
                &values(&first, &second, &integers),
                32768,
                remaining(deadline),
            )
            .unwrap();
        worker.cancel_request(query).unwrap();
        assert_eq!(
            worker.take_reply(query).unwrap(),
            Some(RequestOutcome::Cancelled)
        );
        drain(&mut worker, deadline);
        assert_eq!(worker.committed_state(pid).unwrap(), expected);
        assert_eq!(worker.resources(pid).unwrap().requests, 0);
    }
    let failing = worker.spawn_with_heap(module, limits(), 1, 2).unwrap();
    let query = worker
        .request_values(
            failing,
            &module.handler("compute").unwrap(),
            &values(&[17; 8], &[23; 8], &[3, 5]),
            32768,
            remaining(deadline),
        )
        .unwrap();
    drain(&mut worker, deadline);
    assert_eq!(
        worker.take_reply(query).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert!(worker.committed_state(failing).is_err());
    assert_eq!(worker.last_exit().unwrap().committed_state, Some(0));
    remaining(deadline);
}

fn supervised(module: &ScalarModule, tenant: &ResourceAccount) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut supervisor = ScalarSupervisor::new(tenant, 1, 2).unwrap();
    let child = supervisor
        .spawn_with_heap(module, limits(), 2, 8, RestartPolicy::Never)
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    supervisor
        .send_values(
            pid,
            &module.handler("store").unwrap(),
            &values(&[17; 8], &[23; 8], &[3, 5]),
            32768,
        )
        .unwrap();
    let query = supervisor
        .request_values(
            pid,
            &module.handler("compute").unwrap(),
            &values(&[], &[], &[]),
            32768,
            remaining(deadline),
        )
        .unwrap();
    loop {
        remaining(deadline);
        match supervisor.tick(1).unwrap() {
            SupervisorEvent::Worker(WorkerEvent::Idle) => break,
            SupervisorEvent::Worker(WorkerEvent::Suspended(_) | WorkerEvent::Delivered(_)) => {}
            event => panic!("unexpected typed host supervisor event: {event:?}"),
        }
    }
    assert_eq!(
        supervisor.take_reply(query).unwrap(),
        Some(RequestOutcome::Reply(8))
    );
    supervisor.stop(child).unwrap();
}

fn scheduled(module: &ScalarModule, tenant: &ResourceAccount, workers: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 2,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let pid = handle.spawn_with_heap(module, limits(), 2, 68).unwrap();
    for (first_length, second_length, list_length) in
        [(0, 0, 0), (0, 65536, 1), (32768, 32768, 2), (65536, 0, 64)]
    {
        let mut first = vec![17; first_length];
        let mut second = vec![23; second_length];
        let mut integers = vec![3; list_length];
        let expected = score(&first, &second, &integers);
        handle
            .send_values(
                pid,
                &module.handler("store").unwrap(),
                &values(&first, &second, &integers),
                32768,
            )
            .unwrap();
        let mut read = handle
            .request_values(
                pid,
                &module.handler("get").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        first.fill(99);
        second.fill(99);
        integers.fill(99);
        assert_eq!(
            read.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
        let expected = score(&first, &second, &integers);
        let mut query = handle
            .request_values(
                pid,
                &module.handler("compute").unwrap(),
                &values(&first, &second, &integers),
                32768,
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(
            query.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::TimedOut)
        );
        let mut read = handle
            .request_values(
                pid,
                &module.handler("get").unwrap(),
                &[],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            read.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
    }
    for (name, argument, expected) in [
        ("echo", MessageValue::Bool(true), 1),
        ("ack", MessageValue::Bytes(&[]), 0),
    ] {
        let mut query = handle
            .request_values(
                pid,
                &module.handler(name).unwrap(),
                &[argument],
                32768,
                remaining(deadline),
            )
            .unwrap();
        assert_eq!(
            query.wait(remaining(deadline)).unwrap(),
            Some(RequestOutcome::Reply(expected))
        );
    }
    assert!(
        handle
            .send(pid, &module.handler("store").unwrap(), &[0; 6], 32)
            .is_err()
    );
    let failing = handle.spawn_with_heap(module, limits(), 1, 2).unwrap();
    let mut query = handle
        .request_values(
            failing,
            &module.handler("compute").unwrap(),
            &values(&[17; 8], &[23; 8], &[3, 5]),
            32768,
            remaining(deadline),
        )
        .unwrap();
    assert_eq!(
        query.wait(remaining(deadline)).unwrap(),
        Some(RequestOutcome::Failed)
    );
    drop((query, handle, scheduler));
    remaining(deadline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    single(&module, &tenant);
    supervised(&module, &tenant);
    for workers in [1, 2] {
        scheduled(&module, &tenant, workers);
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Typed host values verified: independent mixed copies, GC import, scalar replies, retained timeout/cancel, import failure and cleanup across worker/supervisor/Scheduler"
    );
}
