use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use wings_vm::{ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig};

fn limits() -> Resources {
    Resources {
        bytes: 1 << 30,
        processes: 256,
        messages: 256,
        requests: 256,
    }
}

fn run(directory: &Path, count: usize) -> Result<(), String> {
    if ![16, 64, 256].contains(&count) {
        return Err("density probe requires 16/64/256 actors".into());
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
            1,
        )?);
    }
    let populated_bytes = vm.used().bytes;
    for pid in &pids {
        handle.stop(*pid)?;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while pids.iter().any(|pid| handle.snapshot(*pid).is_ok()) {
        if Instant::now() >= deadline {
            return Err("density probe cleanup deadline".into());
        }
        std::thread::yield_now();
    }
    let retired_bytes = vm.used().bytes;
    drop((handle, scheduler, module, tenant));
    if vm.used() != Resources::default() {
        return Err("density probe reservation leak".into());
    }
    println!(
        "{}",
        json!({
            "actors": count, "workers": 2, "requests": 64,
            "empty_bytes": empty_bytes, "populated_bytes": populated_bytes,
            "actor_bytes": populated_bytes - empty_bytes, "retired_bytes": retired_bytes,
            "resources_released": true, "metric": "logical_reservation_not_rss",
        })
    );
    Ok(())
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 3 || arguments[0] != "--trusted" {
        eprintln!("usage: scheduler_density --trusted ARTIFACT 16|64|256");
        std::process::exit(2);
    }
    let count = arguments[2].parse().unwrap_or(0);
    if let Err(error) = run(Path::new(&arguments[1]), count) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
