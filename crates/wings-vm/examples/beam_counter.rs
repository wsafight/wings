use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_abi::ValueType;
use wings_vm::{
    ByteStateSnapshot, ExecutionFault, Pid, RequestOutcome, ResourceAccount, Resources,
    ScalarModule, ScalarWorker, WorkerEvent,
};

const MAX_REQUESTS: usize = 256;
const WINDOW: usize = 32;
const MAX_INPUT: usize = 32 + MAX_REQUESTS * 32;
const MAX_STEPS: u64 = 4096;
const SLICE: u64 = 64;
const DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default)]
struct Request {
    sequence: i64,
    operation: i64,
    delta: i64,
    reserved: i64,
}

struct Workload {
    state_bytes: usize,
    initial: i64,
    requests: [Request; MAX_REQUESTS],
    count: usize,
}

fn word(input: &[u8], index: usize) -> u64 {
    u64::from_be_bytes(
        input[index * 8..index * 8 + 8]
            .try_into()
            .expect("word range"),
    )
}

fn decode(input: &[u8]) -> Result<Workload, String> {
    if input.len() < 32 || input.len() > MAX_INPUT {
        return Err("input_bytes".into());
    }
    if word(input, 0) != 1 {
        return Err("version".into());
    }
    let state_bytes = usize::try_from(word(input, 1)).map_err(|_| "state_bytes")?;
    if ![256, 4096, 65536].contains(&state_bytes) {
        return Err("state_bytes".into());
    }
    let count = usize::try_from(word(input, 3)).map_err(|_| "request_count")?;
    if count > MAX_REQUESTS || input.len() != 32 + count * 32 {
        return Err("request_count".into());
    }
    let mut requests = [Request::default(); MAX_REQUESTS];
    for (index, request) in requests[..count].iter_mut().enumerate() {
        let offset = 4 + index * 4;
        let sequence = word(input, offset);
        let operation = word(input, offset + 1);
        let reserved = word(input, offset + 3);
        if sequence != index as u64 + 1 || ![1, 2].contains(&operation) || reserved != 0 {
            return Err("request_record".into());
        }
        *request = Request {
            sequence: sequence as i64,
            operation: operation as i64,
            delta: word(input, offset + 2) as i64,
            reserved: reserved as i64,
        };
    }
    Ok(Workload {
        state_bytes,
        initial: word(input, 2) as i64,
        requests,
        count,
    })
}

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 1,
        messages: WINDOW as u64,
        requests: WINDOW as u64,
    }
}

fn state(snapshot: Option<ByteStateSnapshot>, length: usize) -> Result<i64, String> {
    let snapshot = snapshot.ok_or("missing committed Bytes snapshot")?;
    if snapshot.length != length
        || snapshot.prefix[8..16] != 1u64.to_be_bytes()
        || snapshot.prefix[16..].iter().any(|byte| *byte != 0)
    {
        return Err("committed byte state layout mismatch".into());
    }
    Ok(i64::from_be_bytes(
        snapshot.prefix[..8].try_into().expect("counter range"),
    ))
}

fn advance(worker: &mut ScalarWorker, pid: Pid, deadline: Instant) -> Result<WorkerEvent, String> {
    loop {
        if Instant::now() >= deadline {
            return Err("actor deadline".into());
        }
        match worker.tick(SLICE)? {
            WorkerEvent::Suspended(actual) if actual == pid => {}
            event => return Ok(event),
        }
    }
}

fn execute(directory: &Path, workload: &Workload) -> Result<(i32, Value), String> {
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let configure = module.handler("Configure")?;
    let apply = module.handler("Apply")?;
    if module.manifest().state != ValueType::BytesRef
        || configure.schema().parameters.len() != 2
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
        return Err("Counter artifact schema mismatch".into());
    }
    let tenant = vm.tenant_accounted(limits())?;
    let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, WINDOW)?;
    let deadline = Instant::now() + DEADLINE;
    let pid = worker.spawn_with_heap(&module, limits(), WINDOW, 4)?;
    let configured = worker.request(
        pid,
        &configure,
        &[workload.state_bytes as i64, workload.initial],
        MAX_STEPS,
        deadline.saturating_duration_since(Instant::now()),
    )?;
    if advance(&mut worker, pid, deadline)? != WorkerEvent::Delivered(pid)
        || worker.take_reply(configured)? != Some(RequestOutcome::Reply(workload.initial))
        || state(worker.byte_state_snapshot(pid)?, workload.state_bytes)? != workload.initial
    {
        return Err("Counter initialization mismatch".into());
    }
    let mut replies = Vec::with_capacity(workload.count);
    let mut reason = None;
    let mut failed_seq = None;
    let mut committed = workload.initial;
    for batch in workload.requests[..workload.count].chunks(WINDOW) {
        let mut pending = [None; WINDOW];
        for (index, request) in batch.iter().enumerate() {
            pending[index] = Some(worker.request(
                pid,
                &apply,
                &[
                    request.sequence,
                    request.operation,
                    request.delta,
                    request.reserved,
                ],
                MAX_STEPS,
                deadline.saturating_duration_since(Instant::now()),
            )?);
        }
        for (index, request) in batch.iter().enumerate() {
            match advance(&mut worker, pid, deadline)? {
                WorkerEvent::Delivered(actual) if actual == pid => {
                    let outcome =
                        worker.take_reply(pending[index].take().expect("pending request"))?;
                    let Some(RequestOutcome::Reply(value)) = outcome else {
                        return Err("request completion FIFO mismatch".into());
                    };
                    committed = state(worker.byte_state_snapshot(pid)?, workload.state_bytes)?;
                    if committed != value {
                        return Err("reply before committed state".into());
                    }
                    replies.push(json!({"seq": request.sequence, "value": value}));
                }
                WorkerEvent::Exited { pid: actual, .. } if actual == pid => {
                    let exit = worker.last_exit().ok_or("missing failure snapshot")?;
                    if exit.pid != pid
                        || exit.handler() != "Apply"
                        || exit.queued != batch.len() - index - 1
                    {
                        return Err("failure identity or queue mismatch".into());
                    }
                    committed = state(exit.byte_state, workload.state_bytes)?;
                    reason = Some(match exit.execution.fault {
                        Some(ExecutionFault::Overflow) => "overflow",
                        Some(ExecutionFault::DivisionByZero) if request.operation == 2 => {
                            "injected_failure"
                        }
                        _ => return Err("unexpected managed failure".into()),
                    });
                    failed_seq = Some(request.sequence);
                    for pending in pending[index..batch.len()].iter_mut() {
                        if worker.take_reply(pending.take().expect("pending failure"))?
                            != Some(RequestOutcome::Failed)
                        {
                            return Err("failed queue request did not terminate".into());
                        }
                    }
                    if worker.process_count() != 0 || worker.tick(SLICE)? != WorkerEvent::Idle {
                        return Err("failed actor continued execution".into());
                    }
                    break;
                }
                _ => return Err("unexpected worker event".into()),
            }
        }
        if reason.is_some() {
            break;
        }
    }
    let code = if reason.is_some() { 1 } else { 0 };
    if code == 0 {
        committed = state(worker.byte_state_snapshot(pid)?, workload.state_bytes)?;
        worker.stop(pid)?;
    }
    let mut output = json!({
        "version": 1, "status": if code == 0 { "ok" } else { "failure" },
        "state_bytes": workload.state_bytes, "message_bytes": 32,
        "committed": committed, "replies": replies,
        "environment": {"runtime": "wings", "abi": wings_abi::SCALAR_ABI_VERSION,
                        "architecture": std::env::consts::ARCH, "os": std::env::consts::OS,
                        "workers": 1, "window": WINDOW},
    });
    if let Some(reason) = reason {
        output["reason"] = json!(reason);
        output["failed_seq"] = json!(failed_seq);
    }
    drop((configure, apply, worker, module, tenant));
    if vm.used() != Resources::default() {
        return Err("Counter leaked resource reservations".into());
    }
    output["resources_released"] = json!(true);
    Ok((code, output))
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        eprintln!("usage: beam_counter --trusted ARTIFACT");
        std::process::exit(2);
    }
    let mut input = [0; MAX_INPUT + 1];
    let mut count = 0;
    let mut source = std::io::stdin().lock();
    while count < input.len() {
        match source.read(&mut input[count..]) {
            Ok(0) => break,
            Ok(size) => count += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => std::process::exit(2),
        }
    }
    let (code, output) = match decode(&input[..count]) {
        Ok(workload) => execute(Path::new(&arguments[1]), &workload)
            .unwrap_or_else(|_| (3, json!({"version": 1, "status": "oracle_error"}))),
        Err(reason) => (
            2,
            json!({"version": 1, "status": "invalid", "reason": reason}),
        ),
    };
    if writeln!(std::io::stdout(), "{output}").is_err() {
        std::process::exit(3);
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Vec<u8> {
        [1u64, 256, i64::MIN as u64, 1, 1, 1, i64::MAX as u64, 0]
            .into_iter()
            .flat_map(u64::to_be_bytes)
            .collect()
    }

    #[test]
    fn bridge_rejects_bounds_schema_and_trailing_bytes_before_loading() {
        let input = input();
        let workload = decode(&input).unwrap();
        assert_eq!(
            (workload.state_bytes, workload.initial, workload.count),
            (256, i64::MIN, 1)
        );
        assert_eq!(workload.requests[0].delta, i64::MAX);
        for (index, value) in [
            (0, 2),
            (1, 257),
            (3, 257),
            (3, u64::MAX),
            (4, 0),
            (5, 3),
            (7, 1),
        ] {
            let mut invalid = input.clone();
            invalid[index * 8..index * 8 + 8].copy_from_slice(&value.to_be_bytes());
            assert!(decode(&invalid).is_err());
        }
        for length in 0..input.len() {
            assert!(decode(&input[..length]).is_err());
        }
        let mut trailing = input;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        assert!(decode(&[0; MAX_INPUT + 1]).is_err());
    }

    #[test]
    fn bridge_accepts_empty_and_maximum_windows_without_integer_truncation() {
        for size in [256u64, 4096, 65536] {
            for count in [0u64, 32, 256] {
                let mut bytes = [1, size, i64::MAX as u64, count]
                    .into_iter()
                    .flat_map(u64::to_be_bytes)
                    .collect::<Vec<_>>();
                for sequence in 1..=count {
                    bytes.extend(
                        [sequence, 2, i64::MIN as u64, 0]
                            .into_iter()
                            .flat_map(u64::to_be_bytes),
                    );
                }
                let workload = decode(&bytes).unwrap();
                assert_eq!(workload.count, count as usize);
                for (index, request) in workload.requests[..workload.count].iter().enumerate() {
                    assert_eq!(request.sequence, index as i64 + 1);
                    assert_eq!(request.delta, i64::MIN);
                    assert_eq!(request.operation, 2);
                }
            }
        }
    }
}
