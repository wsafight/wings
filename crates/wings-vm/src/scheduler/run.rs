use super::locks::LockPhase;
use super::*;
use crate::timer::DeadlineTimer;
use crate::{ExecutionStatus, PreemptionToken, RequestOutcome, ScalarTask};
use std::collections::VecDeque;

pub(super) struct Running {
    pid: SchedulerPid,
    task: ScalarTask,
    message: Envelope,
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
    let preemption = shared
        .observations
        .get(index)
        .map(|observer| observer.token.clone())
        .unwrap_or_default();
    let setup = (|| {
        let pool = StackPool::new_on_demand(
            &shared.tenant,
            shared.config.capacity,
            2.min(shared.config.capacity),
        )?;
        let timer = DeadlineTimer::new(&shared.tenant, &preemption)?;
        let mut local = VecDeque::new();
        local
            .try_reserve_exact(shared.config.capacity)
            .map_err(|_| "scheduler active queue allocation failed")?;
        if local.capacity() != shared.config.capacity {
            return Err("allocator changed scheduler active queue capacity".into());
        }
        Ok::<_, String>((pool, timer, local))
    })();
    let (pool, timer, mut local): (_, _, VecDeque<Running>) = match setup {
        Ok(value) => {
            let _ = ready.send(Ok(()));
            value
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    loop {
        let mut progress = false;
        let claim = {
            let mut state = shared.worker_lock(index, LockPhase::Claim);
            state.maintain(shared.identity);
            if state.closed {
                None
            } else if let Some(slot) = state.pop_ready(index) {
                let pid = state.pid(shared.identity, slot);
                let actor = state.slots[slot].actor.as_mut().expect("ready actor");
                assert!(!actor.stopping && actor.owner.is_none() && actor.mailbox.count > 0);
                actor.owner = Some(index);
                actor.starting = true;
                Some(Claim {
                    pid,
                    image: actor.image.take().expect("unowned actor image"),
                    message: actor.mailbox.pop(),
                    account: actor.account.clone(),
                    cancellation: actor.cancellation.clone(),
                })
            } else {
                None
            }
        };
        let attempted = claim.is_some();
        if let Some(claim) = claim {
            if let Some(active) = start(&shared, index, claim, &pool, &preemption) {
                assert!(local.len() < shared.config.capacity);
                local.push_back(active);
            }
        }
        if let Some(mut active) = local.pop_front() {
            progress = true;
            let status = match Instant::now()
                .checked_add(shared.config.quantum)
                .and_then(|deadline| timer.arm(deadline).ok())
            {
                Some(()) => {
                    let result = active.task.resume(shared.config.slice_steps);
                    timer.disarm();
                    if let Some(observer) = shared.observations.get(index) {
                        observer.record(active.pid, index);
                    }
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
                    let mut state = shared.worker_lock(index, LockPhase::Snapshot);
                    if let Ok(actor) = state.actor_mut(shared.identity, active.pid) {
                        actor.execution = active.task.snapshot();
                    }
                    local.push_back(active);
                }
                Ok(ExecutionStatus::Complete(reply)) => {
                    finish(&shared, index, active, Ok(reply.map(RequestOutcome::Reply)))
                }
                Ok(ExecutionStatus::CompleteList(reply)) => finish(
                    &shared,
                    index,
                    active,
                    Ok(Some(RequestOutcome::ReplyList(reply))),
                ),
                Ok(ExecutionStatus::CompleteBytes(reply)) => finish(
                    &shared,
                    index,
                    active,
                    Ok(Some(RequestOutcome::ReplyBytes(reply))),
                ),
                Ok(ExecutionStatus::CompleteString(reply)) => finish(
                    &shared,
                    index,
                    active,
                    Ok(Some(RequestOutcome::ReplyString(reply))),
                ),
                Ok(ExecutionStatus::CompleteRecord(reply)) => finish(
                    &shared,
                    index,
                    active,
                    Ok(Some(RequestOutcome::ReplyRecord(reply))),
                ),
                Err(error) => finish(&shared, index, active, Err(error)),
            }
        }
        let mut state = shared.lock();
        if state.closed && local.is_empty() {
            break;
        }
        if state.should_park(index, progress, attempted) {
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
        let bound = pool.for_process(&claim.account)?;
        let process = claim.image.attach(&bound)?;
        let mut task = ScalarTask::new(process);
        task.enable_effects();
        match task.start_kind(
            &claim.message.handler,
            &claim.message.arguments[..claim.message.count],
            claim.message.max_steps,
            || (claim.cancellation.clone(), preemption.clone()),
            &bound,
            claim.message.system,
            claim.message.record_mask,
            &claim.message.lists,
            &mut claim.message.bytes,
        ) {
            Ok(()) => Ok(task),
            Err(error) => {
                claim.image = task.into_process().detach();
                Err(error)
            }
        }
    })();
    match result {
        Ok(task) => {
            let mut state = shared.worker_lock(index, LockPhase::Start);
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
            })
        }
        Err(_) => {
            let mut state = shared.worker_lock(index, LockPhase::Start);
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
            state.enqueue(claim.pid);
            drop(state);
            drop(claim.account);
            drop(claim.cancellation);
            drop(retired);
            None
        }
    }
}

fn finish(
    shared: &Shared,
    index: usize,
    active: Running,
    result: Result<Option<RequestOutcome>, String>,
) {
    let Running {
        pid,
        task,
        mut message,
    } = active;
    let snapshot = task.snapshot();
    let process = task.into_process();
    let committed_state =
        (!process.module().manifest().state.is_reference()).then_some(process.state);
    let image = process.detach();
    let byte_state = image.byte_state_snapshot();
    let mut state = shared.worker_lock(index, LockPhase::Finish);
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
            byte_state,
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
    actor.byte_state = byte_state;
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
        Ok(Some(value)) if !failed => value,
        _ => RequestOutcome::Failed,
    };
    match std::mem::replace(&mut message.reply, Reply::None) {
        Reply::External(publisher) => match outcome {
            RequestOutcome::Reply(value) => publisher.complete(value),
            RequestOutcome::ReplyList(value) => publisher.complete_list(value),
            RequestOutcome::ReplyBytes(value) => publisher.complete_bytes(value),
            RequestOutcome::ReplyString(value) => publisher.complete_string(value),
            RequestOutcome::ReplyRecord(value) => publisher.complete_record(value),
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
    state.enqueue(pid);
    state.maintain(shared.identity);
    drop(state);
    drop(message);
    drop(retired);
    shared.changed.notify_all();
}
