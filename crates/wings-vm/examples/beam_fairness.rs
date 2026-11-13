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
    expected: i64,
    request: IngressRequest,
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
        let sample = &mut samples[entry.index];
        sample.observed = Some(observed);
        sample.value = Some(value);
        sample.status = if observed < planned(entry.index) + DEADLINE_NS {
            "reply"
        } else {
            "timed_out"
        };
    }
    Ok(())
}

fn collect_yields(
    handle: &SchedulerHandle,
    pids: &[SchedulerPid],
    start: Instant,
    samples: &mut Vec<Value>,
) -> Result<usize, String> {
    let mut count = 0;
    for _ in 0..32 {
        let Some(sample) = handle.take_yield_sample(0)? else {
            break;
        };
        if samples.len() == 4096 {
            return Err("fairness yield history limit".into());
        }
        let identity = pids
            .iter()
            .position(|pid| *pid == sample.pid)
            .ok_or("fairness yield pid")?
            + 1;
        samples.push(json!([
            sample.sequence,
            identity,
            sample
                .observation
                .requested_at
                .checked_duration_since(start)
                .ok_or("fairness yield origin")?
                .as_nanos() as u64,
            sample
                .observation
                .yielded_at
                .checked_duration_since(start)
                .ok_or("fairness yield clock")?
                .as_nanos() as u64
        ]));
        count += 1;
    }
    Ok(count)
}

fn execute(directory: &Path, count: usize, observe: bool) -> Result<Value, String> {
    let vm = ResourceAccount::vm(limits());
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
    let mut yield_samples = Vec::with_capacity(if observe { 4096 } else { 0 });
    let configure = module.handler("Configure")?;
    let cpu = module.handler("Cpu")?;
    let pulse = module.handler("Pulse")?;
    let deadline = observation_start + Duration::from_secs(3);
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
    let mut samples = Vec::with_capacity(count);
    let mut pending = Vec::with_capacity(MAX_PENDING);
    let mut committed = [0i64; ACTORS];
    let mut hot_samples = Vec::with_capacity(32);
    let mut hot_request = None;
    let mut hot_sent = 0;
    let mut cursor = 0;
    let mut next = 0;
    let start = Instant::now();
    while next < count || !pending.is_empty() || hot_request.is_some() {
        remaining(deadline)?;
        if observe {
            collect_yields(&handle, &pids, observation_start, &mut yield_samples)?;
        }
        collect(&mut pending, &mut samples, start, &mut cursor)?;
        if let Some(request) = hot_request.as_mut() {
            let request: &mut IngressRequest = request;
            if let Some(outcome) = request.try_take()? {
                let observed = elapsed(start);
                if outcome != RequestOutcome::Reply(1_000_000) {
                    return Err(format!("fairness incomplete CPU {outcome:?}"));
                }
                hot_samples.push(json!([
                    hot_samples.len() + 1,
                    hot_sent,
                    observed,
                    1_000_000
                ]));
                committed[0] += 1;
                hot_request = None;
            }
        }
        if hot_request.is_none() && next < count {
            if hot_samples.len() == 10000 {
                return Err("fairness CPU sample limit".into());
            }
            hot_sent = elapsed(start);
            hot_request = Some(handle.request(
                pids[0],
                &cpu,
                &[hot_samples.len() as i64 + 1, 2, 0, 0],
                CPU_STEPS,
                remaining(deadline)?,
            )?);
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
                        expected: committed[actor],
                        request,
                    });
                    "pending"
                }
                Err(error) if error == "mailbox_full" => "mailbox_full",
                Err(error) => return Err(error),
            }
        };
        samples.push(Sample {
            sent,
            observed: None,
            value: None,
            status,
        });
        next += 1;
    }
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
        while collect_yields(&handle, &pids, observation_start, &mut yield_samples)? != 0 {}
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
    drop((handle, configure, cpu, pulse, module, tenant));
    if vm.used() != Resources::default() {
        return Err("fairness retained resources".into());
    }
    Ok(
        json!({"version": 7, "status": "ok", "mode": "smoke", "actors": ACTORS,
        "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": MAX_PENDING,
        "deadline_ns": DEADLINE_NS, "message_bytes": 32, "samples": samples, "hot": hot_samples,
        "committed": committed.to_vec(), "states_verified": true, "resources_released": true,
        "yield_observation": yield_observation,
        "environment": {"runtime": "wings", "workers": 1, "slice_steps": 4096, "quantum_ns": 1_000_000}}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let observe = arguments.len() == 4 && arguments[3] == "--observe";
    let count = if (arguments.len() == 3 || observe) && arguments[0] == "--trusted" {
        decode(&arguments[2])
    } else {
        Err("arguments".into())
    };
    let (code, result) = match count {
        Ok(count) => match execute(Path::new(&arguments[1]), count, observe) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (3, json!({"version": 7, "status": "oracle_error"}))
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
