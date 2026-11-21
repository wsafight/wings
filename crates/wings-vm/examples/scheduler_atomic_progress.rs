use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    ExecutionPhase, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig,
};

const HEALTHY: usize = 8;
const ROUNDS: usize = 16;
const SAMPLES: usize = 256;

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 16,
        messages: 32,
        requests: 16,
    }
}

fn run(directory: &Path, workers: usize) -> Result<Value, String> {
    let origin = Instant::now();
    let deadline = origin + Duration::from_secs(3);
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or("atomic progress probe deadline")
    };
    let vm = ResourceAccount::vm_accounted(limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers,
            capacity: workers + HEALTHY,
            requests: HEALTHY,
            slice_steps: u64::MAX,
            quantum: Duration::from_millis(1),
            yield_samples: SAMPLES,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let hot = (0..workers)
        .map(|_| handle.spawn(&module, limits(), 1))
        .collect::<Result<Vec<_>, _>>()?;
    let healthy = (0..HEALTHY)
        .map(|_| handle.spawn(&module, limits(), 1))
        .collect::<Result<Vec<_>, _>>()?;
    let hold = module.handler("hold")?;
    let work = module.handler("work")?;
    let read = module.handler("read")?;
    let started = Instant::now();
    for pid in &hot {
        handle.send(*pid, &hold, &[], u64::MAX)?;
    }
    for worker in 0..workers {
        while handle
            .yield_statistics(worker)?
            .ok_or("atomic progress observer is disabled")?
            .published
            == 0
        {
            remaining()?;
            std::thread::yield_now();
        }
    }
    let mut heartbeats = Vec::with_capacity(ROUNDS * HEALTHY);
    for round in 1..=ROUNDS {
        for (actor, pid) in healthy.iter().enumerate() {
            let sent = Instant::now();
            let mut request = handle.request(*pid, &work, &[round as i64], 4096, remaining()?)?;
            let outcome = request.wait(remaining()?)?;
            let observed = Instant::now();
            if outcome != Some(RequestOutcome::Reply(round as i64)) {
                return Err("healthy actor failed alongside an unbounded step slice".into());
            }
            heartbeats.push(json!([
                actor,
                round,
                sent.duration_since(origin).as_nanos(),
                observed.duration_since(origin).as_nanos()
            ]));
        }
    }
    let heartbeat_end = Instant::now();
    for (worker, pid) in hot.iter().enumerate() {
        let snapshot = handle.snapshot(*pid)?;
        if snapshot.committed_state != Some(0)
            || snapshot.last_worker != Some(worker)
            || snapshot.migrations != 0
            || snapshot.execution.is_none_or(|execution| {
                execution.phase != ExecutionPhase::Suspended
                    || execution.max_steps != u64::MAX
                    || execution.remaining_steps == 0
            })
        {
            return Err("infinite handler returned, migrated or committed its candidate".into());
        }
    }
    for pid in &healthy {
        let mut request = handle.request(*pid, &read, &[], 64, remaining()?)?;
        if request.wait(remaining()?)? != Some(RequestOutcome::Reply(ROUNDS as i64)) {
            return Err("healthy actor lost its committed progress".into());
        }
    }
    for pid in hot.iter().chain(&healthy) {
        handle.stop(*pid)?;
    }
    while hot
        .iter()
        .chain(&healthy)
        .any(|pid| handle.snapshot(*pid).is_ok())
    {
        remaining()?;
        std::thread::yield_now();
    }
    let replacement = handle.spawn(&module, limits(), 1)?;
    if handle.last_exit().is_some()
        || hot
            .iter()
            .chain(&healthy)
            .any(|pid| handle.snapshot(*pid).is_ok())
    {
        return Err("stopped actors retained execution or reported a handler failure".into());
    }
    let mut request = handle.request(replacement, &read, &[], 64, remaining()?)?;
    if request.wait(remaining()?)? != Some(RequestOutcome::Reply(0)) {
        return Err("replacement actor inherited stopped candidate or committed state".into());
    }
    drop(request);
    handle.stop(replacement)?;
    while handle.snapshot(replacement).is_ok() {
        remaining()?;
        std::thread::yield_now();
    }
    drop(scheduler);
    let stopped = Instant::now();
    remaining()?;
    let mut observations = Vec::with_capacity(workers);
    for worker in 0..workers {
        let mut samples = Vec::with_capacity(SAMPLES);
        while let Some(sample) = handle.take_yield_sample(worker)? {
            if samples.len() == SAMPLES {
                return Err("atomic progress FIFO exceeded its fixed capacity".into());
            }
            let actor = hot
                .iter()
                .position(|pid| *pid == sample.pid)
                .or_else(|| {
                    healthy
                        .iter()
                        .position(|pid| *pid == sample.pid)
                        .map(|index| index + workers)
                })
                .or_else(|| (sample.pid == replacement).then_some(workers + HEALTHY))
                .ok_or("atomic sample lost its full actor identity")?;
            samples.push(json!([
                sample.sequence,
                actor,
                sample
                    .observation
                    .requested_at
                    .checked_duration_since(origin)
                    .ok_or("atomic request predates probe")?
                    .as_nanos(),
                sample
                    .observation
                    .yielded_at
                    .checked_duration_since(origin)
                    .ok_or("atomic confirmation predates probe")?
                    .as_nanos()
            ]));
        }
        let statistics = handle
            .yield_statistics(worker)?
            .ok_or("missing stopped yield counts")?;
        let counters = statistics.preemption.counters;
        if statistics.overflowed
            || counters.overflowed
            || statistics.retained != 0
            || statistics.taken != samples.len() as u64
            || statistics.published != statistics.taken + statistics.dropped
            || counters.confirmed != statistics.published
            || counters.consumed != counters.confirmed
            || counters.requests != counters.coalesced + counters.consumed + counters.cleared
            || statistics.preemption.pending_since.is_some()
        {
            return Err("atomic progress stopped counters do not conserve".into());
        }
        observations.push(json!({"worker": worker, "samples": samples,
            "published": statistics.published, "taken": statistics.taken,
            "dropped": statistics.dropped, "requests": counters.requests,
            "coalesced": counters.coalesced, "consumed": counters.consumed,
            "cleared": counters.cleared, "confirmed": counters.confirmed}));
    }
    drop((handle, hot, healthy, hold, work, read, module, tenant));
    if vm.used() != bootstrap {
        return Err("atomic progress retained business or control resources".into());
    }
    let finished = Instant::now();
    remaining()?;
    Ok(
        json!({"workers": workers, "hot_actors": workers, "healthy_actors": HEALTHY,
        "rounds": ROUNDS, "slice_steps": u64::MAX, "quantum_ns": 1_000_000,
        "sample_capacity": SAMPLES, "started_ns": started.duration_since(origin).as_nanos(),
        "heartbeat_end_ns": heartbeat_end.duration_since(origin).as_nanos(),
        "stopped_ns": stopped.duration_since(origin).as_nanos(),
        "finished_ns": finished.duration_since(origin).as_nanos(),
        "heartbeats": heartbeats, "observations": observations,
        "hot_committed": vec![0; workers], "healthy_committed": vec![ROUNDS; HEALTHY],
        "replacement_committed": 0,
        "stale_identities_rejected": true, "resources_released": true,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes}),
    )
}

fn main() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        return Err("usage: scheduler_atomic_progress --trusted <artifact>".into());
    }
    let directory = Path::new(&arguments[1]);
    let cases = [run(directory, 1)?, run(directory, 2)?];
    println!(
        "{}",
        json!({"version": 1, "status": "ok", "cases": cases,
        "metric": "short_atomic_progress_not_fairness_or_performance_acceptance"})
    );
    Ok(())
}
