use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    ByteStateSnapshot, ExecutionFault, IngressRequest, RequestOutcome, ResourceAccount, Resources,
    RestartPolicy, ScalarModule, SchedulerConfig, SchedulerExitReason, SchedulerSupervisor,
    SchedulerTermination, StackPool,
};

const MAX_REQUESTS: usize = 256;
const MAX_INPUT: usize = 40 + MAX_REQUESTS * 40;
const WINDOW: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
struct Request {
    actor: usize,
    sequence: i64,
    operation: i64,
    delta: i64,
}

struct Workload {
    actors: usize,
    state_bytes: usize,
    initial: i64,
    count: usize,
    requests: [Request; MAX_REQUESTS],
}

fn word(input: &[u8], index: usize) -> u64 {
    u64::from_be_bytes(
        input[index * 8..index * 8 + 8]
            .try_into()
            .expect("word range"),
    )
}

fn decode(input: &[u8]) -> Result<Workload, String> {
    if input.len() < 40 || input.len() > MAX_INPUT {
        return Err("input_bytes".into());
    }
    if word(input, 0) != 2 {
        return Err("version".into());
    }
    let state_bytes = usize::try_from(word(input, 1)).map_err(|_| "state_bytes")?;
    let actors = usize::try_from(word(input, 3)).map_err(|_| "actors")?;
    let count = usize::try_from(word(input, 4)).map_err(|_| "requests")?;
    if ![256, 4096, 65536].contains(&state_bytes)
        || !(1..=10000).contains(&actors)
        || actors * state_bytes > 16 << 20
        || count > MAX_REQUESTS
        || input.len() != 40 + count * 40
    {
        return Err("workload_limits".into());
    }
    let mut requests = [Request::default(); MAX_REQUESTS];
    for (index, request) in requests[..count].iter_mut().enumerate() {
        let offset = 5 + index * 5;
        let actor = usize::try_from(word(input, offset)).map_err(|_| "actor")?;
        let sequence = word(input, offset + 1);
        let operation = word(input, offset + 2);
        if !(1..=actors).contains(&actor)
            || sequence != index as u64 + 1
            || ![1, 2].contains(&operation)
            || word(input, offset + 4) != 0
        {
            return Err("request_record".into());
        }
        *request = Request {
            actor,
            sequence: sequence as i64,
            operation: operation as i64,
            delta: word(input, offset + 3) as i64,
        };
    }
    Ok(Workload {
        actors,
        state_bytes,
        initial: word(input, 2) as i64,
        count,
        requests,
    })
}

fn limits() -> Resources {
    Resources {
        bytes: 256 << 20,
        processes: 10000,
        messages: 256,
        requests: 256,
    }
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        return Err("multi Counter deadline".into());
    }
    Ok(duration)
}

fn wait(mut request: IngressRequest, deadline: Instant) -> Result<RequestOutcome, String> {
    request
        .wait(remaining(deadline)?)?
        .ok_or("multi Counter incomplete request".into())
}

fn state(
    snapshot: Option<ByteStateSnapshot>,
    length: usize,
    identity: usize,
) -> Result<i64, String> {
    let snapshot = snapshot.ok_or("missing multi Counter byte state")?;
    if snapshot.length != length
        || u64::from_be_bytes(snapshot.prefix[8..16].try_into().expect("identity"))
            != identity as u64
        || snapshot.prefix[16..].iter().any(|byte| *byte != 0)
    {
        return Err("multi Counter state layout mismatch".into());
    }
    Ok(i64::from_be_bytes(
        snapshot.prefix[..8].try_into().expect("counter"),
    ))
}

fn execute(directory: &Path, workload: &Workload) -> Result<(i32, Value), String> {
    let vm = ResourceAccount::vm_accounted(limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(limits())?;
    let mut supervisor = SchedulerSupervisor::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: workload.actors,
            requests: WINDOW,
            slice_steps: 64,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = supervisor.handle();
    let configure = module.handler("configure")?;
    let apply = module.handler("apply")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let initializer_pool = StackPool::new_accounted(&tenant, 1, 1)?;
    let mut pids = Vec::with_capacity(workload.actors);
    let mut children = Vec::with_capacity(workload.actors);
    let mut mailboxes = vec![1; workload.actors];
    for batch in workload.requests[..workload.count].chunks(WINDOW) {
        for request in batch {
            mailboxes[request.actor - 1] = mailboxes[request.actor - 1].max(
                batch
                    .iter()
                    .filter(|other| other.actor == request.actor)
                    .count(),
            );
        }
    }
    for identity in 1..=workload.actors {
        remaining(deadline)?;
        let child = supervisor.spawn_with_heap_in_pool(
            &module,
            Resources {
                bytes: 4 << 20,
                ..limits()
            },
            mailboxes[identity - 1],
            4,
            RestartPolicy::Never,
            &initializer_pool,
        )?;
        children.push(child);
        pids.push(supervisor.current_pid(child)?);
        if identity % WINDOW == 0 || identity == workload.actors {
            let start = identity - (identity - 1) % WINDOW - 1;
            let mut pending = Vec::with_capacity(WINDOW);
            for (index, pid) in pids.iter().enumerate().skip(start) {
                pending.push(handle.request(
                    *pid,
                    &configure,
                    &[
                        workload.state_bytes as i64,
                        workload.initial,
                        index as i64 + 1,
                    ],
                    4096,
                    remaining(deadline)?,
                )?);
            }
            for request in pending {
                if wait(request, deadline)? != RequestOutcome::Reply(workload.initial) {
                    return Err("multi Counter configure failed".into());
                }
            }
        }
    }
    let mut failures = vec![None; workload.actors];
    let mut committed = vec![workload.initial; workload.actors];
    let mut replies = Vec::with_capacity(workload.count);
    for batch in workload.requests[..workload.count].chunks(WINDOW) {
        let mut pending = Vec::with_capacity(WINDOW);
        for request in batch {
            pending.push(if failures[request.actor - 1].is_none() {
                match handle.request(
                    pids[request.actor - 1],
                    &apply,
                    &[request.sequence, request.operation, request.delta, 0],
                    4096,
                    remaining(deadline)?,
                ) {
                    Ok(request) => Some(request),
                    Err(_) if handle.snapshot(pids[request.actor - 1]).is_err() => None,
                    Err(error) => return Err(error),
                }
            } else {
                None
            });
        }
        for (request, pending) in batch.iter().zip(pending) {
            let Some(pending) = pending else { continue };
            match wait(pending, deadline)? {
                RequestOutcome::Reply(value) => replies
                    .push(json!({"actor": request.actor, "seq": request.sequence, "value": value})),
                RequestOutcome::Failed => {
                    if failures[request.actor - 1].is_none() {
                        loop {
                            remaining(deadline)?;
                            let Some(event) = supervisor.poll() else {
                                std::thread::yield_now();
                                continue;
                            };
                            let identity = children
                                .iter()
                                .position(|child| *child == event.child)
                                .ok_or("foreign multi Counter exit")?;
                            let SchedulerTermination::Failed(exit) = event.termination else {
                                return Err("unexpected multi Counter stop".into());
                            };
                            if exit.handler() != "apply" {
                                return Err("multi Counter failure handler".into());
                            }
                            let reason = match exit.reason {
                                SchedulerExitReason::Managed(ExecutionFault::Overflow) => {
                                    "overflow"
                                }
                                SchedulerExitReason::Managed(ExecutionFault::DivisionByZero) => {
                                    "injected_failure"
                                }
                                _ => return Err("multi Counter unexpected managed failure".into()),
                            };
                            committed[identity] =
                                state(exit.byte_state, workload.state_bytes, identity + 1)?;
                            failures[identity] = Some(reason);
                            if identity == request.actor - 1 {
                                break;
                            }
                        }
                    }
                }
                _ => return Err("multi Counter unexpected terminal result".into()),
            }
        }
    }
    let mut reported = Vec::new();
    for (index, pid) in pids.iter().enumerate() {
        if let Some(reason) = failures[index] {
            let successful = replies
                .iter()
                .filter(|reply| reply["actor"] == index + 1)
                .count();
            let failed = workload.requests[..workload.count]
                .iter()
                .filter(|request| request.actor == index + 1)
                .nth(successful)
                .ok_or("missing failed input")?;
            reported
                .push(json!({"actor": index + 1, "reason": reason, "failed_seq": failed.sequence}));
        } else {
            committed[index] = state(
                handle.byte_state_snapshot(*pid)?,
                workload.state_bytes,
                index + 1,
            )?;
            supervisor.stop(children[index])?;
        }
    }
    let code = i32::from(!reported.is_empty());
    drop((
        handle,
        supervisor,
        configure,
        apply,
        initializer_pool,
        module,
        tenant,
    ));
    if vm.used() != bootstrap {
        return Err("multi Counter resource leak".into());
    }
    Ok((
        code,
        json!({"version": 2, "status": if code == 0 {"ok"} else {"failure"},
        "actors": workload.actors, "state_bytes": workload.state_bytes, "message_bytes": 32,
        "committed": committed, "replies": replies, "failures": reported, "states_verified": true,
        "resources_released": true, "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        "environment": {"runtime": "wings", "workers": 2, "window": WINDOW}}),
    ))
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        eprintln!("usage: beam_multi_counter --trusted ARTIFACT");
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
        Ok(workload) => execute(Path::new(&arguments[1]), &workload).unwrap_or_else(|error| {
            eprintln!("{error}");
            (3, json!({"version": 2, "status": "oracle_error"}))
        }),
        Err(_) => (2, json!({"version": 2, "status": "invalid"})),
    };
    let mut output_stream = std::io::stdout().lock();
    let _ = writeln!(output_stream, "{output}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_bridge_rejects_lengths_limits_routes_and_unknown_fields() {
        let words = [2u64, 256, 0, 2, 1, 1, 1, 1, 7, 0];
        let input: Vec<_> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(decode(&input).unwrap().requests[0].delta, 7);
        for (index, value) in [
            (0, 1),
            (1, 257),
            (3, 0),
            (3, 10001),
            (4, 257),
            (5, 3),
            (6, 2),
            (7, 3),
            (9, 1),
        ] {
            let mut changed = input.clone();
            changed[index * 8..index * 8 + 8].copy_from_slice(&u64::to_be_bytes(value));
            assert!(decode(&changed).is_err());
        }
        for length in [0, 1, 39, 40, 79] {
            assert!(decode(&input[..length]).is_err());
        }
    }
}
