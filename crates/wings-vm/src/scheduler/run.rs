use super::locks::LockPhase;
use super::ready::WAKE_GLOBAL_BIT;
use super::*;
use crate::execution::WorkerFibers;
use crate::timer::DeadlineTimer;
use crate::{ExecutionStatus, PreemptionToken, RequestOutcome, ScalarProcess, ScalarTask};
use std::collections::VecDeque;

const HOT_SPIN_WINDOW: Duration = Duration::from_micros(25);
const BACKLOG_BATCH_THRESHOLD: usize = 8;
const MAX_BACKLOG_BATCH: usize = 16;

pub(super) struct Running {
    pid: SchedulerPid,
    task: ScalarTask,
    handler: HandlerId,
    max_steps: u64,
    reply: Reply,
    _charge: Reservation,
    _message_charge: Option<Reservation>,
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

fn map_outcome(result: Result<ExecutionStatus, String>) -> Result<Option<RequestOutcome>, String> {
    match result {
        Ok(ExecutionStatus::Suspended) => unreachable!(),
        Ok(ExecutionStatus::Complete(reply)) => Ok(reply.map(RequestOutcome::Reply)),
        Ok(ExecutionStatus::CompleteList(reply)) => Ok(Some(RequestOutcome::ReplyList(reply))),
        Ok(ExecutionStatus::CompleteBytes(reply)) => Ok(Some(RequestOutcome::ReplyBytes(reply))),
        Ok(ExecutionStatus::CompleteString(reply)) => Ok(Some(RequestOutcome::ReplyString(reply))),
        Ok(ExecutionStatus::CompleteRecord(reply)) => Ok(Some(RequestOutcome::ReplyRecord(reply))),
        Err(error) => Err(error),
    }
}

pub(super) fn worker(
    shared: Arc<Shared>,
    index: usize,
    ready: mpsc::SyncSender<Result<(), String>>,
    stack_check: fn(usize) -> Result<(), &'static str>,
) {
    let _cpu = shared
        .lock_observations
        .get(index)
        .map(|observer| observer.cpu.begin());
    let _exit = WorkerExit(shared.clone());
    let preemption = shared
        .observations
        .get(index)
        .map(|observer| observer.token.clone())
        .unwrap_or_default();
    let setup = (|| {
        stack_check(THREAD_STACK + 65536)?;
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
        let fibers = WorkerFibers::new(&pool, &preemption);
        Ok::<_, String>((pool, timer, local, fibers))
    })();
    let (pool, timer, mut local, mut fibers): (_, _, VecDeque<Running>, _) = match setup {
        Ok(value) => {
            let _ = ready.send(Ok(()));
            value
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut hot_until: Option<Instant> = None;
    let watch = (1u32 << index) | WAKE_GLOBAL_BIT;
    loop {
        let (claim, wake, should_exit) = {
            let mut state = shared.worker_lock(index, LockPhase::Claim);
            let wake = state.maintain(shared.identity);
            if state.closed {
                (None, wake, local.is_empty())
            } else if let Some(slot) = state.pop_ready(index) {
                let pid = state.pid(shared.identity, slot);
                let actor = state.slots[slot].actor.as_mut().expect("ready actor");
                assert!(!actor.stopping && actor.owner.is_none() && actor.mailbox.count > 0);
                actor.owner = Some(index);
                actor.starting = true;
                (
                    Some(Claim {
                        pid,
                        image: actor.image.take().expect("unowned actor image"),
                        message: actor.mailbox.pop(),
                        account: actor.account.clone(),
                        cancellation: actor.cancellation.clone(),
                    }),
                    wake,
                    false,
                )
            } else {
                (None, wake, false)
            }
        };
        wake.fire(&shared.changed);
        if should_exit {
            break;
        }
        let attempted = claim.is_some();
        if local.is_empty()
            && claim.as_ref().is_some_and(|claim| {
                WorkerFibers::can_run_fast(
                    &claim.message.handler,
                    claim.message.record_mask,
                    &claim.message.lists,
                    &claim.message.bytes,
                )
            })
        {
            if run_fast_claim(
                &shared,
                index,
                claim.expect("checked fast claim"),
                &pool,
                &timer,
                &mut fibers,
                &mut local,
            ) {
                hot_until = Instant::now().checked_add(HOT_SPIN_WINDOW);
                continue;
            }
        } else {
            let next_active = match claim {
                Some(claim) => {
                    match start(&shared, index, claim, &pool, &preemption, &mut fibers) {
                        Some(started) if local.is_empty() => Some(started),
                        Some(started) => {
                            assert!(local.len() < shared.config.capacity);
                            local.push_back(started);
                            local.pop_front()
                        }
                        None => local.pop_front(),
                    }
                }
                None => local.pop_front(),
            };
            if let Some(mut active) = next_active {
                match Instant::now()
                    .checked_add(shared.config.quantum)
                    .and_then(|deadline| timer.arm(deadline).ok())
                {
                    Some(()) => {
                        let started_at =
                            shared.slice_observations.get(index).map(|_| Instant::now());
                        let result = active
                            .task
                            .resume_with_fibers(shared.config.slice_steps, &mut fibers);
                        if let Some(started_at) = started_at {
                            shared.slice_observations[index].record(
                                active.pid,
                                index,
                                started_at,
                                Instant::now(),
                                &result,
                            );
                        }
                        if let Some(observer) = shared.observations.get(index) {
                            observer.record(active.pid, index);
                        }
                        timer.disarm();
                        if matches!(result, Ok(ExecutionStatus::Suspended)) {
                            if let Some(effect) = active.task.take_effect() {
                                let (status, value) =
                                    shared.effect(active.pid, effect, active.max_steps, &pool);
                                active.task.acknowledge_effect(status, value);
                            }
                            let mut state = shared.worker_lock(index, LockPhase::Snapshot);
                            if let Ok(actor) = state.actor_mut(shared.identity, active.pid) {
                                actor.execution = active.task.snapshot();
                            }
                            local.push_back(active);
                        } else {
                            finish(&shared, index, active, map_outcome(result));
                        }
                    }
                    None => {
                        finish(
                            &shared,
                            index,
                            active,
                            Err("scheduler timer failure".into()),
                        );
                    }
                }
                hot_until = Instant::now().checked_add(HOT_SPIN_WINDOW);
                continue;
            }
        }
        if let (false, Some(spin_deadline)) = (attempted, hot_until) {
            while shared.ready_mask.load(Ordering::Acquire) & watch == 0 {
                for _ in 0..32 {
                    if shared.ready_mask.load(Ordering::Acquire) & watch != 0 {
                        break;
                    }
                    std::hint::spin_loop();
                }
                if shared.ready_mask.load(Ordering::Acquire) & watch != 0 {
                    break;
                }
                if Instant::now() >= spin_deadline {
                    hot_until = None;
                    break;
                }
            }
            if shared.ready_mask.load(Ordering::Acquire) & watch != 0 {
                continue;
            }
        }
        let mut state = shared.lock();
        if state.closed && local.is_empty() {
            shared.publish_signal(&state);
            break;
        }
        if state.should_park(index, false, attempted) {
            let bit = 1u32 << index;
            state.parked_mask |= bit;
            shared.publish_signal(&state);
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(1))
                .unwrap_or_else(|error| error.into_inner())
                .0;
            state.parked_mask &= !bit;
            shared.publish_signal(&state);
        }
        drop(state);
    }
}

fn mark_started(shared: &Shared, index: usize, pid: SchedulerPid) {
    let mut state = shared.worker_lock(index, LockPhase::Start);
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("claimed actor");
    assert_eq!(actor.owner, Some(index));
    actor.starting = false;
    if actor.last_worker.is_some_and(|previous| previous != index) {
        actor.migrations += 1;
    }
    actor.last_worker = Some(index);
}

fn run_fast_claim(
    shared: &Arc<Shared>,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    timer: &DeadlineTimer,
    fibers: &mut WorkerFibers,
    local: &mut VecDeque<Running>,
) -> bool {
    let mut process = match claim.image.attach_scheduled(pool, &claim.account) {
        Ok(process) => process,
        Err(_) => {
            rollback_failed_start(shared, index, claim);
            return false;
        }
    };
    let pid = claim.pid;
    let account = claim.account;
    let cancellation = claim.cancellation;
    let mut current_message = Some(claim.message);
    for batch_step in 0..MAX_BACKLOG_BATCH {
        let message = current_message.take().expect("active fast batch message");
        let prepared = match fibers.prepare(&process, &account) {
            Ok(prepared) => prepared,
            Err(_) => {
                rollback_failed_start(
                    shared,
                    index,
                    Claim {
                        pid,
                        image: process.detach(),
                        message,
                        account,
                        cancellation,
                    },
                );
                return batch_step > 0;
            }
        };
        if batch_step == 0 {
            mark_started(shared, index, pid);
        } else {
            let mut state = shared.worker_lock(index, LockPhase::Start);
            let actor = state
                .actor_mut(shared.identity, pid)
                .expect("batched actor");
            assert_eq!(actor.owner, Some(index));
            actor.starting = false;
        }
        let Envelope {
            handler,
            arguments,
            count,
            max_steps,
            mut reply,
            _charge,
            _message_charge,
            ..
        } = message;
        let armed = Instant::now()
            .checked_add(shared.config.quantum)
            .and_then(|deadline| timer.arm(deadline).ok());
        let Some(()) = armed else {
            drop(prepared);
            finish_process(
                shared,
                index,
                pid,
                process,
                None,
                handler,
                reply,
                _charge,
                _message_charge,
                Err("scheduler timer failure".into()),
            );
            return true;
        };
        let started_at = shared.slice_observations.get(index).map(|_| Instant::now());
        let step = prepared.run_first_slice(
            fibers,
            process,
            handler.index,
            &arguments[..count],
            max_steps,
            shared.config.slice_steps,
            &cancellation,
        );
        let returned_at = started_at.map(|_| Instant::now());
        timer.disarm();
        match step {
            Ok((next_process, snapshot, result)) => {
                process = next_process;
                if let (Some(started_at), Some(returned_at)) = (started_at, returned_at) {
                    shared.slice_observations[index].record(
                        pid,
                        index,
                        started_at,
                        returned_at,
                        &result,
                    );
                }
                if let Some(observer) = shared.observations.get(index) {
                    observer.record(pid, index);
                }
                let mapped = map_outcome(result);
                if batch_step + 1 < MAX_BACKLOG_BATCH && mapped.is_ok() {
                    let committed_state = (!process.module().manifest().state.is_reference())
                        .then_some(process.state);
                    let mut state = shared.worker_lock(index, LockPhase::Finish);
                    let can_continue = !state.closed && state.ready[index].head.is_none() && {
                        let actor = state
                            .actor(shared.identity, pid)
                            .expect("finishing batched actor");
                        !actor.stopping
                            && actor.mailbox.count >= BACKLOG_BATCH_THRESHOLD
                            && actor.mailbox.front_is_fast()
                    };
                    if can_continue {
                        let outcome = match mapped {
                            Ok(Some(value)) => value,
                            _ => RequestOutcome::Failed,
                        };
                        let actor = state
                            .actor_mut(shared.identity, pid)
                            .expect("continuing batched actor");
                        let next_msg = actor.mailbox.pop();
                        actor.starting = true;
                        actor.execution = Some(snapshot);
                        actor.committed_state = committed_state;
                        let mut external_reply = None;
                        match std::mem::replace(&mut reply, Reply::None) {
                            Reply::External(publisher) => {
                                external_reply = Some((publisher, outcome))
                            }
                            Reply::Language(key) => state.finish(key, outcome),
                            Reply::None => {}
                        }
                        let wake = state.maintain(shared.identity);
                        drop(state);
                        if let Some((publisher, outcome)) = external_reply {
                            match outcome {
                                RequestOutcome::Reply(value) => publisher.complete(value),
                                RequestOutcome::ReplyList(value) => publisher.complete_list(value),
                                RequestOutcome::ReplyBytes(value) => {
                                    publisher.complete_bytes(value)
                                }
                                RequestOutcome::ReplyString(value) => {
                                    publisher.complete_string(value)
                                }
                                RequestOutcome::ReplyRecord(value) => {
                                    publisher.complete_record(value)
                                }
                                _ => drop(publisher),
                            }
                        }
                        drop((handler, _charge, _message_charge));
                        wake.fire(&shared.changed);
                        current_message = Some(next_msg);
                        continue;
                    }
                }
                finish_process(
                    shared,
                    index,
                    pid,
                    process,
                    Some(snapshot),
                    handler,
                    reply,
                    _charge,
                    _message_charge,
                    mapped,
                );
                return true;
            }
            Err(boxed_task) => {
                let mut task = *boxed_task;
                if let (Some(started_at), Some(returned_at)) = (started_at, returned_at) {
                    let suspended_status = Ok(ExecutionStatus::Suspended);
                    shared.slice_observations[index].record(
                        pid,
                        index,
                        started_at,
                        returned_at,
                        &suspended_status,
                    );
                }
                if let Some(observer) = shared.observations.get(index) {
                    observer.record(pid, index);
                }
                if let Some(effect) = task.take_effect() {
                    let (status, value) = shared.effect(pid, effect, max_steps, pool);
                    task.acknowledge_effect(status, value);
                }
                let snapshot = task.snapshot();
                let mut state = shared.worker_lock(index, LockPhase::Snapshot);
                if let Ok(actor) = state.actor_mut(shared.identity, pid) {
                    actor.execution = snapshot;
                }
                drop(state);
                local.push_back(Running {
                    pid,
                    task,
                    handler,
                    max_steps,
                    reply,
                    _charge,
                    _message_charge,
                });
                return true;
            }
        }
    }
    unreachable!()
}

fn start(
    shared: &Shared,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    preemption: &PreemptionToken,
    fibers: &mut WorkerFibers,
) -> Option<Running> {
    if WorkerFibers::can_run_fast(
        &claim.message.handler,
        claim.message.record_mask,
        &claim.message.lists,
        &claim.message.bytes,
    ) {
        let ready = match claim.image.attach_scheduled(pool, &claim.account) {
            Ok(process) => match fibers.prepare(&process, &claim.account) {
                Ok(prepared) => match prepared.into_ready_task(
                    fibers,
                    process,
                    claim.message.handler.index,
                    &claim.message.arguments[..claim.message.count],
                    claim.message.max_steps,
                    &claim.cancellation,
                ) {
                    Ok(task) => Ok(task),
                    Err((process, _)) => {
                        claim.image = process.detach();
                        Err(())
                    }
                },
                Err(_) => {
                    claim.image = process.detach();
                    Err(())
                }
            },
            Err(_) => Err(()),
        };
        return match ready {
            Ok(task) => {
                mark_started(shared, index, claim.pid);
                let Envelope {
                    handler,
                    max_steps,
                    reply,
                    _charge,
                    _message_charge,
                    ..
                } = claim.message;
                Some(Running {
                    pid: claim.pid,
                    task,
                    handler,
                    max_steps,
                    reply,
                    _charge,
                    _message_charge,
                })
            }
            Err(()) => {
                rollback_failed_start(shared, index, claim);
                None
            }
        };
    }
    let result = (|| {
        let bound = pool.for_process(&claim.account)?;
        let process = claim.image.attach(&bound)?;
        let mut task = ScalarTask::new(process);
        task.enable_effects();
        match task.start_scheduled(
            &claim.message.handler,
            &claim.message.arguments[..claim.message.count],
            claim.message.max_steps,
            || (claim.cancellation.clone(), preemption.clone()),
            &bound,
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
            mark_started(shared, index, claim.pid);
            let Envelope {
                handler,
                max_steps,
                reply,
                _charge,
                _message_charge,
                ..
            } = claim.message;
            Some(Running {
                pid: claim.pid,
                task,
                handler,
                max_steps,
                reply,
                _charge,
                _message_charge,
            })
        }
        Err(_) => {
            rollback_failed_start(shared, index, claim);
            None
        }
    }
}

fn rollback_failed_start(shared: &Shared, index: usize, claim: Claim) {
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
    let wake = state.enqueue(claim.pid);
    drop(state);
    wake.fire(&shared.changed);
    drop(claim.account);
    drop(claim.cancellation);
    drop(retired);
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
        handler,
        max_steps: _,
        reply,
        _charge,
        _message_charge,
    } = active;
    let snapshot = task.snapshot();
    let process = task.into_process();
    finish_process(
        shared,
        index,
        pid,
        process,
        snapshot,
        handler,
        reply,
        _charge,
        _message_charge,
        result,
    );
}

#[allow(clippy::too_many_arguments)]
fn finish_process(
    shared: &Shared,
    index: usize,
    pid: SchedulerPid,
    process: ScalarProcess,
    snapshot: Option<ExecutionSnapshot>,
    handler: HandlerId,
    mut reply: Reply,
    _charge: Reservation,
    _message_charge: Option<Reservation>,
    result: Result<Option<RequestOutcome>, String>,
) {
    let committed_state =
        (!process.module().manifest().state.is_reference()).then_some(process.state);
    let image = process.detach();
    let byte_state = result
        .is_err()
        .then(|| image.byte_state_snapshot())
        .flatten();
    let mut state = shared.worker_lock(index, LockPhase::Finish);
    let closed = state.closed;
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("finishing actor");
    assert_eq!(actor.owner, Some(index));
    let failed = result.is_err() || actor.stopping || closed;
    let diagnostic = if result.is_err() && !actor.stopping && !closed {
        let name = &handler.schema().name;
        let mut handler_name = [0; 256];
        handler_name[..name.len()].copy_from_slice(name.as_bytes());
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
            handler: handler_name,
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
        Ok(Some(value)) if !failed => value,
        _ => RequestOutcome::Failed,
    };
    let mut external_reply = None;
    match std::mem::replace(&mut reply, Reply::None) {
        Reply::External(publisher) => external_reply = Some((publisher, outcome)),
        Reply::Language(key) => state.finish(key, outcome),
        Reply::None => {}
    }
    let retired = if failed {
        state.retire(shared.identity, pid)
    } else {
        None
    };
    let wake = state.enqueue(pid).combine(state.maintain(shared.identity));
    drop(state);
    if let Some((publisher, outcome)) = external_reply {
        match outcome {
            RequestOutcome::Reply(value) => publisher.complete(value),
            RequestOutcome::ReplyList(value) => publisher.complete_list(value),
            RequestOutcome::ReplyBytes(value) => publisher.complete_bytes(value),
            RequestOutcome::ReplyString(value) => publisher.complete_string(value),
            RequestOutcome::ReplyRecord(value) => publisher.complete_record(value),
            _ => drop(publisher),
        }
    }
    drop((handler, _charge, _message_charge));
    drop(retired);
    let _ = wake;
    shared.changed.notify_all();
}
