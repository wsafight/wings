use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_abi::ValueType;
use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig, SchedulerHandle, SchedulerPid, SchedulerSliceOutcome,
};

const ACTORS: usize = 101;
const MAX_PENDING: usize = 255;
const CPU_STEPS: u64 = 10_000_000;
const DEADLINE_NS: u64 = 100_000_000;
const MAX_SLICES: u64 = 8_000_000;

#[derive(Default)]
struct SliceTrace {
    emitted: u64,
    discarded: u64,
    bounded: bool,
}

impl SliceTrace {
    fn retain(&mut self) -> Result<bool, String> {
        if self.emitted < MAX_SLICES {
            self.emitted += 1;
            Ok(true)
        } else if self.bounded {
            self.discarded = self
                .discarded
                .checked_add(1)
                .ok_or("resume discarded counter overflow")?;
            Ok(false)
        } else {
            Err("fairness resume history limit".into())
        }
    }
}

#[derive(Debug)]
struct Sample {
    sent: u64,
    observed: Option<u64>,
    value: Option<i64>,
    status: &'static str,
}

struct Pending {
    index: usize,
    sent: u64,
    expected: i64,
    request: IngressRequest,
}

fn event(
    trace: &mut BufWriter<File>,
    sequence: u64,
    first: u64,
    second: u64,
    value: i64,
    kind: u64,
) -> Result<(), String> {
    for field in [sequence, first, second, value as u64, kind] {
        trace
            .write_all(&field.to_be_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn decode(text: &str) -> Result<usize, String> {
    match text {
        "10" => Ok(100),
        "30" => Ok(300),
        _ => Err("fairness duration must be 10 or 30ms".into()),
    }
}

fn planned(index: usize) -> u64 {
    index as u64 / 100 * 10_000_000
}

fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err("fairness stage deadline".into())
    } else {
        Ok(left)
    }
}

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: ACTORS as u64,
        messages: 512,
        requests: 512,
    }
}

fn collect(
    pending: &mut Vec<Pending>,
    samples: &mut [Sample],
    start: Instant,
    trace: &mut Option<BufWriter<File>>,
) -> Result<(), String> {
    let mut index = 0;
    for _ in 0..pending.len().min(64) {
        if index >= pending.len() {
            break;
        }
        let Some(outcome) = pending[index].request.try_take()? else {
            index += 1;
            continue;
        };
        let observed = elapsed(start);
        let entry = pending.remove(index);
        let RequestOutcome::Reply(value) = outcome else {
            return Err(format!("fairness incomplete pulse {outcome:?}"));
        };
        if value != entry.expected {
            return Err("fairness per-target FIFO/value".into());
        }
        let timely = observed < planned(entry.index) + DEADLINE_NS;
        let status = if timely { "reply" } else { "timed_out" };
        if let Some(trace) = trace {
            event(
                trace,
                entry.index as u64 + 1,
                entry.sent,
                observed,
                value,
                if timely { 1 } else { 2 },
            )?;
        } else {
            let sample = &mut samples[entry.index];
            sample.observed = Some(observed);
            sample.value = Some(value);
            sample.status = status;
        }
    }
    Ok(())
}

fn collect_yields(
    handle: &SchedulerHandle,
    pids: &[SchedulerPid],
    start: Instant,
    samples: &mut Vec<Value>,
    trace: &mut Option<BufWriter<File>>,
) -> Result<usize, String> {
    let mut count = 0;
    for _ in 0..32 {
        let Some(sample) = handle.take_yield_sample(0)? else {
            break;
        };
        if trace.is_none() && samples.len() == 4096 {
            return Err("fairness yield history limit".into());
        }
        let identity = pids
            .iter()
            .position(|pid| *pid == sample.pid)
            .ok_or("fairness yield pid")?
            + 1;
        let requested = sample
            .observation
            .requested_at
            .checked_duration_since(start)
            .ok_or("fairness yield origin")?
            .as_nanos() as u64;
        let yielded = sample
            .observation
            .yielded_at
            .checked_duration_since(start)
            .ok_or("fairness yield clock")?
            .as_nanos() as u64;
        if let Some(trace) = trace {
            event(
                trace,
                sample.sequence,
                requested,
                yielded,
                identity as i64,
                7,
            )?;
        } else {
            samples.push(json!([sample.sequence, identity, requested, yielded]));
        }
        count += 1;
    }
    Ok(count)
}

fn collect_slices(
    handle: &SchedulerHandle,
    pids: &[SchedulerPid],
    start: Instant,
    trace: &mut BufWriter<File>,
    retention: &mut SliceTrace,
) -> Result<usize, String> {
    let mut count = 0;
    while count < 1024 {
        let Some(sample) = handle.take_slice_sample(0)? else {
            break;
        };
        if !retention.retain()? {
            count += 1;
            continue;
        }
        let identity = pids
            .iter()
            .position(|pid| *pid == sample.pid)
            .ok_or("fairness resume pid")?
            + 1;
        let began = sample
            .started_at
            .checked_duration_since(start)
            .ok_or("fairness resume origin")?
            .as_nanos() as u64;
        let returned = sample
            .returned_at
            .checked_duration_since(start)
            .ok_or("fairness resume clock")?
            .as_nanos() as u64;
        let kind = match sample.outcome {
            SchedulerSliceOutcome::Suspended => 8,
            SchedulerSliceOutcome::Complete => 9,
            SchedulerSliceOutcome::Failed => 10,
        };
        event(
            trace,
            sample.sequence,
            began,
            returned,
            identity as i64,
            kind,
        )?;
        count += 1;
    }
    Ok(count)
}

fn execute(
    directory: &Path,
    count: usize,
    observe: bool,
    observe_slices: bool,
    bounded_slices: bool,
    destination: Option<&Path>,
) -> Result<Value, String> {
    let mut trace = destination
        .map(|destination| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map(|file| BufWriter::with_capacity(65536, file))
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    let measure = trace.is_some();
    let vm = ResourceAccount::vm_accounted(limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let configure = module.handler("Configure")?;
    if module.manifest().state != ValueType::BytesRef
        || configure.schema().parameters.len() != 1
        || configure
            .schema()
            .parameters
            .iter()
            .any(|(_, ty)| *ty != ValueType::Int)
        || configure.schema().reply != Some(ValueType::Int)
    {
        return Err("fairness artifact schema mismatch".into());
    }
    let cpu = module.handler("Cpu")?;
    let pulse = module.handler("Pulse")?;
    if [&cpu, &pulse].iter().any(|handler| {
        handler.schema().parameters.len() != 4
            || handler
                .schema()
                .parameters
                .iter()
                .any(|(_, ty)| *ty != ValueType::Int)
            || handler.schema().reply != Some(ValueType::Int)
    }) {
        return Err("fairness artifact schema mismatch".into());
    }
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 1,
            capacity: ACTORS,
            requests: 256,
            slice_steps: 4096,
            yield_samples: if observe { 64 } else { 0 },
            slice_samples: if observe_slices { 1024 } else { 0 },
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let observation_start = Instant::now();
    let mut yield_samples = Vec::with_capacity(if observe && !measure { 4096 } else { 0 });
    let mut slice_retention = SliceTrace {
        bounded: bounded_slices,
        ..SliceTrace::default()
    };
    let deadline = observation_start + Duration::from_secs(if measure { 120 } else { 3 });
    let mut pids = Vec::with_capacity(ACTORS);
    for identity in 1..=ACTORS {
        remaining(deadline)?;
        let pid = handle.spawn_with_heap(
            &module,
            Resources {
                bytes: 4 << 20,
                processes: 1,
                ..limits()
            },
            1,
            4,
        )?;
        let mut request = handle.request(
            pid,
            &configure,
            &[identity as i64],
            4096,
            remaining(deadline)?,
        )?;
        if request.wait(remaining(deadline)?)? != Some(RequestOutcome::Reply(0)) {
            return Err("fairness configure".into());
        }
        pids.push(pid);
    }
    let mut samples = Vec::with_capacity(if measure { 0 } else { count });
    let mut pending = Vec::with_capacity(MAX_PENDING);
    let mut committed = [0i64; ACTORS];
    let mut hot_samples = Vec::with_capacity(if measure { 0 } else { 32 });
    let mut hot_count = 0;
    let mut hot_request = None;
    let mut hot_sent = 0;
    let mut next = 0;
    let start = Instant::now();
    let observation_offset = start.duration_since(observation_start).as_nanos() as u64;
    let mut sampling = false;
    if measure {
        println!(
            "{}",
            json!({"event": "start", "warmup_seconds": 30, "sample_seconds": 60})
        );
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
    }
    while next < count
        || !pending.is_empty()
        || hot_request.is_some()
        || measure && elapsed(start) < 90_000_000_000
    {
        remaining(deadline)?;
        if observe_slices {
            collect_slices(
                &handle,
                &pids,
                observation_start,
                trace.as_mut().ok_or("resume observation requires trace")?,
                &mut slice_retention,
            )?;
        }
        if observe {
            collect_yields(
                &handle,
                &pids,
                observation_start,
                &mut yield_samples,
                &mut trace,
            )?;
        }
        if measure && !sampling && elapsed(start) >= 30_000_000_000 {
            println!(
                "{}",
                json!({"event": "sampling", "elapsed_ns": elapsed(start)})
            );
            std::io::stdout()
                .flush()
                .map_err(|error| error.to_string())?;
            sampling = true;
        }
        collect(&mut pending, &mut samples, start, &mut trace)?;
        if let Some(request) = hot_request.as_mut() {
            let request: &mut IngressRequest = request;
            if let Some(outcome) = request.try_take()? {
                let observed = elapsed(start);
                if outcome != RequestOutcome::Reply(1_000_000) {
                    return Err(format!("fairness incomplete CPU {outcome:?}"));
                }
                hot_count += 1;
                if let Some(trace) = &mut trace {
                    event(trace, hot_count, hot_sent, observed, 1_000_000, 6)?;
                } else {
                    hot_samples.push(json!([hot_count, hot_sent, observed, 1_000_000]));
                }
                committed[0] += 1;
                hot_request = None;
            }
        }
        if hot_request.is_none() && (next < count || measure && elapsed(start) < 90_000_000_000) {
            if hot_count == if measure { 1_000_000 } else { 10000 } {
                return Err("fairness CPU sample limit".into());
            }
            hot_sent = elapsed(start);
            hot_request = Some(handle.request(
                pids[0],
                &cpu,
                &[hot_count as i64 + 1, 2, 0, 0],
                CPU_STEPS,
                remaining(deadline)?,
            )?);
            if let Some(trace) = &mut trace {
                event(trace, hot_count + 1, hot_sent, 0, 0, 5)?;
            }
        }
        if next == count || elapsed(start) < planned(next) {
            std::thread::yield_now();
            continue;
        }
        let actor = 1 + next % 100;
        let sent = elapsed(start);
        let status = if pending.len() == MAX_PENDING {
            "pending_full"
        } else {
            match handle.request(
                pids[actor],
                &pulse,
                &[next as i64 + 1, 1, 1, 0],
                4096,
                remaining(deadline)?,
            ) {
                Ok(request) => {
                    committed[actor] += 1;
                    pending.push(Pending {
                        index: next,
                        sent,
                        expected: committed[actor],
                        request,
                    });
                    "pending"
                }
                Err(error) if error == "mailbox_full" => "mailbox_full",
                Err(error) => return Err(error),
            }
        };
        if let Some(trace) = &mut trace {
            let kind = match status {
                "pending" => 0,
                "mailbox_full" => 3,
                "pending_full" => 4,
                _ => unreachable!(),
            };
            event(trace, next as u64 + 1, sent, 0, 0, kind)?;
        } else {
            samples.push(Sample {
                sent,
                observed: None,
                value: None,
                status,
            });
        }
        next += 1;
    }
    let measurement_elapsed = elapsed(start);
    for (index, pid) in pids.iter().enumerate() {
        loop {
            remaining(deadline)?;
            if handle.snapshot(*pid)?.owner.is_some() {
                std::thread::yield_now();
                continue;
            }
            let state = handle
                .byte_state_snapshot(*pid)?
                .ok_or("fairness missing state")?;
            if state.length != 256
                || i64::from_be_bytes(state.prefix[..8].try_into().expect("counter"))
                    != committed[index]
                || u64::from_be_bytes(state.prefix[8..16].try_into().expect("identity"))
                    != index as u64 + 1
                || state.prefix[16..].iter().any(|byte| *byte != 0)
            {
                return Err("fairness committed state".into());
            }
            break;
        }
        handle.stop(*pid)?;
    }
    let samples: Vec<_> = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            json!([
                index + 1,
                2 + index % 100,
                planned(index),
                sample.sent,
                sample.status,
                sample.observed,
                sample.value
            ])
        })
        .collect();
    drop(scheduler);
    remaining(deadline)?;
    let yield_observation = if observe {
        while collect_yields(
            &handle,
            &pids,
            observation_start,
            &mut yield_samples,
            &mut trace,
        )? != 0
        {}
        let statistics = handle
            .yield_statistics(0)?
            .ok_or("missing scheduler yield statistics")?;
        let counters = statistics.preemption.counters;
        json!({"capacity": 64, "samples": yield_samples, "requests": counters.requests,
            "coalesced": counters.coalesced, "consumed": counters.consumed, "cleared": counters.cleared,
            "confirmed": counters.confirmed, "pending": statistics.preemption.pending_since.is_some(),
            "retained": statistics.retained, "taken": statistics.taken, "dropped": statistics.dropped,
            "published": statistics.published, "overflowed": statistics.overflowed})
    } else {
        Value::Null
    };
    let slice_observation = if observe_slices {
        let trace = trace.as_mut().ok_or("resume observation requires trace")?;
        while collect_slices(
            &handle,
            &pids,
            observation_start,
            trace,
            &mut slice_retention,
        )? != 0
        {}
        let statistics = handle
            .slice_statistics(0)?
            .ok_or("missing resume statistics")?;
        let mut observation = json!({"capacity": 1024, "retained": statistics.retained, "taken": statistics.taken,
            "dropped": statistics.dropped, "published": statistics.published,
            "suspended": statistics.suspended, "completed": statistics.completed,
            "failed": statistics.failed, "overflowed": statistics.overflowed});
        if bounded_slices {
            observation["trace_limit"] = json!(MAX_SLICES);
            observation["trace_emitted"] = json!(slice_retention.emitted);
            observation["trace_dropped"] = json!(slice_retention.discarded);
        }
        observation
    } else {
        Value::Null
    };
    if let Some(trace) = &mut trace {
        trace.flush().map_err(|error| error.to_string())?;
    }
    drop((handle, configure, cpu, pulse, module, tenant));
    if vm.used() != bootstrap {
        return Err("fairness retained resources".into());
    }
    let mut result = json!({"version": if bounded_slices { 13 } else if observe_slices { 12 } else if measure { 8 } else { 7 }, "status": "ok", "mode": if measure { "measure" } else { "smoke" }, "actors": ACTORS,
        "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": MAX_PENDING,
        "deadline_ns": DEADLINE_NS, "message_bytes": 32, "samples": samples, "hot": hot_samples,
        "committed": committed.to_vec(), "states_verified": true, "resources_released": true,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        "yield_observation": yield_observation,
        "observation_offset_ns": observation_offset,
        "measurement_elapsed_ns": measurement_elapsed,
        "environment": {"runtime": "wings", "workers": 1, "slice_steps": 4096, "quantum_ns": 1_000_000}});
    if observe_slices {
        result["slice_observation"] = slice_observation;
    }
    Ok(result)
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let bounded_slices = arguments
        .get(2)
        .is_some_and(|argument| argument == "--measure-slices-bounded");
    let observe_slices = bounded_slices
        || arguments
            .get(2)
            .is_some_and(|argument| argument == "--measure-slices");
    let measure = observe_slices
        || arguments
            .get(2)
            .is_some_and(|argument| argument == "--measure");
    let observe = arguments
        .last()
        .is_some_and(|argument| argument == "--observe");
    let count = if measure
        && arguments
            .first()
            .is_some_and(|argument| argument == "--trusted")
        && (arguments.len() == 4 && !observe || arguments.len() == 5 && observe)
    {
        Ok(900000)
    } else if !measure
        && (arguments.len() == 3 || arguments.len() == 4 && observe)
        && arguments[0] == "--trusted"
    {
        decode(&arguments[2])
    } else {
        Err("arguments".into())
    };
    let (code, result) = match count {
        Ok(count) => match execute(
            Path::new(&arguments[1]),
            count,
            observe,
            observe_slices,
            bounded_slices,
            measure.then(|| Path::new(&arguments[3])),
        ) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (
                    3,
                    json!({"version": if bounded_slices { 13 } else if observe_slices { 12 } else if measure { 8 } else { 7 }, "status": "oracle_error"}),
                )
            }
        },
        Err(_) => (2, json!({"version": 7, "status": "invalid"})),
    };
    println!("{result}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_retention_keeps_the_cap_and_accounts_discard_without_changing_legacy_failure() {
        let mut retention = SliceTrace {
            emitted: MAX_SLICES - 1,
            bounded: true,
            ..SliceTrace::default()
        };
        assert!(retention.retain().unwrap());
        assert!(!retention.retain().unwrap());
        assert_eq!(retention.emitted, MAX_SLICES);
        assert_eq!(retention.discarded, 1);
        retention.bounded = false;
        assert_eq!(
            retention.retain().unwrap_err(),
            "fairness resume history limit"
        );
        assert_eq!(retention.discarded, 1);
        retention.bounded = true;
        retention.discarded = u64::MAX;
        assert!(retention.retain().is_err());
        assert_eq!(retention.discarded, u64::MAX);
        assert_eq!(retention.emitted, MAX_SLICES);
    }

    #[test]
    fn exact_bursts_and_strict_duration_before_artifact_load() {
        assert_eq!(decode("10").unwrap(), 100);
        assert_eq!(decode("30").unwrap(), 300);
        for text in [
            "0",
            "1",
            "20",
            "100",
            "030",
            "-10",
            "10.0",
            "false",
            "18446744073709551616",
        ] {
            assert!(decode(text).is_err());
        }
        for index in 0..300 {
            assert_eq!(planned(index), index as u64 / 100 * 10_000_000);
        }
        assert_eq!(planned(99), 0);
        assert_eq!(planned(100), 10_000_000);
    }
}
