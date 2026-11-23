use super::locks::LockPhase;
use super::ready::WAKE_GLOBAL_BIT;
use super::*;
use crate::execution::{PreparedDirect, WorkerFibers};
use crate::timer::DeadlineTimer;
use crate::{ExecutionStatus, PreemptionToken, RequestOutcome, ScalarProcess, ScalarTask};
use std::collections::VecDeque;

#[cfg(target_os = "macos")]
const HOT_SPIN_WINDOW: Duration = Duration::from_micros(55);
#[cfg(not(target_os = "macos"))]
const HOT_SPIN_WINDOW: Duration = Duration::from_micros(120);
const SIBLING_BATCH_THRESHOLD: usize = 4;
const MAX_BACKLOG_BATCH: usize = 32;

pub(super) struct Running {
    pid: SchedulerPid,
    task: ScalarTask,
    handler: usize,
    max_steps: u64,
    reply: Reply,
    _charge: crate::resources::EnvelopeCharge,
}

struct Claim {
    pid: SchedulerPid,
    image: DetachedProcess,
    message: Envelope,
    mailbox: SharedMailbox,
    account: ResourceAccount,
    cancellation: CancellationToken,
    fused_direct: Option<PreparedDirect>,
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

fn complete_external(publisher: ReplyPublisher, outcome: RequestOutcome) {
    match outcome {
        RequestOutcome::Reply(value) => publisher.complete(value),
        RequestOutcome::ReplyList(value) => publisher.complete_list(value),
        RequestOutcome::ReplyBytes(value) => publisher.complete_bytes(value),
        RequestOutcome::ReplyString(value) => publisher.complete_string(value),
        RequestOutcome::ReplyRecord(value) => publisher.complete_record(value),
        _ => drop(publisher),
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
        let native_stack_limit = crate::native_stack::current_managed_limit()?;
        shared.changed.register(index);
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
        let fibers = WorkerFibers::new(&pool, &preemption, native_stack_limit);
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
    let yield_observed = !shared.observations.is_empty();
    loop {
        let had_local = !local.is_empty();
        let (claim, wake, should_exit) = {
            let mut state = shared.worker_lock(index, LockPhase::Claim);
            let wake = state.maintain(shared.identity);
            let out = if state.closed {
                (None, wake, local.is_empty())
            } else if let Some(slot) = state.pop_ready_for_claim(index) {
                let pid = state.pid(shared.identity, slot);
                let actor = state.slots[slot].actor.as_mut().expect("ready actor");
                assert!(!actor.stopping && actor.owner.is_none() && actor.mailbox.count() > 0);
                let can_fuse = actor.image.as_ref().is_some_and(|image| {
                    actor.mailbox.front_can_run_direct(
                        image,
                        &fibers,
                        shared.config.slice_steps,
                        yield_observed,
                    )
                });
                let fused_direct = if can_fuse {
                    let failed = actor.image.as_ref().expect("actor image").is_failed();
                    fibers.prepare_direct_account(failed, &actor.account).ok()
                } else {
                    None
                };
                actor.owner = Some(index);
                let is_starting = if fused_direct.is_some() {
                    actor.starting = false;
                    if actor.last_worker.is_some_and(|previous| previous != index) {
                        actor.migrations += 1;
                    }
                    actor.last_worker = Some(index);
                    false
                } else {
                    actor.starting = true;
                    true
                };
                let popped = actor.mailbox.pop_for_claim(is_starting);
                let claim = Claim {
                    pid,
                    image: actor.image.take().expect("unowned actor image"),
                    message: popped,
                    mailbox: (*actor.mailbox).clone(),
                    account: actor.account.clone(),
                    cancellation: actor.cancellation.clone(),
                    fused_direct,
                };
                state.worker_active[index] += 1;
                (Some(claim), wake, false)
            } else {
                (None, wake, false)
            };
            shared.publish_signal(&state);
            out
        };
        wake.fire(&shared.changed);
        if should_exit {
            break;
        }
        let attempted = claim.is_some();
        if claim
            .as_ref()
            .is_some_and(|claim| claim.fused_direct.is_some() || claim.message.fast_eligible)
        {
            let max_batch = if had_local { 1 } else { MAX_BACKLOG_BATCH };
            let progressed = run_fast_claim(
                &shared,
                index,
                claim.expect("checked fast claim"),
                &pool,
                &timer,
                &mut fibers,
                &mut local,
                watch,
                yield_observed,
                max_batch,
            );
            if had_local {
                if let Some(active) = local.pop_front() {
                    run_active_slice(
                        &shared,
                        index,
                        active,
                        &pool,
                        &timer,
                        &mut fibers,
                        &mut local,
                    );
                    hot_until = Instant::now().checked_add(HOT_SPIN_WINDOW);
                    continue;
                }
            }
            if progressed {
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
            if let Some(active) = next_active {
                run_active_slice(
                    &shared,
                    index,
                    active,
                    &pool,
                    &timer,
                    &mut fibers,
                    &mut local,
                );
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
        let bit = 1u32 << index;
        shared.changed.mark_parked(bit);
        let (should_break, sleep_duration) =
            if !attempted && (shared.ready_mask.load(Ordering::SeqCst) & watch != 0) {
                (false, None)
            } else {
                let state = shared.lock();
                if state.closed && local.is_empty() {
                    shared.publish_signal(&state);
                    (true, None)
                } else {
                    let sleep = state
                        .should_park(index, false, attempted)
                        .then(|| state.next_park_timeout(Duration::from_millis(1)));
                    shared.publish_signal(&state);
                    (false, sleep)
                }
            };
        if should_break {
            shared.changed.clear_parked(bit);
            break;
        }
        if let Some(duration) = sleep_duration.filter(|d| !d.is_zero()) {
            std::thread::park_timeout(duration);
        }
        shared.changed.clear_parked(bit);
    }
}

fn mark_started(shared: &Shared, index: usize, pid: SchedulerPid) {
    let mut state = shared.worker_lock(index, LockPhase::Start);
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("claimed actor");
    assert_eq!(actor.owner, Some(index));
    actor.starting = false;
    actor.mailbox.set_starting(false);
    if actor.last_worker.is_some_and(|previous| previous != index) {
        actor.migrations += 1;
    }
    actor.last_worker = Some(index);
}

#[allow(clippy::too_many_arguments)]
fn run_active_slice(
    shared: &Arc<Shared>,
    index: usize,
    mut active: Running,
    pool: &StackPool,
    timer: &DeadlineTimer,
    fibers: &mut WorkerFibers,
    local: &mut VecDeque<Running>,
) {
    match Instant::now()
        .checked_add(shared.config.quantum)
        .and_then(|deadline| timer.arm(deadline).ok())
    {
        Some(()) => {
            let started_at = shared.slice_observations.get(index).map(|_| Instant::now());
            let result = active
                .task
                .resume_with_fibers(shared.config.slice_steps, fibers);
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
                    let (status, value) = shared.effect(active.pid, effect, active.max_steps, pool);
                    active.task.acknowledge_effect(status, value);
                }
                let mut state = shared.worker_lock(index, LockPhase::Snapshot);
                if let Ok(actor) = state.actor_mut(shared.identity, active.pid) {
                    actor.execution = active.task.snapshot();
                }
                drop(state);
                local.push_back(active);
            } else {
                finish(shared, index, active, map_outcome(result));
            }
        }
        None => {
            finish(shared, index, active, Err("scheduler timer failure".into()));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_fast_claim(
    shared: &Arc<Shared>,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    timer: &DeadlineTimer,
    fibers: &mut WorkerFibers,
    local: &mut VecDeque<Running>,
    watch: u32,
    yield_observed: bool,
    max_batch: usize,
) -> bool {
    let mut process = match claim.image.attach_scheduled(pool, &claim.account) {
        Ok(process) => process,
        Err(_) => {
            if let Some(prepared) = claim.fused_direct.take() {
                prepared.release(fibers);
            }
            rollback_failed_start(shared, index, claim);
            return false;
        }
    };
    let pid = claim.pid;
    let mailbox = claim.mailbox;
    let account = claim.account;
    let cancellation = claim.cancellation;
    let mut already_started = claim.fused_direct.is_some();
    let mut cached_direct = claim.fused_direct;
    let mut current_message = Some(claim.message);
    for batch_step in 0..max_batch {
        let message = current_message.take().expect("active fast batch message");
        let run_direct = if batch_step == 0 && cached_direct.is_some() {
            true
        } else if fibers.can_run_direct(
            &process,
            message.handler,
            message.max_steps,
            shared.config.slice_steps,
            yield_observed,
        ) {
            if cached_direct.is_none() {
                match fibers.prepare_direct(&process, &account) {
                    Ok(prepared) => cached_direct = Some(prepared),
                    Err(_) => {
                        rollback_failed_start(
                            shared,
                            index,
                            Claim {
                                pid,
                                image: process.detach(),
                                message,
                                mailbox,
                                account,
                                cancellation,
                                fused_direct: None,
                            },
                        );
                        return batch_step > 0;
                    }
                }
            }
            true
        } else {
            if let Some(prepared) = cached_direct.take() {
                prepared.release(fibers);
            }
            false
        };

        let step = if run_direct {
            if !already_started {
                if batch_step == 0 {
                    mark_started(shared, index, pid);
                } else {
                    let mut state = shared.worker_lock(index, LockPhase::Start);
                    let actor = state
                        .actor_mut(shared.identity, pid)
                        .expect("batched actor");
                    assert_eq!(actor.owner, Some(index));
                    actor.starting = false;
                    actor.mailbox.set_starting(false);
                }
            }
            let Envelope {
                handler,
                arguments,
                max_steps,
                reply,
                _charge,
                ..
            } = message;
            let started_at = shared.slice_observations.get(index).map(|_| Instant::now());
            let (next_process, snapshot, result) =
                cached_direct.as_ref().expect("cached direct lease").run(
                    process,
                    handler,
                    arguments.as_slice(),
                    max_steps,
                    shared.config.slice_steps,
                    &cancellation,
                );
            let returned_at = started_at.map(|_| Instant::now());
            (
                Ok((next_process, snapshot, result)),
                handler,
                max_steps,
                reply,
                _charge,
                started_at,
                returned_at,
            )
        } else {
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
                            mailbox,
                            account,
                            cancellation,
                            fused_direct: None,
                        },
                    );
                    return batch_step > 0;
                }
            };
            if batch_step == 0 {
                mark_started(shared, index, pid);
            } else if !already_started {
                let mut state = shared.worker_lock(index, LockPhase::Start);
                let actor = state
                    .actor_mut(shared.identity, pid)
                    .expect("batched actor");
                assert_eq!(actor.owner, Some(index));
                actor.starting = false;
                actor.mailbox.set_starting(false);
            }
            let Envelope {
                handler,
                arguments,
                max_steps,
                reply,
                _charge,
                ..
            } = message;
            let armed = Instant::now()
                .checked_add(shared.config.quantum)
                .and_then(|deadline| timer.arm(deadline).ok());
            let Some(()) = armed else {
                drop(prepared);
                drop((mailbox, account, cancellation));
                finish_process(
                    shared,
                    index,
                    pid,
                    process,
                    None,
                    handler,
                    reply,
                    _charge,
                    Err("scheduler timer failure".into()),
                );
                return true;
            };
            let started_at = shared.slice_observations.get(index).map(|_| Instant::now());
            let outcome = prepared.run_first_slice(
                fibers,
                process,
                handler,
                arguments.as_slice(),
                max_steps,
                shared.config.slice_steps,
                &cancellation,
            );
            let returned_at = started_at.map(|_| Instant::now());
            timer.disarm();
            (
                outcome,
                handler,
                max_steps,
                reply,
                _charge,
                started_at,
                returned_at,
            )
        };

        let (step_result, handler, max_steps, mut reply, _charge, started_at, returned_at) = step;

        match step_result {
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
                if batch_step + 1 < max_batch && mapped.is_ok() {
                    let sibling_ready = (shared.ready_mask.load(Ordering::Acquire) & watch) != 0;
                    let can_batch_fast = !matches!(reply, Reply::Language(_))
                        && !shared.closed.load(Ordering::Acquire)
                        && (!sibling_ready || batch_step < SIBLING_BATCH_THRESHOLD);
                    let fast_threshold = if sibling_ready {
                        SIBLING_BATCH_THRESHOLD
                    } else {
                        1
                    };
                    if let Some(next_msg) = can_batch_fast
                        .then(|| mailbox.try_pop_batch_fast(fast_threshold))
                        .flatten()
                    {
                        let outcome = match mapped {
                            Ok(Some(value)) => value,
                            _ => RequestOutcome::Failed,
                        };
                        drop(_charge);
                        if let Reply::External(publisher) =
                            std::mem::replace(&mut reply, Reply::None)
                        {
                            complete_external(publisher, outcome);
                        }
                        already_started = true;
                        current_message = Some(next_msg);
                        continue;
                    }

                    if matches!(reply, Reply::Language(_)) {
                        let committed_state = (!process.module().manifest().state.is_reference())
                            .then_some(process.state);
                        let mut state = shared.worker_lock(index, LockPhase::Finish);
                        let can_continue = !state.closed && state.ready[index].head.is_none() && {
                            let actor = state
                                .actor(shared.identity, pid)
                                .expect("finishing batched actor");
                            !actor.stopping
                                && actor.mailbox.count() >= SIBLING_BATCH_THRESHOLD
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
                            let next_msg = actor.mailbox.pop_for_claim(true);
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
                            shared.publish_signal(&state);
                            drop(_charge);
                            drop(state);
                            if let Some((publisher, outcome)) = external_reply {
                                complete_external(publisher, outcome);
                            }
                            wake.fire(&shared.changed);
                            already_started = false;
                            current_message = Some(next_msg);
                            continue;
                        }
                    }
                }
                if let Some(prepared) = cached_direct.take() {
                    prepared.release(fibers);
                }
                drop((mailbox, account, cancellation));
                finish_process(
                    shared,
                    index,
                    pid,
                    process,
                    Some(snapshot),
                    handler,
                    reply,
                    _charge,
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
                });
                return true;
            }
        }
    }
    unreachable!("batch loop exits via finish_process or suspension")
}

fn start(
    shared: &Shared,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    preemption: &PreemptionToken,
    fibers: &mut WorkerFibers,
) -> Option<Running> {
    if let Some(prepared) = claim.fused_direct.take() {
        prepared.release(fibers);
    }
    if claim.message.fast_eligible {
        let prepared = match claim.image.attach_scheduled(pool, &claim.account) {
            Ok(process) => match fibers.prepare(&process, &claim.account) {
                Ok(prepared) => {
                    match prepared.into_ready_task(
                        fibers,
                        process,
                        claim.message.handler,
                        claim.message.arguments.as_slice(),
                        claim.message.max_steps,
                        &claim.cancellation,
                    ) {
                        Ok(task) => Ok(task),
                        Err((process, _)) => {
                            claim.image = process.detach();
                            Err(())
                        }
                    }
                }
                Err(_) => {
                    claim.image = process.detach();
                    Err(())
                }
            },
            Err(_) => Err(()),
        };
        return match prepared {
            Ok(task) => {
                mark_started(shared, index, claim.pid);
                let Envelope {
                    handler,
                    max_steps,
                    reply,
                    _charge,
                    ..
                } = claim.message;
                Some(Running {
                    pid: claim.pid,
                    task,
                    handler,
                    max_steps,
                    reply,
                    _charge,
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
            claim.message.handler,
            claim.message.arguments.as_slice(),
            claim.message.max_steps,
            || (claim.cancellation.clone(), preemption.clone()),
            &bound,
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
                ..
            } = claim.message;
            Some(Running {
                pid: claim.pid,
                task,
                handler,
                max_steps,
                reply,
                _charge,
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
    state.worker_active[index] = state.worker_active[index].saturating_sub(1);
    let closed = state.closed;
    let actor = state
        .actor_mut(shared.identity, claim.pid)
        .expect("failed claim actor");
    actor.owner = None;
    actor.starting = false;
    actor.mailbox.set_starting(false);
    actor.image = Some(claim.image);
    actor.mailbox.restore(claim.message);
    let mut closed_termination = if closed {
        actor.termination.take()
    } else {
        None
    };
    let failed = actor.stopping || closed;
    if failed {
        shared.fast_slots[claim.pid.slot as usize].clear(shared.fast_slot_contention());
    }
    let exit_reason = if closed {
        i64::from(wings_abi::STATUS_CANCELLED)
    } else {
        0
    };
    let retired = if failed {
        state.retire_with_reason(shared.identity, claim.pid, exit_reason)
    } else {
        None
    };
    if let Some(mut retired) = retired {
        retired.actor.mailbox.retire_clear();
        clear_timers(&mut retired.timers);
        clear_monitors(&mut retired.monitors);
        drop(retired.actor);
    }
    drop((claim.mailbox, claim.account, claim.cancellation));
    let wake = state
        .finish_enqueue(claim.pid, failed)
        .combine(state.maintain(shared.identity));
    shared.publish_signal(&state);
    drop(state);
    if let Some(mut publisher) = closed_termination.take() {
        publisher.set(SchedulerTermination::Closed);
    }
    wake.fire(&shared.changed);
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
    } = active;
    let snapshot = task.snapshot();
    let process = task.into_process();
    finish_process(
        shared, index, pid, process, snapshot, handler, reply, _charge, result,
    );
}

#[allow(clippy::too_many_arguments)]
fn finish_process(
    shared: &Shared,
    index: usize,
    pid: SchedulerPid,
    process: ScalarProcess,
    snapshot: Option<ExecutionSnapshot>,
    handler: usize,
    mut reply: Reply,
    _charge: crate::resources::EnvelopeCharge,
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
    state.worker_active[index] = state.worker_active[index].saturating_sub(1);
    let closed = state.closed;
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("finishing actor");
    assert_eq!(actor.owner, Some(index));
    let failed = result.is_err() || actor.stopping || closed;
    let exit_reason = if closed {
        i64::from(wings_abi::STATUS_CANCELLED)
    } else if actor.stopping {
        0
    } else if result.is_err() {
        snapshot
            .and_then(|execution| execution.fault)
            .map_or(i64::from(wings_abi::STATUS_INVALID_CONTROL), |fault| {
                fault.exit_code()
            })
    } else {
        0
    };
    let diagnostic = if result.is_err() && !actor.stopping && !closed {
        let name = &actor.module.manifest().handlers[handler].name;
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
            queued: actor.mailbox.count(),
            resources_before_cleanup: actor.account.used(),
            handler: handler_name,
            handler_len: name.len(),
        })
    } else {
        None
    };
    let mut deferred_charge = Some(_charge);
    if failed {
        drop(deferred_charge.take());
    }
    actor.owner = None;
    actor.image = Some(image);
    actor.execution = snapshot;
    actor.committed_state = committed_state;
    let mut termination = None;
    if failed {
        shared.fast_slots[pid.slot as usize].clear(shared.fast_slot_contention());
        termination = actor.termination.take().map(|publisher| {
            (
                publisher,
                if closed {
                    SchedulerTermination::Closed
                } else if let Some(diagnostic) = diagnostic {
                    SchedulerTermination::Failed(diagnostic)
                } else {
                    SchedulerTermination::Stopped
                },
            )
        });
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
        state.retire_with_reason(shared.identity, pid, exit_reason)
    } else {
        None
    };
    if let Some(mut retired) = retired {
        retired.actor.mailbox.retire_clear();
        clear_timers(&mut retired.timers);
        clear_monitors(&mut retired.monitors);
        drop(retired.actor);
    }
    let wake = state
        .finish_enqueue(pid, failed)
        .combine(state.maintain(shared.identity));
    shared.publish_signal(&state);
    drop(state);
    drop(deferred_charge);
    if let Some((publisher, outcome)) = external_reply {
        complete_external(publisher, outcome);
    }
    if let Some((mut publisher, reason)) = termination {
        publisher.set(reason);
    }
    wake.fire(&shared.changed);
}
