use super::*;
use crate::timer::DeadlineTimer;
use crate::{ExecutionStatus, PreemptionToken, RequestOutcome, ScalarTask};

pub(super) struct Running {
    pid: SchedulerPid,
    task: ScalarTask,
    message: Envelope,
    _charge: Reservation,
}

struct Claim {
    pid: SchedulerPid,
    image: DetachedProcess,
    message: Envelope,
    account: ResourceAccount,
    cancellation: CancellationToken,
}

struct WorkerExit(Arc<Shared>);

impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(super) fn worker(
    shared: Arc<Shared>,
    index: usize,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let _exit = WorkerExit(shared.clone());
    let preemption = PreemptionToken::default();
    let setup = (|| {
        let pool = StackPool::new_accounted(
            &shared.tenant,
            shared.config.capacity,
            2.min(shared.config.capacity),
        )?;
        let timer = DeadlineTimer::new(&shared.tenant, &preemption)?;
        let local = storage(shared.config.capacity, || None)?;
        Ok::<_, String>((pool, timer, local))
    })();
    let (pool, timer, mut local): (_, _, Box<[Option<Running>]>) = match setup {
        Ok(value) => {
            let _ = ready.send(Ok(()));
            value
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut cursor = index % shared.config.capacity;
    loop {
        let mut progress = false;
        for _ in 0..local.len() {
            let slot = cursor;
            cursor = (cursor + 1) % local.len();
            let claim = {
                let mut state = shared.lock();
                state.maintain(shared.identity);
                if state.closed {
                    None
                } else if local[slot].is_none() {
                    let pid = state.pid(shared.identity, slot);
                    state.slots[slot]
                        .actor
                        .as_mut()
                        .filter(|actor| {
                            !actor.stopping
                                && actor.owner.is_none()
                                && actor.mailbox.count > 0
                                && actor.last_worker.is_none_or(|previous| {
                                    (previous + 1) % shared.config.workers == index
                                })
                        })
                        .map(|actor| {
                            actor.owner = Some(index);
                            actor.starting = true;
                            Claim {
                                pid,
                                image: actor.image.take().expect("unowned actor image"),
                                message: actor.mailbox.pop(),
                                account: actor.account.clone(),
                                cancellation: actor.cancellation.clone(),
                            }
                        })
                } else {
                    None
                }
            };
            if let Some(claim) = claim {
                local[slot] = start(&shared, index, claim, &pool, &preemption);
            }
            if let Some(mut active) = local[slot].take() {
                progress = true;
                let status = match Instant::now()
                    .checked_add(shared.config.quantum)
                    .and_then(|deadline| timer.arm(deadline).ok())
                {
                    Some(()) => {
                        let result = active.task.resume(shared.config.slice_steps);
                        timer.disarm();
                        result
                    }
                    None => Err("scheduler timer failure".into()),
                };
                match status {
                    Ok(ExecutionStatus::Suspended) => {
                        if let Some(effect) = active.task.take_effect() {
                            let (status, value) =
                                shared.effect(active.pid, effect, active.message.max_steps, &pool);
                            active.task.acknowledge_effect(status, value);
                        }
                        let mut state = shared.lock();
                        if let Ok(actor) = state.actor_mut(shared.identity, active.pid) {
                            actor.execution = active.task.snapshot();
                        }
                        local[slot] = Some(active);
                    }
                    Ok(ExecutionStatus::Complete(reply)) => {
                        finish(&shared, index, active, Ok(reply))
                    }
                    Err(error) => finish(&shared, index, active, Err(error)),
                }
            }
        }
        let mut state = shared.lock();
        if state.closed && local.iter().all(Option::is_none) {
            break;
        }
        if !progress {
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(1))
                .unwrap_or_else(|error| error.into_inner())
                .0;
            drop(state);
        }
    }
}

fn start(
    shared: &Shared,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    preemption: &PreemptionToken,
) -> Option<Running> {
    let result = (|| {
        let charge = claim.account.reserve(Resources {
            bytes: INVOCATION_BYTES,
            ..Resources::default()
        })?;
        let bound = pool.for_process(&claim.account)?;
        let process = claim.image.attach(&bound)?;
        let mut task = ScalarTask::new(process);
        task.enable_effects();
        match task.start_kind(
            &claim.message.handler,
            &claim.message.arguments[..claim.message.count],
            claim.message.max_steps,
            &claim.cancellation,
            preemption,
            &bound,
            claim.message.system,
            claim.message.record_mask,
            &claim.message.lists,
        ) {
            Ok(()) => Ok((task, charge)),
            Err(error) => {
                claim.image = task.into_process().detach();
                Err(error)
            }
        }
    })();
    match result {
        Ok((task, charge)) => {
            let mut state = shared.lock();
            let actor = state
                .actor_mut(shared.identity, claim.pid)
                .expect("claimed actor");
            assert_eq!(actor.owner, Some(index));
            actor.starting = false;
            if actor.last_worker.is_some_and(|previous| previous != index) {
                actor.migrations += 1;
            }
            actor.last_worker = Some(index);
            Some(Running {
                pid: claim.pid,
                task,
                message: claim.message,
                _charge: charge,
            })
        }
        Err(_) => {
            let mut state = shared.lock();
            let closed = state.closed;
            let actor = state
                .actor_mut(shared.identity, claim.pid)
                .expect("failed claim actor");
            actor.owner = None;
            actor.starting = false;
            actor.image = Some(claim.image);
            actor.mailbox.restore(claim.message);
            if closed {
                if let Some(publisher) = actor.termination.as_mut() {
                    publisher.set(SchedulerTermination::Closed);
                }
            }
            let retired = if actor.stopping || closed {
                state.retire(shared.identity, claim.pid)
            } else {
                None
            };
            drop(state);
            drop(claim.account);
            drop(claim.cancellation);
            drop(retired);
            None
        }
    }
}

fn finish(shared: &Shared, index: usize, active: Running, result: Result<Option<i64>, String>) {
    let Running {
        pid,
        task,
        mut message,
        _charge: charge,
    } = active;
    let snapshot = task.snapshot();
    let process = task.into_process();
    let committed_state =
        (!process.module().manifest().state.is_reference()).then_some(process.state);
    let image = process.detach();
    let mut state = shared.lock();
    let closed = state.closed;
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("finishing actor");
    assert_eq!(actor.owner, Some(index));
    let failed = result.is_err() || actor.stopping || closed;
    let diagnostic = if result.is_err() && !actor.stopping && !closed {
        let name = &message.handler.schema().name;
        let mut handler = [0; 256];
        handler[..name.len()].copy_from_slice(name.as_bytes());
        Some(SchedulerExitSnapshot {
            pid,
            worker: index,
            reason: snapshot
                .and_then(|execution| execution.fault)
                .map(SchedulerExitReason::Managed)
                .unwrap_or(SchedulerExitReason::ExecutionControl),
            execution: snapshot,
            state_type: actor.module.manifest().state,
            committed_state,
            queued: actor.mailbox.count,
            resources_before_cleanup: actor.account.used(),
            handler,
            handler_len: name.len(),
        })
    } else {
        None
    };
    actor.owner = None;
    actor.image = Some(image);
    actor.execution = snapshot;
    actor.committed_state = committed_state;
    if failed {
        if let Some(publisher) = actor.termination.as_mut() {
            publisher.set(if closed {
                SchedulerTermination::Closed
            } else if let Some(diagnostic) = diagnostic {
                SchedulerTermination::Failed(diagnostic)
            } else {
                SchedulerTermination::Stopped
            });
        }
    }
    if let Some(diagnostic) = diagnostic {
        state.last_exit = Some(diagnostic);
    }
    let outcome = match result {
        Ok(Some(value)) if !failed => RequestOutcome::Reply(value),
        _ => RequestOutcome::Failed,
    };
    match std::mem::replace(&mut message.reply, Reply::None) {
        Reply::External(publisher) => match outcome {
            RequestOutcome::Reply(value) => publisher.complete(value),
            _ => drop(publisher),
        },
        Reply::Language(key) => state.finish(key, outcome),
        Reply::None => {}
    }
    let retired = if failed {
        state.retire(shared.identity, pid)
    } else {
        None
    };
    state.maintain(shared.identity);
    drop(state);
    drop(message);
    drop(charge);
    drop(retired);
    shared.changed.notify_all();
}
