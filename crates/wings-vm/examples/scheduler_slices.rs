use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    ExecutionPhase, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig, SchedulerHandle, SchedulerPid, SchedulerSliceOutcome, SchedulerSliceSample,
};

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn drain(
    handle: &SchedulerHandle,
    worker: usize,
    capacity: usize,
    previous: &mut Option<SchedulerSliceSample>,
    actors: &[SchedulerPid; 2],
    origin: Instant,
    samples: &mut Vec<Value>,
) -> Result<(), String> {
    let mut count = 0;
    while let Some(sample) = handle.take_slice_sample(worker)? {
        count += 1;
        if count > capacity
            || sample.worker != worker
            || sample.started_at < origin
            || sample.returned_at < sample.started_at
            || previous.is_some_and(|previous| {
                sample.sequence <= previous.sequence || sample.started_at < previous.returned_at
            })
        {
            return Err("inconsistent scheduler slice interval or sequence".into());
        }
        let actor = actors
            .iter()
            .position(|pid| *pid == sample.pid)
            .ok_or("slice sample lost full actor identity")?;
        let outcome = match sample.outcome {
            SchedulerSliceOutcome::Suspended => "suspended",
            SchedulerSliceOutcome::Complete => "complete",
            SchedulerSliceOutcome::Failed => "failed",
        };
        samples.push(json!([
            sample.sequence,
            actor,
            sample.started_at.duration_since(origin).as_nanos(),
            sample.returned_at.duration_since(origin).as_nanos(),
            outcome
        ]));
        *previous = Some(sample);
    }
    Ok(())
}

fn run(directory: &Path, workers: usize, capacity: usize) -> Result<Value, String> {
    let origin = Instant::now();
    let deadline = origin + Duration::from_secs(3);
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or("slice observation probe deadline")
    };
    let vm = ResourceAccount::vm_accounted(limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers,
            capacity: 2,
            requests: 2,
            slice_steps: 1,
            slice_samples: capacity,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let actors = [
        handle.spawn(&module, limits(), 1)?,
        handle.spawn(&module, limits(), 1)?,
    ];
    let work = module.handler("Work")?;
    let read = module.handler("Read")?;
    let mut first = handle.request(actors[0], &work, &[64], 4096, remaining()?)?;
    let mut second = handle.request(actors[1], &work, &[64], 4096, remaining()?)?;
    if first.wait(remaining()?)? != Some(RequestOutcome::Reply(64))
        || second.wait(remaining()?)? != Some(RequestOutcome::Reply(64))
    {
        return Err("slice observation changed successful handler results".into());
    }
    drop((first, second));
    for pid in actors {
        let mut reply = handle.request(pid, &read, &[], 64, remaining()?)?;
        if reply.wait(remaining()?)? != Some(RequestOutcome::Reply(64)) {
            return Err("slice observation changed committed state".into());
        }
    }
    let mut samples = (0..workers)
        .map(|_| Vec::with_capacity(capacity * 2))
        .collect::<Vec<_>>();
    let mut previous = vec![None; workers];
    for worker in 0..workers {
        drain(
            &handle,
            worker,
            capacity,
            &mut previous[worker],
            &actors,
            origin,
            &mut samples[worker],
        )?;
    }
    handle.send(actors[0], &module.handler("Hold")?, &[], u64::MAX)?;
    loop {
        remaining()?;
        if handle
            .snapshot(actors[0])?
            .execution
            .is_some_and(|snapshot| snapshot.phase == ExecutionPhase::Suspended)
        {
            break;
        }
        std::thread::yield_now();
    }
    let mut healthy = handle.request(actors[1], &work, &[256], 4096, remaining()?)?;
    if healthy.wait(remaining()?)? != Some(RequestOutcome::Reply(256)) {
        return Err("healthy actor did not advance alongside a live suspended handler".into());
    }
    drop(healthy);
    handle.stop(actors[0])?;
    let mut failed = handle.request(actors[1], &module.handler("Fail")?, &[], 64, remaining()?)?;
    if failed.wait(remaining()?)? != Some(RequestOutcome::Failed)
        || handle
            .last_exit()
            .is_none_or(|exit| exit.committed_state != Some(256))
    {
        return Err("slice observation changed failure or committed state".into());
    }
    drop(failed);
    drop(scheduler);
    remaining()?;
    let mut rows = Vec::with_capacity(workers);
    let mut completed = 0;
    let mut failed = 0;
    let mut suspended = 0;
    for worker in 0..workers {
        drain(
            &handle,
            worker,
            capacity,
            &mut previous[worker],
            &actors,
            origin,
            &mut samples[worker],
        )?;
        if handle.yield_statistics(worker)?.is_some() {
            return Err("slice observation enabled the independent atomic yield observer".into());
        }
        match handle.slice_statistics(worker)? {
            None if capacity == 0 => {}
            Some(statistics) if capacity != 0 => {
                if statistics.overflowed
                    || statistics.retained != 0
                    || statistics.taken != samples[worker].len() as u64
                    || statistics.published != statistics.taken + statistics.dropped
                    || statistics.published
                        != statistics.suspended + statistics.completed + statistics.failed
                {
                    return Err("slice observation failed stopped accounting conservation".into());
                }
                completed += statistics.completed;
                failed += statistics.failed;
                suspended += statistics.suspended;
                rows.push(json!({"worker": worker, "published": statistics.published,
                    "taken": statistics.taken, "dropped": statistics.dropped,
                    "suspended": statistics.suspended, "completed": statistics.completed,
                    "failed": statistics.failed, "samples": samples[worker]}));
            }
            _ => return Err("slice observation enablement mismatch".into()),
        }
    }
    if capacity != 0 && (completed != 5 || failed != 2 || suspended == 0) {
        return Err("slice outcome counts do not match real handlers".into());
    }
    drop((handle, work, read, module, tenant));
    if vm.used() != bootstrap {
        return Err("slice observation retained resources".into());
    }
    Ok(
        json!({"workers": workers, "capacity": capacity, "committed": [64, 256],
        "statistics": rows, "resources_released": true,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes}),
    )
}

fn main() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        return Err("usage: scheduler_slices --trusted ARTIFACT".into());
    }
    let mut cases = Vec::with_capacity(6);
    for workers in [1, 2] {
        for capacity in [0, 1, 256] {
            cases.push(run(Path::new(&arguments[1]), workers, capacity)?);
        }
    }
    println!(
        "{}",
        json!({"status": "ok", "metric": "resume_wall_intervals_not_cpu_or_fairness_acceptance", "cases": cases})
    );
    Ok(())
}
