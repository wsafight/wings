use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig,
    SchedulerLockMetrics,
};

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn metrics(value: SchedulerLockMetrics) -> Result<Value, String> {
    if value.saturated || value.max_wait_ns > value.wait_ns || value.max_held_ns > value.held_ns {
        return Err("inconsistent or saturated lock statistics".into());
    }
    Ok(json!({
        "acquisitions": value.acquisitions, "wait_ns": value.wait_ns, "held_ns": value.held_ns,
        "max_wait_ns": value.max_wait_ns, "max_held_ns": value.max_held_ns,
        "saturated": value.saturated,
    }))
}

fn run(directory: &Path, workers: usize, enabled: bool) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or("lock statistics probe deadline")
    };
    let vm = ResourceAccount::vm_accounted(limits())?;
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 2,
            slice_steps: 1,
            lock_statistics: enabled,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let first = handle.spawn(&module, limits(), 1)?;
    let second = handle.spawn(&module, limits(), 1)?;
    let increment = module.handler("Inc")?;
    for expected in 1..=8 {
        let mut first_reply = handle.request(first, &increment, &[1], 64, remaining()?)?;
        let mut second_reply = handle.request(second, &increment, &[1], 64, remaining()?)?;
        if first_reply.wait(remaining()?)? != Some(RequestOutcome::Reply(expected))
            || second_reply.wait(remaining()?)? != Some(RequestOutcome::Reply(expected))
        {
            return Err("lock statistics changed actor results".into());
        }
    }
    if handle.snapshot(first)?.committed_state != Some(8)
        || handle.snapshot(second)?.committed_state != Some(8)
    {
        return Err("lock statistics changed committed states".into());
    }
    let mut failed = handle.request(first, &increment, &[i64::MAX], 64, remaining()?)?;
    if failed.wait(remaining()?)? != Some(RequestOutcome::Failed)
        || handle
            .last_exit()
            .is_none_or(|exit| exit.committed_state != Some(8))
    {
        return Err("lock statistics changed failure cleanup".into());
    }
    drop(failed);
    handle.stop(second)?;
    drop(scheduler);
    let mut rows = Vec::with_capacity(workers);
    let mut starts = 0;
    let mut finishes = 0;
    let mut snapshots = 0;
    for worker in 0..workers {
        match handle.lock_statistics(worker)? {
            Some(value) if enabled => {
                starts += value.start.acquisitions;
                finishes += value.finish.acquisitions;
                snapshots += value.snapshot.acquisitions;
                if value.claim.acquisitions < value.start.acquisitions {
                    return Err("worker starts exceed claims".into());
                }
                rows.push(json!({"worker": worker,
                    "claim": metrics(value.claim)?, "start": metrics(value.start)?,
                    "snapshot": metrics(value.snapshot)?, "finish": metrics(value.finish)?,
                }));
            }
            None if !enabled => {}
            _ => return Err("lock statistics enablement mismatch".into()),
        }
    }
    if enabled && (starts != 17 || finishes != 17 || snapshots == 0) {
        return Err("worker lock phase counts do not match real handlers".into());
    }
    if handle.lock_statistics(workers).is_ok() {
        return Err("invalid worker accepted".into());
    }
    drop((handle, increment, module, tenant));
    if vm.used() != baseline {
        return Err("lock statistics probe resources retained".into());
    }
    Ok(
        json!({"workers": workers, "enabled": enabled, "replies": 16,
            "failed": 1, "committed": [8, 8], "statistics": rows,
            "resources_released": true, "bootstrap_bytes": baseline.bytes,
            "remaining_bytes": vm.used().bytes,
        }),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        eprintln!("usage: scheduler_locks --trusted ARTIFACT");
        std::process::exit(2);
    }
    let result = (|| {
        let mut cases = Vec::with_capacity(4);
        for workers in [1, 2] {
            for enabled in [false, true] {
                cases.push(run(Path::new(&arguments[1]), workers, enabled)?);
            }
        }
        Ok::<_, String>(json!({"status": "ok", "cases": cases,
            "metric": "partial_worker_lock_wall_time_not_cpu_or_performance_acceptance"}))
    })();
    match result {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
