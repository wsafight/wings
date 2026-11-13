use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig,
};

const PENDING: usize = 256;
const DEADLINE_NS: u64 = 100_000_000;

#[derive(Clone, Copy, Debug)]
struct Workload {
    actors: usize,
    state_bytes: usize,
    mailbox: usize,
    hot: bool,
    rate: u64,
    count: usize,
}

fn number(text: &str) -> Result<u64, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("integer".into());
    }
    text.parse().map_err(|_| "integer".into())
}

fn decode(name: &str, duration: &str, rate: &str) -> Result<Workload, String> {
    let (actors, state_bytes, mailbox, hot) = match name {
        "sparse-256" => (10000, 256, 1, false),
        "mixed-4k" => (1000, 4096, 1, false),
        "dense-64k" => (100, 65536, 1, false),
        "hot-queue" => (100, 4096, 100, true),
        _ => return Err("case".into()),
    };
    let duration = number(duration)?;
    let rate = number(rate)?;
    if !(1..=1000).contains(&duration) || !(1..=1_280_000).contains(&rate) {
        return Err("limits".into());
    }
    let count = (duration * rate).div_ceil(1000) as usize;
    if count > 10000 {
        return Err("samples".into());
    }
    Ok(Workload {
        actors,
        state_bytes,
        mailbox,
        hot,
        rate,
        count,
    })
}

impl Workload {
    fn target(self, index: usize) -> usize {
        if !self.hot {
            index % 100
        } else if index % 100 < 90 {
            0
        } else {
            1 + (index / 100 * 10 + index % 100 - 90) % 99
        }
    }

    fn planned(self, index: usize) -> u64 {
        index as u64 * 1_000_000_000 / self.rate
    }
}

#[derive(Debug)]
struct Sample {
    planned: u64,
    sent: u64,
    status: &'static str,
    observed: Option<u64>,
    value: Option<i64>,
}

struct Pending {
    index: usize,
    expected: i64,
    request: IngressRequest,
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err("open-loop stage deadline".into())
    } else {
        Ok(duration)
    }
}

fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

fn classify(planned: u64, observed: u64) -> &'static str {
    if observed < planned + DEADLINE_NS {
        "reply"
    } else {
        "timed_out"
    }
}

fn collect(
    pending: &mut Vec<Pending>,
    samples: &mut [Sample],
    start: Instant,
    cursor: &mut usize,
) -> Result<(), String> {
    let checks = pending.len().min(32);
    for _ in 0..checks {
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
            return Err(format!(
                "open-loop missing actual reply: {outcome:?}, elapsed {}ns",
                elapsed(start)
            ));
        };
        if value != entry.expected {
            return Err("open-loop per-target FIFO/value".into());
        }
        let sample = &mut samples[entry.index];
        sample.status = classify(sample.planned, observed);
        sample.observed = Some(observed);
        sample.value = Some(value);
    }
    Ok(())
}

fn limits() -> Resources {
    Resources {
        bytes: 256 << 20,
        processes: 10000,
        messages: 512,
        requests: 512,
    }
}

fn execute(directory: &Path, workload: Workload) -> Result<Value, String> {
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            capacity: workload.actors,
            requests: PENDING,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let configure = module.handler("Configure")?;
    let apply = module.handler("Apply")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pids = Vec::with_capacity(workload.actors);
    for _ in 0..workload.actors {
        remaining(deadline)?;
        pids.push(handle.spawn_with_heap(
            &module,
            Resources {
                bytes: 4 << 20,
                processes: 1,
                ..limits()
            },
            workload.mailbox,
            4,
        )?);
    }
    for (batch, group) in pids.chunks(32).enumerate() {
        let mut initializing = Vec::with_capacity(32);
        for (offset, pid) in group.iter().enumerate() {
            initializing.push(handle.request(
                *pid,
                &configure,
                &[
                    workload.state_bytes as i64,
                    0,
                    (batch * 32 + offset + 1) as i64,
                ],
                4096,
                remaining(deadline)?,
            )?);
        }
        for mut request in initializing {
            if request.wait(remaining(deadline)?)? != Some(RequestOutcome::Reply(0)) {
                return Err("open-loop configure".into());
            }
        }
    }
    let mut samples = Vec::with_capacity(workload.count);
    let mut pending = Vec::with_capacity(PENDING);
    let mut cursor = 0;
    let mut committed = vec![0i64; workload.actors];
    let start = Instant::now();
    while samples.len() < workload.count || !pending.is_empty() {
        remaining(deadline)?;
        collect(&mut pending, &mut samples, start, &mut cursor)?;
        let index = samples.len();
        if index == workload.count || elapsed(start) < workload.planned(index) {
            std::thread::yield_now();
            continue;
        }
        let target = workload.target(index);
        let planned = workload.planned(index);
        let sent = elapsed(start);
        let status = if pending.len() == PENDING {
            "pending_full"
        } else {
            match handle.request(
                pids[target],
                &apply,
                &[index as i64 + 1, 1, 1, 0],
                4096,
                remaining(deadline)?,
            ) {
                Ok(request) => {
                    committed[target] += 1;
                    pending.push(Pending {
                        index,
                        expected: committed[target],
                        request,
                    });
                    "pending"
                }
                Err(error) if error == "mailbox_full" => "mailbox_full",
                Err(error) if error == "ingress_requests_full" => "pending_full",
                Err(error) => return Err(error),
            }
        };
        samples.push(Sample {
            planned,
            sent,
            status,
            observed: None,
            value: None,
        });
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
                .ok_or("missing byte state")?;
            if state.length != workload.state_bytes
                || i64::from_be_bytes(state.prefix[..8].try_into().expect("counter"))
                    != committed[index]
                || u64::from_be_bytes(state.prefix[8..16].try_into().expect("identity"))
                    != index as u64 + 1
                || state.prefix[16..].iter().any(|byte| *byte != 0)
            {
                return Err("open-loop committed state".into());
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
                workload.target(index) + 1,
                sample.planned,
                sample.sent,
                sample.status,
                sample.observed,
                sample.value
            ])
        })
        .collect();
    drop((handle, scheduler, apply, configure, module, tenant));
    if vm.used() != Resources::default() {
        return Err("open-loop resources retained".into());
    }
    Ok(
        json!({"version": 5, "status": "ok", "mode": "smoke", "actors": workload.actors,
        "state_bytes": workload.state_bytes, "mailbox_capacity": workload.mailbox,
        "rate": workload.rate, "deadline_ns": DEADLINE_NS, "pending_capacity": PENDING,
        "message_bytes": 32, "samples": samples, "committed": committed,
        "states_verified": true, "resources_released": true,
        "environment": {"runtime": "wings", "workers": 2}}),
    )
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let workload = if arguments.len() == 5 && arguments[0] == "--trusted" {
        decode(&arguments[2], &arguments[3], &arguments[4])
    } else {
        Err("arguments".into())
    };
    let (code, result) = match workload {
        Ok(workload) => match execute(Path::new(&arguments[1]), workload) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (3, json!({"version": 5, "status": "oracle_error"}))
            }
        },
        Err(_) => (2, json!({"version": 5, "status": "invalid"})),
    };
    println!("{result}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_loop_limits_before_artifact_load() {
        for arguments in [
            ("unknown", "1", "1"),
            ("hot-queue", "0", "1000"),
            ("hot-queue", "1001", "1000"),
            ("hot-queue", "1000", "10001"),
            ("hot-queue", "1", "1280001"),
            ("hot-queue", "1", "false"),
            ("hot-queue", "1", "1.0"),
            ("hot-queue", "1", "-1"),
            ("hot-queue", "1", "18446744073709551616"),
        ] {
            assert!(decode(arguments.0, arguments.1, arguments.2).is_err());
        }
        assert_eq!(decode("hot-queue", "1", "1280000").unwrap().count, 1280);
        assert_eq!(decode("sparse-256", "1", "1").unwrap().count, 1);
    }

    #[test]
    fn planned_arrivals_and_hot_routing_never_rebase() {
        let workload = decode("hot-queue", "1000", "10000").unwrap();
        for index in 0..10000 {
            assert_eq!(workload.planned(index), index as u64 * 100000);
            assert_eq!(
                workload.target(index),
                if index % 100 < 90 {
                    0
                } else {
                    1 + (index / 100 * 10 + index % 100 - 90) % 99
                }
            );
        }
        let thirds = decode("mixed-4k", "1000", "3").unwrap();
        assert_eq!(thirds.planned(2), 666_666_666);
    }

    #[test]
    fn deadline_includes_late_generation_and_observation() {
        assert_eq!(classify(17, 100_000_016), "reply");
        assert_eq!(classify(17, 100_000_017), "timed_out");
        assert_eq!(classify(17, 200_000_017), "timed_out");
    }
}
