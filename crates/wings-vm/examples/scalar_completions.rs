use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    ExecutionFault, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    Scheduler, SchedulerConfig, SchedulerExitReason, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let left = deadline.saturating_duration_since(Instant::now());
    assert!(!left.is_zero(), "scalar completion stage deadline");
    left
}

fn check(value: i64, kind: bool, input: bool, mode: usize, same: bool) {
    let status = value / 1000 - 10;
    let payload = value % 1000;
    match mode {
        0 => assert_eq!(status, 0),
        1 => assert_eq!(status, 1),
        2 if same => assert_eq!(status, 2),
        2 => assert!(matches!(status, 0 | 2)),
        3 => assert_eq!(status, 3),
        _ => unreachable!(),
    }
    assert_eq!(
        payload,
        if kind {
            if status == 0 && input { 11 } else { 7 }
        } else {
            13
        }
    );
}

fn arguments(kind: bool, value: bool, mode: usize, same: bool) -> [i64; 6] {
    [
        i64::from(kind),
        i64::from(value),
        if mode == 1 { 0 } else { 60000 },
        i64::from(mode == 2),
        i64::from(mode == 3),
        i64::from(same),
    ]
}

fn single(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    kind: bool,
    value: bool,
    mode: usize,
    same: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut worker = ScalarWorker::with_request_capacity(tenant, 2, 3).unwrap();
    let owner = worker.spawn(module, limits(), 2).unwrap();
    for name in ["CompletedBool", "CompletedUnit"] {
        let baseline = tenant.used();
        assert!(
            worker
                .send(owner, &module.handler(name).unwrap(), &[1, 0, 0], 128)
                .is_err()
        );
        assert_eq!(tenant.used(), baseline);
    }
    let request = worker
        .request(
            owner,
            &module.handler("Start").unwrap(),
            &arguments(kind, value, mode, same),
            4096,
            remaining(deadline),
        )
        .unwrap();
    loop {
        remaining(deadline);
        if worker.tick(1).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
    let outcome = worker.take_reply(request).unwrap();
    assert!(worker.take_reply(request).is_err());
    if mode == 2 && !same && outcome == Some(RequestOutcome::Failed) {
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, owner);
        assert_eq!(exit.handler(), "Start");
        assert_eq!(exit.committed_state, Some(0));
        assert_eq!(exit.execution.fault, Some(ExecutionFault::RequestRejected));
        let child = worker.pids().find(|pid| *pid != owner).unwrap();
        assert_eq!(worker.committed_state(child).unwrap(), 1);
    } else if same && mode == 3 {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        assert!(worker.committed_state(owner).is_err());
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, owner);
        assert_eq!(exit.committed_state, Some(9));
        assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    } else {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        check(
            worker.committed_state(owner).unwrap(),
            kind,
            value,
            mode,
            same,
        );
        if !same {
            if mode == 3 {
                let exit = worker.last_exit().unwrap();
                assert_ne!(exit.pid, owner);
                assert_eq!(exit.committed_state, Some(0));
                assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
            } else {
                let child = worker.pids().find(|pid| *pid != owner).unwrap();
                assert_eq!(worker.committed_state(child).unwrap(), 1);
            }
        }
    }
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
    drop(worker);
    remaining(deadline);
}

fn scheduled(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    workers: usize,
    kind: bool,
    value: bool,
    mode: usize,
    same: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let scheduler = Scheduler::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 3,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let owner = handle.spawn(module, limits(), 2).unwrap();
    for name in ["CompletedBool", "CompletedUnit"] {
        let baseline = tenant.used();
        assert!(
            handle
                .send(owner, &module.handler(name).unwrap(), &[1, 0, 0], 128)
                .is_err()
        );
        assert_eq!(tenant.used(), baseline);
    }
    let mut request = handle
        .request(
            owner,
            &module.handler("Start").unwrap(),
            &arguments(kind, value, mode, same),
            4096,
            remaining(deadline),
        )
        .unwrap();
    let outcome = request.wait(remaining(deadline)).unwrap();
    assert!(request.try_take().is_err());
    if mode == 2 && !same && outcome == Some(RequestOutcome::Failed) {
        loop {
            remaining(deadline);
            if let Some(exit) = handle.last_exit() {
                assert_eq!(exit.pid, owner);
                assert_eq!(exit.handler(), "Start");
                assert_eq!(exit.committed_state, Some(0));
                assert_eq!(
                    exit.reason,
                    SchedulerExitReason::Managed(ExecutionFault::RequestRejected)
                );
                break;
            }
            std::thread::yield_now();
        }
    } else if same && mode == 3 {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        loop {
            remaining(deadline);
            if let Some(exit) = handle.last_exit() {
                assert_eq!(exit.pid, owner);
                assert_eq!(exit.committed_state, Some(9));
                assert_eq!(
                    exit.reason,
                    SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
                );
                break;
            }
            std::thread::yield_now();
        }
    } else {
        assert_eq!(outcome, Some(RequestOutcome::Reply(9)));
        loop {
            remaining(deadline);
            let snapshot = handle.snapshot(owner).unwrap();
            if snapshot.owner.is_none()
                && snapshot.queued == 0
                && snapshot.committed_state.unwrap() >= 10000
            {
                check(snapshot.committed_state.unwrap(), kind, value, mode, same);
                break;
            }
            std::thread::yield_now();
        }
        if mode == 3 {
            let exit = handle.last_exit().unwrap();
            assert_ne!(exit.pid, owner);
            assert_eq!(exit.committed_state, Some(0));
            assert_eq!(
                exit.reason,
                SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
            );
        }
    }
    drop((request, handle, scheduler));
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().messages, 0);
    remaining(deadline);
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(args.len() == 2 && args[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&args[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    for (kind, value) in [(true, false), (true, true), (false, false)] {
        for mode in 0..4 {
            for same in [false, true] {
                single(&module, &tenant, kind, value, mode, same);
                for workers in [1, 2] {
                    scheduled(&module, &tenant, workers, kind, value, mode, same);
                }
            }
        }
    }
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Scalar completion events verified: 72 bounded Bool/Unit scenarios, matching values, default failures, non-retracted timeout/cancel, self failure and cleanup; not complete MVP"
    );
}
