use std::sync::Barrier;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wings_vm::{
    ByteStateSnapshot, ExecutionFault, RequestOutcome, ResourceAccount, Resources, RestartPolicy,
    ScalarModule, SchedulerChildId, SchedulerConfig, SchedulerExitReason, SchedulerPid,
    SchedulerRestartStatus, SchedulerSupervisor, SchedulerTermination, StackPool,
};

const WINDOW: usize = 16;
pub(super) const MAX_INPUT: usize = 48 + 256 * 32;

#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub(super) actor: usize,
    pub(super) sequence: i64,
    pub(super) delta: i64,
    pub(super) operation: i64,
}

pub(super) struct Workload {
    pub(super) actors: usize,
    pub(super) bytes: usize,
    pub(super) initial: i64,
    pub(super) streams: [Vec<Request>; 2],
}

#[derive(Clone, Copy)]
struct Committed {
    counter: i64,
    sequences: [i64; 2],
}

fn word(input: &[u8], index: usize) -> u64 {
    u64::from_be_bytes(input[index * 8..index * 8 + 8].try_into().expect("word"))
}

pub(super) fn decode(input: &[u8]) -> Result<Workload, String> {
    if input.len() < 48 || input.len() > MAX_INPUT || word(input, 0) != 9 {
        return Err("header".into());
    }
    let bytes = usize::try_from(word(input, 1)).map_err(|_| "bytes")?;
    let actors = usize::try_from(word(input, 3)).map_err(|_| "actors")?;
    let counts = [word(input, 4), word(input, 5)];
    if ![256, 4096, 65536].contains(&bytes)
        || !(1..=100).contains(&actors)
        || counts.iter().any(|count| *count > 128)
        || input.len() != 48 + (counts[0] + counts[1]) as usize * 32
    {
        return Err("limits".into());
    }
    let initial = word(input, 2) as i64;
    let mut bounds = vec![[i128::from(initial); 2]; actors];
    let mut streams = [Vec::new(), Vec::new()];
    let mut offset = 6;
    for (stream, count) in streams.iter_mut().zip(counts) {
        let mut sequences = vec![0; actors];
        for _ in 0..count {
            let actor = usize::try_from(word(input, offset)).map_err(|_| "actor")?;
            let sequence = word(input, offset + 1);
            let delta = word(input, offset + 2) as i64;
            let operation = word(input, offset + 3);
            if !(1..=actors).contains(&actor)
                || sequence != sequences[actor - 1] + 1
                || ![1, 2].contains(&operation)
            {
                return Err("request".into());
            }
            sequences[actor - 1] = sequence;
            bounds[actor - 1][0] += i128::from(delta.min(0));
            bounds[actor - 1][1] += i128::from(delta.max(0));
            stream.push(Request {
                actor,
                sequence: sequence as i64,
                delta,
                operation: operation as i64,
            });
            offset += 4;
        }
    }
    if bounds
        .iter()
        .any(|bound| bound[0] < i128::from(i64::MIN) || bound[1] > i128::from(i64::MAX))
    {
        return Err("unsafe_interleaving".into());
    }
    Ok(Workload {
        actors,
        bytes,
        initial,
        streams,
    })
}

pub(super) fn limits() -> Resources {
    Resources {
        bytes: 256 << 20,
        processes: 100,
        messages: 256,
        requests: 256,
    }
}

pub(super) fn remaining(deadline: Instant) -> Result<Duration, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err("concurrent failure Counter deadline".into())
    } else {
        Ok(remaining)
    }
}

fn state(
    snapshot: Option<ByteStateSnapshot>,
    bytes: usize,
    identity: usize,
) -> Result<Committed, String> {
    let snapshot = snapshot.ok_or("missing concurrent failure byte state")?;
    if snapshot.length != bytes
        || u64::from_be_bytes(snapshot.prefix[8..16].try_into().expect("identity"))
            != identity as u64
    {
        return Err("concurrent failure state layout".into());
    }
    let sequences = [16, 24].map(|offset| {
        i64::from_be_bytes(
            snapshot.prefix[offset..offset + 8]
                .try_into()
                .expect("sequence"),
        )
    });
    if sequences
        .iter()
        .any(|sequence| !(0..=128).contains(sequence))
    {
        return Err("concurrent failure state sequence".into());
    }
    Ok(Committed {
        counter: i64::from_be_bytes(snapshot.prefix[..8].try_into().expect("counter")),
        sequences,
    })
}

pub(super) struct CompletedEpoch {
    pub(super) result: Value,
    pub(super) pids: Vec<SchedulerPid>,
    pub(super) children: Vec<SchedulerChildId>,
    pub(super) deadline: Instant,
}

impl CompletedEpoch {
    pub(super) fn reject_stale(
        &self,
        module: &ScalarModule,
        supervisor: &SchedulerSupervisor,
    ) -> Result<usize, String> {
        let handle = supervisor.handle();
        let apply = module.handler("apply")?;
        for (child, pid) in self.children.iter().zip(&self.pids) {
            if supervisor.current_pid(*child).is_ok()
                || handle.snapshot(*pid).is_ok()
                || handle.send(*pid, &apply, &[1, 1, 0, 1], 4096).is_ok()
                || handle
                    .request(*pid, &apply, &[1, 1, 0, 1], 4096, Duration::from_secs(3))
                    .is_ok()
            {
                return Err("concurrent failure accepted stale identity".into());
            }
        }
        Ok(self.pids.len())
    }
}

pub(super) fn supervisor(
    tenant: &ResourceAccount,
    actors: usize,
) -> Result<SchedulerSupervisor, String> {
    SchedulerSupervisor::new(
        tenant,
        SchedulerConfig {
            workers: 2,
            capacity: actors,
            requests: 2 * WINDOW,
            ..SchedulerConfig::default()
        },
    )
}

pub(super) fn epoch(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    supervisor: &mut SchedulerSupervisor,
    workload: &Workload,
    previous: Option<&CompletedEpoch>,
    deadline: Option<Instant>,
) -> Result<CompletedEpoch, String> {
    let baseline = tenant.used();
    let handle = supervisor.handle();
    let configure = module.handler("configure")?;
    let apply = module.handler("apply")?;
    let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3));
    let pool = StackPool::new_accounted(tenant, 1, 1)?;
    let mut pids = Vec::with_capacity(workload.actors);
    let mut children = Vec::with_capacity(workload.actors);
    for identity in 1..=workload.actors {
        let child = supervisor.spawn_with_heap_in_pool(
            module,
            Resources {
                bytes: 4 << 20,
                ..limits()
            },
            2 * WINDOW,
            4,
            RestartPolicy::Never,
            &pool,
        )?;
        let pid = supervisor.current_pid(child)?;
        let mut request = handle.request(
            pid,
            &configure,
            &[workload.bytes as i64, workload.initial, identity as i64],
            4096,
            remaining(deadline)?,
        )?;
        if request.wait(remaining(deadline)?)? != Some(RequestOutcome::Reply(workload.initial)) {
            return Err("concurrent failure configure".into());
        }
        children.push(child);
        pids.push(pid);
    }
    drop(pool);
    if let Some(previous) = previous {
        previous.reject_stale(module, supervisor)?;
        if pids.iter().any(|pid| previous.pids.contains(pid)) {
            return Err("concurrent failure reused full Pid".into());
        }
    }
    let barrier = Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let mut senders = Vec::with_capacity(2);
        for (sender, stream) in workload.streams.iter().enumerate() {
            let handle = &handle;
            let pids = &pids;
            let apply = &apply;
            let barrier = &barrier;
            senders.push(scope.spawn(move || {
                barrier.wait();
                let mut outcomes = Vec::with_capacity(stream.len());
                for batch in stream.chunks(WINDOW) {
                    let mut pending = Vec::with_capacity(batch.len());
                    for request in batch {
                        let pid = pids[request.actor - 1];
                        pending.push(
                            match handle.request(
                                pid,
                                apply,
                                &[
                                    sender as i64 + 1,
                                    request.sequence,
                                    request.delta,
                                    request.operation,
                                ],
                                4096,
                                remaining(deadline)?,
                            ) {
                                Ok(request) => Some(request),
                                Err(_) if handle.snapshot(pid).is_err() => None,
                                Err(error) => return Err::<Vec<Value>, String>(error),
                            },
                        );
                    }
                    for (request, pending) in batch.iter().zip(pending) {
                        let (status, value) = if let Some(mut pending) = pending {
                            let outcome = pending.wait(remaining(deadline)?)?;
                            if pending.try_take().is_ok() {
                                return Err("concurrent failure result consumed twice".into());
                            }
                            match outcome {
                                Some(RequestOutcome::Reply(value)) => ("reply", Some(value)),
                                Some(RequestOutcome::Failed) => ("failed", None),
                                _ => {
                                    return Err(
                                        "concurrent failure unexpected terminal result".into()
                                    );
                                }
                            }
                        } else {
                            ("closed", None)
                        };
                        outcomes.push(json!({"sender": sender + 1, "actor": request.actor,
                            "seq": request.sequence, "status": status, "value": value}));
                    }
                }
                Ok(outcomes)
            }));
        }
        let mut outcomes = Vec::with_capacity(256);
        for sender in senders {
            outcomes.extend(
                sender
                    .join()
                    .map_err(|_| "concurrent failure host thread")??,
            );
        }
        Ok::<_, String>(outcomes)
    })?;
    let expected: Vec<_> = (1..=workload.actors)
        .map(|identity| {
            workload
                .streams
                .iter()
                .flatten()
                .any(|request| request.actor == identity && request.operation == 2)
        })
        .collect();
    let count = expected.iter().filter(|failed| **failed).count();
    let mut states = vec![None; workload.actors];
    let mut failures = Vec::with_capacity(count);
    while failures.len() < count {
        remaining(deadline)?;
        let Some(event) = supervisor.poll() else {
            std::thread::yield_now();
            continue;
        };
        let identity = children
            .iter()
            .position(|child| *child == event.child)
            .ok_or("foreign failure child")?;
        let SchedulerTermination::Failed(exit) = event.termination else {
            return Err("concurrent failure unexpected exit".into());
        };
        if event.pid != pids[identity]
            || event.restart != SchedulerRestartStatus::Disabled
            || !expected[identity]
            || states[identity].is_some()
            || exit.handler() != "apply"
            || exit.reason != SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
        {
            return Err("concurrent failure unexpected diagnostic".into());
        }
        states[identity] = Some(state(exit.byte_state, workload.bytes, identity + 1)?);
        failures.push(json!({"actor": identity + 1, "reason": "injected_failure"}));
    }
    for (identity, pid) in pids.iter().enumerate() {
        remaining(deadline)?;
        if states[identity].is_none() {
            states[identity] = Some(state(
                handle.byte_state_snapshot(*pid)?,
                workload.bytes,
                identity + 1,
            )?);
            supervisor.stop(children[identity])?;
        } else if handle.snapshot(*pid).is_ok() {
            return Err("concurrent failure Pid retained".into());
        }
    }
    let states: Vec<_> = states
        .into_iter()
        .map(|state| state.expect("verified actor"))
        .collect();
    remaining(deadline)?;
    loop {
        remaining(deadline)?;
        let used = tenant.used();
        if used.processes == baseline.processes
            && used.messages == baseline.messages
            && used.requests == baseline.requests
        {
            break;
        }
        std::thread::yield_now();
    }
    for (child, pid) in children.iter().zip(&pids) {
        if supervisor.current_pid(*child).is_ok() || handle.snapshot(*pid).is_ok() {
            return Err("concurrent failure retained terminated identity".into());
        }
    }
    drop((handle, configure, apply));
    Ok(CompletedEpoch {
        pids,
        children,
        deadline,
        result: json!({"version": 9, "status": if count == 0 {"ok"} else {"failure"},
            "actors": workload.actors, "state_bytes": workload.bytes, "message_bytes": 32,
            "committed": states.iter().map(|state| state.counter).collect::<Vec<_>>(),
            "sequences": states.iter().map(|state| state.sequences).collect::<Vec<_>>(),
            "outcomes": outcomes, "failures": failures, "states_verified": true,
            "environment": {"runtime": "wings", "senders": 2, "window_per_sender": WINDOW, "workers": 2}}),
    })
}
