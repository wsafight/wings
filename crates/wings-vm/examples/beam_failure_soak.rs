use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

#[path = "support/failure_counter.rs"]
mod failure_counter;
use failure_counter::{CompletedEpoch, MAX_INPUT, Workload};

const TRACE_BYTES: usize = 64 << 20;
const PERIOD_NS: u64 = 100_000_000;

struct Trace {
    output: BufWriter<File>,
    remaining: usize,
}

impl Write for Trace {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other("failure soak trace limit"));
        }
        let count = self.output.write(bytes)?;
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

fn emit(value: &Value) -> Result<(), String> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

fn elapsed(start: Instant) -> Result<u64, String> {
    let nanos = u64::try_from(start.elapsed().as_nanos()).map_err(|_| "failure soak clock")?;
    if nanos >= 120_000_000_000 {
        return Err("failure soak total stage deadline".into());
    }
    Ok(nanos)
}

fn wait_until(start: Instant, planned: u64) -> Result<(), String> {
    loop {
        let now = elapsed(start)?;
        if now >= planned {
            return Ok(());
        }
        std::thread::sleep(Duration::from_nanos((planned - now).min(10_000_000)));
    }
}

fn validate(workload: &Workload) -> Result<(), String> {
    let failed: Vec<_> = (1..=workload.actors)
        .map(|actor| {
            workload
                .streams
                .iter()
                .flatten()
                .any(|request| request.actor == actor && request.operation == 2)
        })
        .collect();
    if workload.streams.iter().any(Vec::is_empty)
        || !failed.iter().any(|value| *value)
        || !workload
            .streams
            .iter()
            .flatten()
            .any(|request| !failed[request.actor - 1] && request.operation == 1)
    {
        return Err(
            "failure soak requires two streams, injected failure and healthy traffic".into(),
        );
    }
    Ok(())
}

fn quiescent(
    vm: &ResourceAccount,
    cold: Resources,
    slot_bytes: u64,
    deadline: Instant,
) -> Result<(Resources, u64), String> {
    loop {
        failure_counter::remaining(deadline)?;
        let used = vm.used();
        if used.processes == 0 && used.messages == 0 && used.requests == 0 {
            if let Some(retained) = used.bytes.checked_sub(cold.bytes) {
                if retained % slot_bytes == 0 && retained / slot_bytes <= 4 {
                    return Ok((used, retained / slot_bytes));
                }
            }
        }
        std::thread::yield_now();
    }
}

fn run(directory: &Path, path: &Path, workload: &Workload, soak: bool) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("failure soak trace: {error}"))?;
    let mut trace = Trace {
        output: BufWriter::with_capacity(65536, file),
        remaining: TRACE_BYTES,
    };
    let vm = ResourceAccount::vm_accounted(failure_counter::limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(failure_counter::limits())?;
    let mut supervisor = failure_counter::supervisor(&tenant, workload.actors)?;
    let pool = StackPool::new_accounted(&tenant, 1, 1)?;
    let stats = pool.stats();
    let slot_bytes = (stats.usable_bytes_per_stack + stats.guard_bytes_per_stack) as u64;
    drop(pool);
    let cold = vm.used();
    let count = if soak { 900 } else { 3 };
    let mode = if soak { "soak" } else { "verify" };
    emit(
        &json!({"version": 10, "event": "start", "mode": mode, "epochs": count,
        "warmup_seconds": if soak {30} else {0}, "sample_seconds": if soak {60} else {0},
        "period_ns": if soak {PERIOD_NS} else {0}, "round_deadline_ns": 3_000_000_000u64,
        "trace_limit_bytes": TRACE_BYTES, "cold_bytes": cold.bytes, "stack_slot_bytes": slot_bytes,
        "max_cached_stacks": 4,
        "environment": {"runtime": "wings", "workers": 2, "senders": 2, "window_per_sender": 16}}),
    )?;
    let start = Instant::now();
    let mut previous: Option<CompletedEpoch> = None;
    let mut sampling = false;
    let mut stale_total = 0;
    for sequence in 1..=count {
        let planned = if soak { (sequence - 1) * PERIOD_NS } else { 0 };
        wait_until(start, planned)?;
        let started = elapsed(start)?;
        if soak && !sampling && started >= 30_000_000_000 {
            emit(&json!({"version": 10, "event": "sampling", "elapsed_ns": started}))?;
            sampling = true;
        }
        let mut completed = failure_counter::epoch(
            &module,
            &tenant,
            &mut supervisor,
            workload,
            previous.as_ref(),
            Some(start + Duration::from_nanos(started) + Duration::from_secs(3)),
        )?;
        let stale = completed.reject_stale(&module, &supervisor)?
            + previous.as_ref().map_or(0, |epoch| epoch.pids.len());
        let (used, cached) = quiescent(&vm, cold, slot_bytes, completed.deadline)?;
        let ended = elapsed(start)?;
        stale_total += stale;
        let result = std::mem::take(&mut completed.result);
        let record = json!({"version": 10, "epoch": sequence, "planned_ns": planned,
            "started_ns": started, "completed_ns": ended, "stale_identities_checked": stale,
            "retained_bytes": used.bytes, "retained_processes": used.processes,
            "retained_messages": used.messages, "retained_requests": used.requests,
            "cached_stacks": cached, "result": result});
        serde_json::to_writer(&mut trace, &record).map_err(|error| error.to_string())?;
        trace.write_all(b"\n").map_err(|error| error.to_string())?;
        previous = Some(completed);
    }
    if soak {
        wait_until(start, 90_000_000_000)?;
        if !sampling {
            return Err("failure soak missing sampling event".into());
        }
    }
    let measured = elapsed(start)?;
    drop((previous, supervisor, module, tenant));
    elapsed(start)?;
    if vm.used() != bootstrap {
        return Err("failure soak resources retained".into());
    }
    trace.flush().map_err(|error| error.to_string())?;
    trace
        .output
        .get_ref()
        .sync_all()
        .map_err(|error| error.to_string())?;
    emit(
        &json!({"version": 10, "event": "complete", "status": "ok", "mode": mode,
        "epochs": count, "stale_identities_checked": stale_total,
        "measurement_elapsed_ns": measured, "trace_bytes": TRACE_BYTES - trace.remaining,
        "resources_released": true, "bootstrap_bytes": bootstrap.bytes,
        "remaining_bytes": vm.used().bytes}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 4
        || arguments[0] != "--trusted"
        || !(arguments[2] == "--soak" || arguments[2] == "--verify-soak")
    {
        let _ = emit(&json!({"version": 10, "status": "invalid"}));
        std::process::exit(2);
    }
    let mut input = [0; MAX_INPUT + 1];
    let result = (|| {
        let mut source = std::io::stdin().lock();
        let mut count = 0;
        while count < input.len() {
            match source.read(&mut input[count..]) {
                Ok(0) => break,
                Ok(read) => count += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err("failure soak input".into()),
            }
        }
        let workload = failure_counter::decode(&input[..count])?;
        validate(&workload)?;
        Ok::<_, String>(workload)
    })();
    let workload = match result {
        Ok(workload) => workload,
        Err(_) => {
            let _ = emit(&json!({"version": 10, "status": "invalid"}));
            std::process::exit(2);
        }
    };
    if let Err(error) = run(
        Path::new(&arguments[1]),
        Path::new(&arguments[3]),
        &workload,
        arguments[2] == "--soak",
    ) {
        eprintln!("{error}");
        let _ = emit(&json!({"version": 10, "status": "oracle_error"}));
        std::process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_limit_rejects_before_writing_past_remaining_storage() {
        let path = std::env::temp_dir().join(format!("wings-failure-trace-{}", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut trace = Trace {
            output: BufWriter::new(file),
            remaining: 3,
        };
        trace.write_all(b"abc").unwrap();
        assert!(trace.write_all(b"d").is_err());
        trace.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        drop(trace);
        std::fs::remove_file(path).unwrap();
    }
}
