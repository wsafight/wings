use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig, SchedulerHandle, SchedulerPid,
};

const ACTORS: usize = 101;
const MAX_PENDING: usize = 255;
const CPU_STEPS: u64 = 10_000_000;
const DEADLINE_NS: u64 = 100_000_000;

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
    cursor: &mut usize,
    trace: &mut Option<BufWriter<File>>,
) -> Result<(), String> {
    for _ in 0..pending.len().min(32) {
        if *cursor >= pending.len() {
            *cursor = 0;
        }
        let Some(outcome) = pending[*cursor].request.try_take()? else {
            *cursor += 1;
            continue;
        };
        let observed = elapsed(start);
        let entry = pending.remove(*cursor);
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

fn execute(
    directory: &Path,
    count: usize,
    observe: bool,
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
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 1,
            capacity: ACTORS,
            requests: 256,
            slice_steps: 4096,
            yield_samples: if observe { 64 } else { 0 },
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let observation_start = Instant::now();
    let mut yield_samples = Vec::with_capacity(if observe && !measure { 4096 } else { 0 });
    let configure = module.handler("Configure")?;
    let cpu = module.handler("Cpu")?;
    let pulse = module.handler("Pulse")?;
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
    let mut cursor = 0;
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
        collect(&mut pending, &mut samples, start, &mut cursor, &mut trace)?;
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
    if let Some(trace) = &mut trace {
        trace.flush().map_err(|error| error.to_string())?;
    }
    drop((handle, configure, cpu, pulse, module, tenant));
    if vm.used() != bootstrap {
        return Err("fairness retained resources".into());
    }
    Ok(
        json!({"version": if measure { 8 } else { 7 }, "status": "ok", "mode": if measure { "measure" } else { "smoke" }, "actors": ACTORS,
        "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": MAX_PENDING,
        "deadline_ns": DEADLINE_NS, "message_bytes": 32, "samples": samples, "hot": hot_samples,
        "committed": committed.to_vec(), "states_verified": true, "resources_released": true,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        "yield_observation": yield_observation,
        "observation_offset_ns": observation_offset,
        "measurement_elapsed_ns": measurement_elapsed,
        "environment": {"runtime": "wings", "workers": 1, "slice_steps": 4096, "quantum_ns": 1_000_000}}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let measure = arguments
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
            measure.then(|| Path::new(&arguments[3])),
        ) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (
                    3,
                    json!({"version": if measure { 8 } else { 7 }, "status": "oracle_error"}),
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
