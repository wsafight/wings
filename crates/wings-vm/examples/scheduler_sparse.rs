use std::path::Path;
use std::time::Duration;

use serde_json::json;
use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig,
};

fn limits() -> Resources {
    Resources {
        bytes: 1 << 30,
        processes: 16384,
        messages: 256,
        requests: 256,
    }
}

fn reply(mut request: IngressRequest, expected: i64) -> Result<(), String> {
    if request.wait(Duration::from_secs(3))? != Some(RequestOutcome::Reply(expected)) {
        return Err("sparse scheduler reply/FIFO mismatch".into());
    }
    Ok(())
}

fn run(directory: &Path, count: usize) -> Result<(), String> {
    if ![1000, 10000].contains(&count) {
        return Err("sparse probe requires 1000/10000 actors".into());
    }
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: count,
            requests: 64,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let empty_bytes = vm.used().bytes;
    let mut pids = Vec::with_capacity(count);
    for _ in 0..count {
        pids.push(handle.spawn(
            &module,
            Resources {
                bytes: 4 << 20,
                ..limits()
            },
            2,
        )?);
    }
    let populated_bytes = vm.used().bytes;
    let baseline = vm.used();
    if handle.spawn(&module, limits(), 2).is_ok() || vm.used() != baseline {
        return Err("sparse scheduler full table admission mismatch".into());
    }
    let increment = module.handler("inc")?;
    let get = module.handler("get")?;
    let step = count / 100;
    for round in 0..3 {
        for start in (0..100).step_by(16) {
            let mut pending = Vec::with_capacity(32);
            for index in start..(start + 16).min(100) {
                let pid = pids[index * step];
                let delta = index as i64 + 1;
                let first =
                    handle.request(pid, &increment, &[delta], 32, Duration::from_secs(3))?;
                let second =
                    handle.request(pid, &increment, &[delta], 32, Duration::from_secs(3))?;
                pending.push((first, (2 * round + 1) * delta));
                pending.push((second, (2 * round + 2) * delta));
            }
            for (request, expected) in pending {
                reply(request, expected)?;
            }
        }
    }
    for index in 0..100 {
        let snapshot = handle.snapshot(pids[index * step])?;
        if snapshot.committed_state != Some(6 * (index as i64 + 1))
            || snapshot.last_worker.is_none()
            || snapshot.owner.is_some()
        {
            return Err("sparse scheduler state/owner/migration mismatch".into());
        }
    }
    for index in [1, count / 2 + 1, count - 1] {
        reply(
            handle.request(pids[index], &get, &[], 32, Duration::from_secs(3))?,
            0,
        )?;
    }
    let old = pids[count - 1];
    handle.stop(old)?;
    if handle
        .request(old, &get, &[], 32, Duration::from_secs(3))
        .is_ok()
    {
        return Err("sparse scheduler accepted stale Pid".into());
    }
    let replacement = handle.spawn(
        &module,
        Resources {
            bytes: 4 << 20,
            ..limits()
        },
        2,
    )?;
    if replacement == old {
        return Err("sparse scheduler reused Pid generation".into());
    }
    reply(
        handle.request(replacement, &get, &[], 32, Duration::from_secs(3))?,
        0,
    )?;
    drop((handle, scheduler, increment, get, module, tenant));
    if vm.used() != Resources::default() {
        return Err("sparse scheduler resource leak".into());
    }
    println!(
        "{}",
        json!({"actors": count, "active": 100, "workers": 2, "replies": 600,
        "empty_bytes": empty_bytes, "populated_bytes": populated_bytes,
        "metric": "logical_reservation_not_rss", "resources_released": true})
    );
    Ok(())
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 3 || arguments[0] != "--trusted" {
        eprintln!("usage: scheduler_sparse --trusted ARTIFACT 1000|10000");
        std::process::exit(2);
    }
    if let Err(error) = run(Path::new(&arguments[1]), arguments[2].parse().unwrap_or(0)) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
