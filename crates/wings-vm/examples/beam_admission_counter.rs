use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 8,
        messages: 512,
        requests: 512,
    }
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err("admission Counter deadline".into())
    } else {
        Ok(duration)
    }
}

fn run(directory: &Path, capacity: usize) -> Result<Value, String> {
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
            requests: 256,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let handle = scheduler.handle();
    let busy = handle.spawn_with_heap(&module, limits(), capacity, 4)?;
    let healthy = handle.spawn_with_heap(&module, limits(), 1, 4)?;
    let initial = handle
        .byte_state_snapshot(busy)?
        .ok_or("missing Bytes state")?;
    if initial.length != 4096 || initial.prefix != [0; 32] {
        return Err("initial state mismatch".into());
    }
    let add = module.handler("Add")?;
    handle.send(busy, &module.handler("Hold")?, &[], u64::MAX)?;
    while handle.snapshot(busy)?.execution.is_none() {
        remaining(deadline)?;
        std::thread::yield_now();
    }
    let owner = handle.snapshot(busy)?.owner;
    let mut pending = Vec::with_capacity(capacity);
    for _ in 0..capacity {
        pending.push(handle.request(busy, &add, &[1], 4096, remaining(deadline)?)?);
    }
    let queued = handle.snapshot(busy)?;
    let baseline = vm.used();
    if queued.queued != capacity || queued.resources.messages != capacity as u64 + 1 {
        return Err("admission queue accounting mismatch".into());
    }
    if handle
        .request(busy, &add, &[1], 4096, remaining(deadline)?)
        .err()
        .as_deref()
        != Some("mailbox_full")
        || vm.used() != baseline
    {
        return Err("full mailbox admission mismatch".into());
    }
    handle.send(healthy, &module.handler("Notify")?, &[7], 4096)?;
    loop {
        remaining(deadline)?;
        let snapshot = handle.snapshot(healthy)?;
        if snapshot.owner.is_none() && snapshot.queued == 0 {
            let state = handle
                .byte_state_snapshot(healthy)?
                .ok_or("missing healthy Bytes state")?;
            if i64::from_be_bytes(state.prefix[..8].try_into().expect("counter")) == 7 {
                break;
            }
        }
        std::thread::yield_now();
    }
    if handle.snapshot(busy)?.owner != owner {
        return Err("live owner changed".into());
    }
    handle.stop(busy)?;
    for mut request in pending {
        if request.wait(remaining(deadline)?)? != Some(RequestOutcome::Failed)
            || request.try_take().is_ok()
        {
            return Err("pending did not terminate once".into());
        }
    }
    while handle.snapshot(busy).is_ok() {
        remaining(deadline)?;
        std::thread::yield_now();
    }
    if handle
        .request(busy, &add, &[1], 4096, remaining(deadline)?)
        .is_ok()
    {
        return Err("stale target admitted".into());
    }
    if handle.last_exit().is_some() {
        return Err("stop became managed failure".into());
    }
    remaining(deadline)?;
    drop((scheduler, handle, add, module, tenant));
    if vm.used() != Resources::default() {
        return Err("admission resources retained".into());
    }
    Ok(
        json!({"version": 4, "status": "ok", "state_bytes": 4096, "mailbox_capacity": capacity,
        "accepted": capacity, "rejected_full": 1, "failed": capacity, "healthy_value": 7,
        "initial_value": 0, "queue_released": true, "stale_rejected": true, "resources_released": true,
        "environment": {"workers": 2, "runtime": "wings"}}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let capacity = arguments
        .get(2)
        .and_then(|value| value.parse::<usize>().ok());
    if arguments.len() != 3
        || arguments[0] != "--trusted"
        || !capacity.is_some_and(|value| [1, 100, 256].contains(&value))
    {
        println!("{}", json!({"version": 4, "status": "invalid"}));
        std::process::exit(2);
    }
    match run(
        Path::new(&arguments[1]),
        capacity.expect("validated capacity"),
    ) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(3);
        }
    }
}
