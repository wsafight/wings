use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    ExecutionFault, IngressRequest, RequestOutcome, ResourceAccount, Resources, RestartPolicy,
    ScalarModule, SchedulerChildId, SchedulerConfig, SchedulerExitReason, SchedulerHandle,
    SchedulerPid, SchedulerRestartStatus, SchedulerSupervisor, SchedulerSupervisorEvent,
    SchedulerTermination, StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 8,
        messages: 64,
        requests: 64,
    }
}

fn remaining(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "concurrent recovery stage deadline");
    remaining
}

fn outcome(request: &mut IngressRequest, deadline: Instant) -> RequestOutcome {
    request.wait(remaining(deadline)).unwrap().unwrap()
}

fn event(supervisor: &mut SchedulerSupervisor, deadline: Instant) -> SchedulerSupervisorEvent {
    loop {
        remaining(deadline);
        if let Some(event) = supervisor.poll() {
            return event;
        }
        std::thread::yield_now();
    }
}

fn value(handle: &SchedulerHandle, pid: SchedulerPid) -> i64 {
    let state = handle.byte_state_snapshot(pid).unwrap().unwrap();
    assert_eq!(state.length, 4096);
    assert!(state.prefix[8..].iter().all(|byte| *byte == 0));
    i64::from_be_bytes(state.prefix[..8].try_into().unwrap())
}

struct CompletedEpoch {
    pids: Vec<SchedulerPid>,
    children: Vec<SchedulerChildId>,
    result: Value,
}

impl CompletedEpoch {
    fn reject_stale(&self, module: &ScalarModule, supervisor: &SchedulerSupervisor) -> usize {
        let handle = supervisor.handle();
        let add = module.handler("add").unwrap();
        for child in &self.children {
            assert!(supervisor.current_pid(*child).is_err());
        }
        for pid in &self.pids {
            assert!(handle.snapshot(*pid).is_err());
            assert!(handle.send(*pid, &add, &[999], 4096).is_err());
            assert!(
                handle
                    .request(*pid, &add, &[999], 4096, Duration::ZERO)
                    .is_err()
            );
        }
        self.pids.len()
    }
}

fn status(result: RequestOutcome) -> &'static str {
    match result {
        RequestOutcome::TimedOut => "timed_out",
        RequestOutcome::Cancelled => "cancelled",
        RequestOutcome::Failed => "failed",
        unexpected => panic!("unexpected recovery result: {unexpected:?}"),
    }
}

fn resources(used: Resources) -> Value {
    json!({"bytes": used.bytes, "processes": used.processes,
        "messages": used.messages, "requests": used.requests})
}

fn epoch(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    supervisor: &mut SchedulerSupervisor,
    previous: Option<&CompletedEpoch>,
    deadline: Instant,
) -> CompletedEpoch {
    let handle = supervisor.handle();
    let policy = RestartPolicy::OnFailure {
        max_restarts: 1,
        within: Duration::from_secs(60),
    };
    let child = supervisor
        .spawn_with_heap(module, limits(), 16, 4, policy)
        .unwrap();
    let healthy = supervisor
        .spawn_with_heap(module, limits(), 1, 4, RestartPolicy::Never)
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let sibling = supervisor.current_pid(healthy).unwrap();
    let mut pids = vec![pid, sibling];
    let mut children = vec![child, healthy];
    if let Some(previous) = previous {
        previous.reject_stale(module, supervisor);
        assert!(pids.iter().all(|pid| !previous.pids.contains(pid)));
    }
    let add = module.handler("add").unwrap();
    assert_eq!(value(&handle, pid), 0);
    handle
        .send(pid, &module.handler("hold").unwrap(), &[], u64::MAX)
        .unwrap();
    while handle.snapshot(pid).unwrap().execution.is_none() {
        remaining(deadline);
        std::thread::yield_now();
    }
    let pending = std::thread::scope(|scope| {
        let mut producers = Vec::with_capacity(2);
        for sender in 0..2 {
            let handle = &handle;
            let add = &add;
            producers.push(scope.spawn(move || {
                let mut results = Vec::with_capacity(8);
                for index in 0..8 {
                    let mut request = handle
                        .request(
                            pid,
                            add,
                            &[sender * 8 + index + 1],
                            4096,
                            if index < 2 {
                                Duration::ZERO
                            } else {
                                remaining(deadline)
                            },
                        )
                        .unwrap();
                    let consumed = if index < 4 {
                        let expected = if index < 2 {
                            RequestOutcome::TimedOut
                        } else {
                            request.cancel().unwrap();
                            RequestOutcome::Cancelled
                        };
                        assert_eq!(outcome(&mut request, deadline), expected);
                        assert!(request.try_take().is_err());
                        Some(status(expected))
                    } else {
                        None
                    };
                    results.push((index, request, consumed));
                }
                results
            }));
        }
        producers
            .into_iter()
            .map(|producer| producer.join().unwrap())
            .collect::<Vec<_>>()
    });
    let queued = handle.snapshot(pid).unwrap();
    assert_eq!(queued.queued, 16);
    assert_eq!(queued.resources.requests, 16);
    assert!(handle.byte_state_snapshot(pid).is_err());
    let before = tenant.used();
    assert_eq!(
        handle
            .request(pid, &add, &[999], 4096, remaining(deadline))
            .err()
            .unwrap(),
        "mailbox_full"
    );
    let after = tenant.used();
    assert_eq!(after, before);
    let mut pulse = handle
        .request(sibling, &add, &[7], 4096, remaining(deadline))
        .unwrap();
    assert_eq!(outcome(&mut pulse, deadline), RequestOutcome::Reply(7));
    assert_eq!(value(&handle, sibling), 7);
    let race = Barrier::new(3);
    let outcomes = std::thread::scope(|scope| {
        let mut consumers = Vec::with_capacity(2);
        for (sender, requests) in pending.into_iter().enumerate() {
            let race = &race;
            consumers.push(scope.spawn(move || {
                race.wait();
                let mut outcomes = Vec::with_capacity(8);
                for (index, mut request, consumed) in requests {
                    let result = if let Some(consumed) = consumed {
                        consumed
                    } else {
                        request.cancel().unwrap();
                        let result = outcome(&mut request, deadline);
                        assert!(matches!(
                            result,
                            RequestOutcome::Cancelled | RequestOutcome::Failed
                        ));
                        status(result)
                    };
                    assert!(request.try_take().is_err());
                    outcomes.push(json!({"sender": sender + 1, "seq": index + 1,
                        "status": result, "consumed_once": true}));
                }
                outcomes
            }));
        }
        race.wait();
        handle.stop(pid).unwrap();
        consumers
            .into_iter()
            .flat_map(|consumer| consumer.join().unwrap())
            .collect::<Vec<_>>()
    });
    let stopped = event(supervisor, deadline);
    assert_eq!(stopped.child, child);
    assert_eq!(stopped.pid, pid);
    assert_eq!(stopped.termination, SchedulerTermination::Stopped);
    assert_eq!(stopped.restart, SchedulerRestartStatus::Stopped);
    assert!(supervisor.current_pid(child).is_err());
    assert!(
        handle
            .request(pid, &add, &[999], 4096, remaining(deadline))
            .is_err()
    );
    assert_eq!(tenant.used().requests, 0);
    let child = supervisor
        .spawn_with_heap(module, limits(), 16, 4, policy)
        .unwrap();
    let mut current = supervisor.current_pid(child).unwrap();
    children.push(child);
    pids.push(current);
    assert_ne!(current, pid);
    let mut failures = Vec::with_capacity(2);
    for attempt in 0..2 {
        assert_eq!(value(&handle, current), 0);
        let mut failure = handle
            .request(
                current,
                &module.handler("fail").unwrap(),
                &[],
                4096,
                remaining(deadline),
            )
            .unwrap();
        failure.cancel().unwrap();
        let result = outcome(&mut failure, deadline);
        assert!(matches!(
            result,
            RequestOutcome::Cancelled | RequestOutcome::Failed
        ));
        assert!(failure.try_take().is_err());
        let failed = event(supervisor, deadline);
        assert_eq!(failed.child, child);
        assert_eq!(failed.pid, current);
        let SchedulerTermination::Failed(exit) = failed.termination else {
            panic!("cancelled result must not retract the failing message")
        };
        assert_eq!(exit.handler(), "fail");
        assert_eq!(
            exit.reason,
            SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
        );
        let state = exit.byte_state.unwrap();
        assert_eq!(state.length, 4096);
        assert!(state.prefix.iter().all(|byte| *byte == 0));
        assert!(
            handle
                .request(current, &add, &[999], 4096, remaining(deadline))
                .is_err()
        );
        if attempt == 0 {
            let SchedulerRestartStatus::Restarted(next) = failed.restart else {
                panic!("expected one fresh restart")
            };
            assert_ne!(next, current);
            assert_eq!(supervisor.current_pid(child).unwrap(), next);
            assert_eq!(value(&handle, next), 0);
            pids.push(next);
            current = next;
        } else {
            assert_eq!(failed.restart, SchedulerRestartStatus::IntensityExceeded);
            assert!(supervisor.current_pid(child).is_err());
        }
        failures.push(json!({"attempt": attempt + 1, "status": status(result),
            "handler": exit.handler(), "reason": "division_by_zero", "state_bytes": state.length,
            "committed": i64::from_be_bytes(state.prefix[..8].try_into().unwrap()),
            "restart": if attempt == 0 {"restarted"} else {"intensity_exceeded"},
            "next_initial": if attempt == 0 {Some(0)} else {None}}));
    }
    assert_eq!(supervisor.current_pid(healthy).unwrap(), sibling);
    let healthy_value = value(&handle, sibling);
    assert_eq!(healthy_value, 7);
    supervisor.stop(healthy).unwrap();
    assert!(supervisor.poll().is_none());
    drop(handle);
    remaining(deadline);
    CompletedEpoch {
        pids,
        children,
        result: json!({"state_bytes": 4096, "queued": queued.queued,
            "retained_request_slots": queued.resources.requests,
            "mailbox_full": true, "before_rejected": resources(before),
            "after_rejected": resources(after), "healthy_committed": healthy_value,
            "stop_restarted": false, "outcomes": outcomes, "failures": failures}),
    }
}

const TRACE_BYTES: u64 = 64 << 20;
const PERIOD_NS: u64 = 100_000_000;

struct Trace {
    output: BufWriter<File>,
    remaining: u64,
}

impl Write for Trace {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other("recovery trace limit"));
        }
        let written = self.output.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

fn emit(value: Value) {
    writeln!(std::io::stdout().lock(), "{value}").unwrap();
}

fn elapsed(start: Instant) -> u64 {
    let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap();
    assert!(elapsed < 120_000_000_000, "recovery total stage deadline");
    elapsed
}

fn wait(start: Instant, planned: u64) {
    loop {
        let now = elapsed(start);
        if now >= planned {
            return;
        }
        std::thread::sleep(Duration::from_nanos((planned - now).min(10_000_000)));
    }
}

fn run(
    vm: &ResourceAccount,
    module: &ScalarModule,
    tenant: &ResourceAccount,
    workers: usize,
    count: u64,
    mut trace: Option<&mut Trace>,
) -> Value {
    let setup_started = Instant::now();
    let mut supervisor = SchedulerSupervisor::new(
        tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 17,
            slice_steps: 64,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let pool = StackPool::new_accounted(tenant, 1, 1).unwrap();
    let stats = pool.stats();
    let slot_bytes = (stats.usable_bytes_per_stack + stats.guard_bytes_per_stack) as u64;
    drop(pool);
    let cold = vm.used();
    let soak = count == 900;
    let mode = if soak { "soak" } else { "verify" };
    if trace.is_some() {
        emit(
            json!({"version": 11, "event": "start", "mode": mode, "epochs": count,
            "warmup_seconds": if soak {30} else {0}, "sample_seconds": if soak {60} else {0},
            "period_ns": if soak {PERIOD_NS} else {0}, "round_deadline_ns": 3_000_000_000u64,
            "trace_limit_bytes": TRACE_BYTES, "cold_bytes": cold.bytes,
            "stack_slot_bytes": slot_bytes, "max_cached_stacks": workers * 2,
            "environment": {"runtime": "wings", "workers": workers, "senders": 2,
                "mailbox": 16, "result_slots": 17, "state_bytes": 4096}}),
        );
    }
    let start = Instant::now();
    let mut previous: Option<CompletedEpoch> = None;
    let mut checks = 0;
    let mut sampling = false;
    for sequence in 1..=count {
        let planned = if soak { (sequence - 1) * PERIOD_NS } else { 0 };
        wait(start, planned);
        let started = elapsed(start);
        if soak && !sampling && started >= 30_000_000_000 {
            emit(json!({"version": 11, "event": "sampling", "elapsed_ns": started}));
            sampling = true;
        }
        let deadline = if count == 1 {
            setup_started + Duration::from_secs(3)
        } else {
            start + Duration::from_nanos(started) + Duration::from_secs(3)
        };
        let mut completed = epoch(module, tenant, &mut supervisor, previous.as_ref(), deadline);
        let stale = completed.reject_stale(module, &supervisor)
            + previous.as_ref().map_or(0, |epoch| epoch.pids.len());
        let (used, cached) = loop {
            remaining(deadline);
            let used = vm.used();
            if used.processes == 0 && used.messages == 0 && used.requests == 0 {
                if let Some(extra) = used.bytes.checked_sub(cold.bytes) {
                    if extra % slot_bytes == 0 && extra / slot_bytes <= workers as u64 * 2 {
                        break (used, extra / slot_bytes);
                    }
                }
            }
            std::thread::yield_now();
        };
        checks += stale;
        if let Some(trace) = trace.as_deref_mut() {
            let record = json!({"version": 11, "epoch": sequence, "planned_ns": planned,
                "started_ns": started, "completed_ns": elapsed(start),
                "stale_identities_checked": stale, "retained": resources(used),
                "cached_stacks": cached, "result": std::mem::take(&mut completed.result)});
            serde_json::to_writer(&mut *trace, &record).unwrap();
            trace.write_all(b"\n").unwrap();
        }
        previous = Some(completed);
    }
    if soak {
        wait(start, 90_000_000_000);
        assert!(sampling);
    }
    let measured = elapsed(start);
    drop((previous, supervisor));
    if count == 1 {
        remaining(setup_started + Duration::from_secs(3));
    }
    elapsed(start);
    json!({"version": 11, "event": "complete", "status": "ok", "mode": mode,
        "epochs": count, "measurement_elapsed_ns": measured, "stale_identities_checked": checks})
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert!((arguments.len() == 2 || arguments.len() == 5) && arguments[0] == "--trusted");
    let persistent = arguments.len() == 5;
    let configuration = persistent.then(|| {
        let count = if arguments[2] == "--soak" {
            900
        } else {
            assert_eq!(arguments[2], "--verify-reuse");
            3
        };
        let workers = arguments[3].to_str().unwrap().parse::<usize>().unwrap();
        assert!([1, 2].contains(&workers));
        (workers, count)
    });
    let mut trace = persistent.then(|| Trace {
        output: BufWriter::with_capacity(
            65536,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&arguments[4])
                .unwrap(),
        ),
        remaining: TRACE_BYTES,
    });
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let mut final_result = if let Some((workers, count)) = configuration {
        run(&vm, &module, &tenant, workers, count, trace.as_mut())
    } else {
        for workers in [1, 2] {
            run(&vm, &module, &tenant, workers, 1, None);
        }
        Value::Null
    };
    drop((module, tenant));
    assert_eq!(vm.used(), bootstrap);
    if let Some(mut trace) = trace {
        trace.flush().unwrap();
        trace.output.get_ref().sync_all().unwrap();
        final_result["trace_bytes"] = json!(TRACE_BYTES - trace.remaining);
        final_result["resources_released"] = json!(true);
        final_result["bootstrap_bytes"] = json!(bootstrap.bytes);
        final_result["remaining_bytes"] = json!(vm.used().bytes);
        emit(final_result);
    } else {
        println!(
            "Concurrent recovery verified: retained expired/cancelled queues, stop races, independent Bytes state, fresh restart, intensity and cleanup; not a performance gate"
        );
    }
}
