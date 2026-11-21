use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_abi::ValueType;
use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig, SchedulerHandle, SchedulerLockMetrics, StackPool,
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
    measure: bool,
    observe_locks: bool,
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
        measure: false,
        observe_locks: false,
    })
}

fn decode_measure(name: &str, rate: &str) -> Result<Workload, String> {
    let mut workload = decode(name, "1", rate)?;
    let base = if workload.hot { 10000 } else { 1000 };
    if !(0..8).any(|power| workload.rate == base << power) {
        return Err("measurement rate".into());
    }
    workload.count = workload.rate as usize * 90;
    workload.measure = true;
    Ok(workload)
}

fn decode_arguments(arguments: &[String]) -> Result<Workload, String> {
    if arguments
        .first()
        .is_none_or(|argument| argument != "--trusted")
    {
        return Err("arguments".into());
    }
    if arguments
        .get(2)
        .is_some_and(|argument| argument == "--measure")
    {
        let observe = arguments.len() == 7 && arguments[6] == "--observe-locks";
        if arguments.len() != 6 && !observe {
            return Err("arguments".into());
        }
        let mut workload = decode_measure(&arguments[3], &arguments[4])?;
        workload.observe_locks = observe;
        Ok(workload)
    } else if arguments.len() == 5 {
        decode(&arguments[2], &arguments[3], &arguments[4])
    } else {
        Err("arguments".into())
    }
}

fn lock_metrics(metrics: SchedulerLockMetrics) -> Result<Value, String> {
    if metrics.saturated
        || metrics.max_wait_ns > metrics.wait_ns
        || metrics.max_held_ns > metrics.held_ns
    {
        return Err("open-loop inconsistent lock metrics".into());
    }
    Ok(
        json!({"acquisitions": metrics.acquisitions, "wait_ns": metrics.wait_ns,
        "held_ns": metrics.held_ns, "max_wait_ns": metrics.max_wait_ns,
        "max_held_ns": metrics.max_held_ns, "saturated": metrics.saturated}),
    )
}

fn lock_statistics(handle: &SchedulerHandle, committed: &[i64]) -> Result<(Value, Value), String> {
    let expected = committed
        .iter()
        .try_fold(committed.len() as u64, |total, value| {
            total.checked_add(u64::try_from(*value).ok()?)
        })
        .ok_or("open-loop lock handler count overflow")?;
    let mut starts = 0u64;
    let mut finishes = 0u64;
    let mut workers = Vec::with_capacity(2);
    let mut cpu_workers = Vec::with_capacity(2);
    for worker in 0..2 {
        let stats = handle
            .lock_statistics(worker)?
            .ok_or("open-loop lock statistics missing")?;
        if stats.claim.acquisitions < stats.start.acquisitions {
            return Err("open-loop lock claims missing".into());
        }
        starts = starts
            .checked_add(stats.start.acquisitions)
            .ok_or("lock count overflow")?;
        finishes = finishes
            .checked_add(stats.finish.acquisitions)
            .ok_or("lock count overflow")?;
        workers.push(
            json!({"worker": worker, "claim": lock_metrics(stats.claim)?,
            "start": lock_metrics(stats.start)?, "snapshot": lock_metrics(stats.snapshot)?,
            "finish": lock_metrics(stats.finish)?}),
        );
        let (status, nanoseconds) = match stats.worker_cpu {
            wings_vm::SchedulerWorkerCpu::Pending => ("pending", None),
            wings_vm::SchedulerWorkerCpu::Unavailable => ("unavailable", None),
            wings_vm::SchedulerWorkerCpu::Nanoseconds(value) => ("ok", Some(value)),
        };
        cpu_workers.push(json!({"worker": worker, "status": status, "nanoseconds": nanoseconds}));
    }
    if starts != expected || finishes != expected {
        return Err("open-loop lock handler count mismatch".into());
    }
    Ok((
        json!({"version": 1, "scope": "whole_scheduler_lifetime", "metric": "wall_time_not_cpu",
        "after_worker_join": true, "workers": workers}),
        json!({"version": 1, "scope": "worker_function_including_setup_cleanup",
        "metric": "thread_cpu_time_not_wall", "after_worker_join": true, "workers": cpu_workers}),
    ))
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
    sent: u64,
    request: IngressRequest,
}

enum Recorder {
    Smoke(Vec<Sample>),
    Measure(BufWriter<File>),
}

fn event(sequence: u64, sent: u64, observed: u64, value: i64, kind: u64) -> [u8; 40] {
    let mut output = [0; 40];
    for (position, word) in [sequence, sent, observed, value as u64, kind]
        .into_iter()
        .enumerate()
    {
        output[position * 8..position * 8 + 8].copy_from_slice(&word.to_be_bytes());
    }
    output
}

impl Recorder {
    fn arrival(&mut self, index: usize, sample: Sample) -> Result<(), String> {
        match self {
            Self::Smoke(samples) => samples.push(sample),
            Self::Measure(writer) => {
                let kind = match sample.status {
                    "pending" => 0,
                    "mailbox_full" => 3,
                    "pending_full" => 4,
                    _ => return Err("unknown admission".into()),
                };
                writer
                    .write_all(&event(index as u64 + 1, sample.sent, 0, 0, kind))
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn reply(
        &mut self,
        entry: &Pending,
        planned: u64,
        observed: u64,
        value: i64,
    ) -> Result<(), String> {
        let status = classify(planned, observed);
        match self {
            Self::Smoke(samples) => {
                let sample = &mut samples[entry.index];
                sample.status = status;
                sample.observed = Some(observed);
                sample.value = Some(value);
            }
            Self::Measure(writer) => writer
                .write_all(&event(
                    entry.index as u64 + 1,
                    entry.sent,
                    observed,
                    value,
                    if status == "reply" { 1 } else { 2 },
                ))
                .map_err(|error| error.to_string())?,
        }
        Ok(())
    }
}

fn remaining(deadline: Instant, stage: &str) -> Result<Duration, String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err(format!("open-loop {stage} deadline"))
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
    recorder: &mut Recorder,
    workload: Workload,
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
        recorder.reply(&entry, workload.planned(entry.index), observed, value)?;
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

fn execute(directory: &Path, workload: Workload, trace: Option<&Path>) -> Result<Value, String> {
    let mut recorder = if let Some(path) = trace {
        Recorder::Measure(BufWriter::with_capacity(
            65536,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|error| format!("measurement trace: {error}"))?,
        ))
    } else {
        Recorder::Smoke(Vec::with_capacity(workload.count))
    };
    let vm = ResourceAccount::vm_accounted(limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let configure = module.handler("configure")?;
    let apply = module.handler("apply")?;
    if module.manifest().state != ValueType::BytesRef
        || configure.schema().parameters.len() != 3
        || configure
            .schema()
            .parameters
            .iter()
            .any(|(_, ty)| *ty != ValueType::Int)
        || configure.schema().reply != Some(ValueType::Int)
        || apply.schema().parameters.len() != 4
        || apply
            .schema()
            .parameters
            .iter()
            .any(|(_, ty)| *ty != ValueType::Int)
        || apply.schema().reply != Some(ValueType::Int)
    {
        return Err("open-loop artifact schema mismatch".into());
    }
    let tenant = vm.tenant_accounted(limits())?;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            capacity: workload.actors,
            requests: PENDING,
            lock_statistics: workload.observe_locks,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    let deadline = Instant::now() + Duration::from_secs(if workload.measure { 120 } else { 3 });
    let initializer_pool = StackPool::new_accounted(&tenant, 1, 1)?;
    let mut pids = Vec::with_capacity(workload.actors);
    for _ in 0..workload.actors {
        remaining(deadline, "spawn")?;
        pids.push(handle.spawn_with_heap_in_pool(
            &module,
            Resources {
                bytes: 4 << 20,
                processes: 1,
                ..limits()
            },
            workload.mailbox,
            4,
            &initializer_pool,
        )?);
    }
    drop(initializer_pool);
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
                remaining(deadline, "configure")?,
            )?);
        }
        for mut request in initializing {
            let outcome = request.wait(remaining(deadline, "configure")?)?;
            if outcome != Some(RequestOutcome::Reply(0)) {
                return Err(format!("open-loop configure reply: {outcome:?}"));
            }
        }
    }
    let mut pending = Vec::with_capacity(PENDING);
    let mut cursor = 0;
    let mut committed = vec![0i64; workload.actors];
    if workload.measure {
        println!(
            "{}",
            json!({"event": "start", "warmup_seconds": 30, "sample_seconds": 60})
        );
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
    }
    let start = Instant::now();
    let mut next = 0;
    let mut sampling = false;
    while next < workload.count || !pending.is_empty() {
        remaining(deadline, "arrival/reply")?;
        collect(&mut pending, &mut recorder, workload, start, &mut cursor)?;
        if workload.measure && !sampling && elapsed(start) >= 30_000_000_000 {
            println!(
                "{}",
                json!({"event": "sampling", "elapsed_ns": elapsed(start)})
            );
            std::io::stdout()
                .flush()
                .map_err(|error| error.to_string())?;
            sampling = true;
        }
        let index = next;
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
                remaining(deadline, "apply")?,
            ) {
                Ok(request) => {
                    committed[target] += 1;
                    pending.push(Pending {
                        index,
                        expected: committed[target],
                        sent,
                        request,
                    });
                    "pending"
                }
                Err(error) if error == "mailbox_full" => "mailbox_full",
                Err(error) if error == "ingress_requests_full" => "pending_full",
                Err(error) => return Err(error),
            }
        };
        recorder.arrival(
            index,
            Sample {
                planned,
                sent,
                status,
                observed: None,
                value: None,
            },
        )?;
        next += 1;
    }
    for (index, pid) in pids.iter().enumerate() {
        loop {
            remaining(deadline, "verify/stop")?;
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
    let samples: Vec<_> = match &mut recorder {
        Recorder::Smoke(samples) => samples
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
            .collect(),
        Recorder::Measure(writer) => {
            writer.flush().map_err(|error| error.to_string())?;
            Vec::new()
        }
    };
    drop(scheduler);
    let statistics = if workload.observe_locks {
        Some(lock_statistics(&handle, &committed)?)
    } else {
        None
    };
    drop((handle, apply, configure, module, tenant));
    if vm.used() != bootstrap {
        return Err("open-loop resources retained".into());
    }
    let mut output = json!({"version": if workload.measure { 6 } else { 5 }, "status": "ok", "mode": if workload.measure { "measure" } else { "smoke" }, "actors": workload.actors,
        "state_bytes": workload.state_bytes, "mailbox_capacity": workload.mailbox,
        "rate": workload.rate, "deadline_ns": DEADLINE_NS, "pending_capacity": PENDING,
        "message_bytes": 32, "samples": samples, "committed": committed,
        "states_verified": true, "resources_released": true,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        "environment": {"runtime": "wings", "workers": 2}});
    if let Some((statistics, cpu)) = statistics {
        output["worker_lock_statistics"] = statistics;
        output["worker_cpu_statistics"] = cpu;
    }
    Ok(output)
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let measure = arguments
        .get(2)
        .is_some_and(|argument| argument == "--measure");
    let workload = decode_arguments(&arguments);
    let (code, result) = match workload {
        Ok(workload) => match execute(
            Path::new(&arguments[1]),
            workload,
            measure.then(|| Path::new(&arguments[5])),
        ) {
            Ok(result) => (0, result),
            Err(error) => {
                eprintln!("{error}");
                (
                    3,
                    json!({"version": if measure { 6 } else { 5 }, "status": "oracle_error"}),
                )
            }
        },
        Err(_) => (
            2,
            json!({"version": if measure { 6 } else { 5 }, "status": "invalid"}),
        ),
    };
    println!("{result}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_observation_requires_an_explicit_measurement_suffix() {
        let base: Vec<String> = [
            "--trusted",
            "artifact",
            "--measure",
            "hot-queue",
            "80000",
            "trace",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(!decode_arguments(&base).unwrap().observe_locks);
        let mut observed = base.clone();
        observed.push("--observe-locks".into());
        let workload = decode_arguments(&observed).unwrap();
        assert!(workload.measure && workload.observe_locks);
        assert_eq!(workload.count, 80000 * 90);
        for arguments in [
            Vec::new(),
            vec!["--trusted".into()],
            [base.as_slice(), &["--unknown".into()]].concat(),
            [observed.as_slice(), &["--observe-locks".into()]].concat(),
            [
                "--trusted",
                "artifact",
                "hot-queue",
                "10",
                "80000",
                "--observe-locks",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        ] {
            assert!(decode_arguments(&arguments).is_err());
        }
    }

    #[test]
    fn long_measurement_is_separate_and_fixed_duration() {
        for power in 0..8 {
            let workload = decode_measure("hot-queue", &(10000u64 << power).to_string()).unwrap();
            assert!(workload.measure);
            assert_eq!(workload.count, (10000usize << power) * 90);
        }
        for rate in ["0", "1000", "10001", "2560000", "false"] {
            assert!(decode_measure("hot-queue", rate).is_err());
        }
        assert!(decode("hot-queue", "90000", "10000").is_err());
        assert!(!decode("hot-queue", "100", "10000").unwrap().measure);
    }

    #[test]
    fn binary_event_is_fixed_big_endian_with_signed_value() {
        let encoded = event(17, 23, 31, -7, 2);
        assert_eq!(encoded.len(), 40);
        for (position, expected) in [17, 23, 31, -7i64 as u64, 2].into_iter().enumerate() {
            assert_eq!(
                u64::from_be_bytes(encoded[position * 8..position * 8 + 8].try_into().unwrap()),
                expected
            );
        }
    }

    #[test]
    fn measurement_trace_is_exclusive_and_existing_bytes_are_untouched() {
        let path = std::env::temp_dir().join(format!(
            "wings-measure-existing-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"keep trace").unwrap();
        let workload = decode_measure("hot-queue", "10000").unwrap();
        assert!(execute(Path::new("missing-artifact"), workload, Some(&path)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep trace");
        std::fs::remove_file(path).unwrap();
    }

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
