use super::locks::LockPhase;
use super::ready::{WAKE_GLOBAL_BIT, WakeSignal};
use super::*;
use crate::execution::{PreparedDirect, WorkerFibers};
use crate::timer::DeadlineTimer;
use crate::{ExecutionStatus, PreemptionToken, RequestOutcome, ScalarProcess, ScalarTask};
use std::collections::VecDeque;

const MIN_SPIN_WINDOW: Duration = Duration::from_micros(25);
const INITIAL_SPIN_WINDOW: Duration = Duration::from_micros(60);
const PEER_SPIN_WINDOW: Duration = Duration::from_micros(40);
const MAX_SOLO_SPIN_WINDOW: Duration = Duration::from_micros(1500);
const SOLO_SPIN_GROW_STEP: Duration = Duration::from_micros(250);
const SOLO_SPIN_DECAY_IDLE: Duration = Duration::from_micros(3000);
const IDLE_PARK_TIMEOUT: Duration = Duration::from_millis(50);
const SIBLING_BATCH_THRESHOLD: usize = 4;
const MAX_BACKLOG_BATCH: usize = 32;

pub(super) struct Running {
    pid: SchedulerPid,
    task: ScalarTask,
    mailbox: SharedMailbox,
    handler: usize,
    max_steps: u64,
    reply: Reply,
    snapshot_published: bool,
    _charge: crate::resources::EnvelopeCharge,
}

struct Claim {
    image: DetachedProcess,
    message: Envelope,
    mailbox: SharedMailbox,
    fused_direct: Option<PreparedDirect>,
    prechecked_direct: bool,
}

pub(super) struct AcquiredActor {
    pub(super) image: DetachedProcess,
    pub(super) mailbox: SharedMailbox,
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
        shared.run_queues.changed.register(index);
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
    // `hot` marks that work ran since the last park attempt; the spin deadline
    // is computed lazily when the loop actually goes idle so the per-message
    // path does not pay a clock read. `spin_window` adapts BEAM-style between
    // `MIN_SPIN_WINDOW` (idle) and `MAX_SPIN_WINDOW` (steady medium traffic).
    let mut hot = false;
    let mut spin_window = INITIAL_SPIN_WINDOW;
    // A `finish_process` fused continuation carries the already-claimed next
    // message for the same actor; it is dispatched before touching the state
    // lock on the next iteration.
    let mut pending_claim: Option<Claim> = None;
    // BEAM-style FIFO round-robin between `run_queues[index]` and suspended
    // tasks in `local`: after each round of `local` slices, snapshot the
    // waiting run-queue depth into `drain_fast` so every already-queued actor
    // gets a turn before `local` resumes, while bounding the drain so an
    // unbounded fast-message stream can never starve suspended tasks.
    let mut drain_fast: usize = 0;
    let mut local_round_remaining: usize = 0;
    let watch = (1u32 << index) | WAKE_GLOBAL_BIT;
    let yield_observed = !shared.observations.is_empty();
    loop {
        let had_local = !local.is_empty();
        if !had_local {
            drain_fast = 0;
            local_round_remaining = 0;
        } else if drain_fast == 0 && pending_claim.is_none() {
            let mask = shared.run_queues.ready_mask.load(Ordering::Acquire);
            let timer_due = shared.run_queues.timer_due();
            let closed = shared.run_queues.closed.load(Ordering::Acquire);
            if closed || (mask & WAKE_GLOBAL_BIT) != 0 || timer_due {
                let (wake, should_exit) = {
                    let mut state = shared.worker_lock(index, LockPhase::Claim);
                    let wake = state.maintain(shared.identity);
                    let should_exit = state.closed && local.is_empty();
                    shared.publish_signal(&mut state);
                    (wake, should_exit)
                };
                wake.fire(&shared.run_queues.changed);
                if should_exit {
                    break;
                }
            }
            if let Some(active) = local.pop_front() {
                let allow_continue = local.is_empty() && shared.run_queues.local_len(index) == 0;
                if let Some(fused) = run_active_slice(
                    &shared,
                    index,
                    active,
                    &pool,
                    &timer,
                    &mut fibers,
                    &mut local,
                    allow_continue,
                ) {
                    pending_claim = Some(fused);
                }
                local_round_remaining = local_round_remaining.saturating_sub(1);
                if local_round_remaining == 0 {
                    drain_fast = shared.run_queues.local_len(index);
                    if drain_fast == 0 && shared.run_queues.has_stealable(index) {
                        drain_fast = 1;
                    }
                    local_round_remaining = local.len();
                }
                hot = true;
                continue;
            }
        }
        let (claim, wake, should_exit) = if let Some(mut fused) = pending_claim.take() {
            // A fused continuation skips the lock entirely; the only state
            // maintenance needed is deferred until the next real acquisition.
            if fibers.can_run_direct_image_index(
                &fused.mailbox.inner.module,
                &fused.image,
                fused.message.handler,
                fused.message.max_steps,
                shared.config.slice_steps,
                yield_observed,
            ) {
                fused.fused_direct = fibers
                    .prepare_direct_account(fused.image.is_failed(), &fused.mailbox.inner.account)
                    .ok();
            }
            (Some(fused), WakeSignal::NONE, false)
        } else {
            let mask = shared.run_queues.ready_mask.load(Ordering::Acquire);
            let timer_due = shared.run_queues.timer_due();
            let can_steal = (mask & !watch) != 0 && shared.run_queues.has_stealable(index);
            if !had_local && hot && (mask & watch) == 0 && !can_steal && !timer_due {
                // Nothing is queued for this worker and no global maintenance bit
                // is set. Skip the empty `Claim` lock and go straight into the
                // bounded hot spin; if the spin expires without work, `hot` clears
                // and the park path checks timers and shutdown under the lock.
                (None, WakeSignal::NONE, false)
            } else {
                let closed = shared.run_queues.closed.load(Ordering::Acquire);
                let (acquired, wake, should_exit) =
                    if !closed && (mask & WAKE_GLOBAL_BIT) == 0 && !timer_due {
                        // BEAM-style per-worker run-queue claim with work stealing:
                        // pop from this worker's queue or steal from a congested peer
                        // without touching the global mutex.
                        (
                            shared.run_queues.claim_or_steal(index),
                            WakeSignal::NONE,
                            false,
                        )
                    } else {
                        let (wake, closed_now, should_exit) = {
                            let mut state = shared.worker_lock(index, LockPhase::Claim);
                            let wake = state.maintain(shared.identity);
                            let closed_now = state.closed;
                            let should_exit = closed_now && local.is_empty();
                            shared.publish_signal(&mut state);
                            (wake, closed_now, should_exit)
                        };
                        let acquired = if closed_now {
                            None
                        } else {
                            shared.run_queues.claim_or_steal(index)
                        };
                        (acquired, wake, should_exit)
                    };
                // Pop from the lock-free mailbox, check direct eligibility, and
                // reserve the direct lease *after* claiming ownership: once
                // `exec.owner = Some(index)` is published, this worker is the
                // exclusive consumer of `mailbox`.
                let claim = acquired.map(|acquired| {
                    let (message, fused_direct) = acquired.mailbox.pop_for_claim_direct(|front| {
                        if fibers.can_run_direct_image_index(
                            &acquired.mailbox.inner.module,
                            &acquired.image,
                            front.handler,
                            front.max_steps,
                            shared.config.slice_steps,
                            yield_observed,
                        ) {
                            fibers
                                .prepare_direct_account(
                                    acquired.image.is_failed(),
                                    &acquired.mailbox.inner.account,
                                )
                                .ok()
                        } else {
                            None
                        }
                    });
                    let prechecked_direct = fused_direct.is_some();
                    Claim {
                        image: acquired.image,
                        message,
                        mailbox: acquired.mailbox,
                        fused_direct,
                        prechecked_direct,
                    }
                });
                (claim, wake, should_exit)
            }
        };
        wake.fire(&shared.run_queues.changed);
        if should_exit {
            break;
        }
        let attempted = claim.is_some();
        if claim
            .as_ref()
            .is_some_and(|claim| claim.fused_direct.is_some() || claim.message.fast_eligible)
        {
            let max_batch = if had_local { 1 } else { MAX_BACKLOG_BATCH };
            let (progressed, continued) = run_fast_claim(
                &shared,
                index,
                claim.expect("checked fast claim"),
                &pool,
                &timer,
                &mut fibers,
                &mut local,
                yield_observed,
                max_batch,
            );
            pending_claim = continued;
            if had_local {
                drain_fast = drain_fast.saturating_sub(1);
                hot = true;
                continue;
            }
            if !local.is_empty() {
                drain_fast = shared.run_queues.local_len(index);
                if drain_fast == 0 && shared.run_queues.has_stealable(index) {
                    drain_fast = 1;
                }
                local_round_remaining = local.len();
                hot = true;
                continue;
            }
            if progressed {
                hot = true;
                continue;
            }
        } else {
            let next_active = match claim {
                Some(claim) => {
                    let started = start(&shared, index, claim, &pool, &preemption, &mut fibers);
                    if had_local {
                        if let Some(started) = started {
                            assert!(local.len() < shared.config.capacity);
                            local.push_back(started);
                        }
                        drain_fast = drain_fast.saturating_sub(1);
                        hot = true;
                        continue;
                    }
                    started
                }
                None => {
                    drain_fast = 0;
                    local.pop_front()
                }
            };
            if let Some(active) = next_active {
                let allow_continue = pending_claim.is_none()
                    && local.is_empty()
                    && shared.run_queues.local_len(index) == 0;
                if let Some(fused) = run_active_slice(
                    &shared,
                    index,
                    active,
                    &pool,
                    &timer,
                    &mut fibers,
                    &mut local,
                    allow_continue,
                ) {
                    pending_claim = Some(fused);
                }
                if !local.is_empty() {
                    drain_fast = shared.run_queues.local_len(index);
                    if drain_fast == 0 && shared.run_queues.has_stealable(index) {
                        drain_fast = 1;
                    }
                    local_round_remaining = local.len();
                }
                hot = true;
                continue;
            }
        }
        let bit = 1u32 << index;
        if !attempted && hot {
            let mut solo = shared.run_queues.is_solo_awake(index);
            let spin_start = Instant::now();
            let window = if solo {
                spin_window
            } else {
                PEER_SPIN_WINDOW.min(spin_window)
            };
            let Some(mut spin_deadline) = spin_start.checked_add(window) else {
                hot = false;
                continue;
            };
            let has_work = || {
                let m = shared.run_queues.ready_mask.load(Ordering::Acquire);
                (m & bit) != 0
                    || ((m & !watch) != 0 && shared.run_queues.has_stealable(index))
                    || shared.run_queues.timer_due()
            };
            while !has_work() {
                for _ in 0..32 {
                    if has_work() {
                        break;
                    }
                    std::hint::spin_loop();
                }
                if has_work() {
                    break;
                }
                let now = Instant::now();
                if solo && !shared.run_queues.is_solo_awake(index) {
                    solo = false;
                    if let Some(peer_deadline) = spin_start.checked_add(PEER_SPIN_WINDOW) {
                        spin_deadline = spin_deadline.min(peer_deadline);
                    }
                }
                if now >= spin_deadline {
                    hot = false;
                    break;
                }
            }
            if has_work() {
                continue;
            }
        }
        shared.run_queues.changed.mark_parked(bit);
        let (should_break, sleep_duration) = if !attempted
            && (shared.run_queues.ready_mask.load(Ordering::SeqCst) & bit != 0
                || shared.run_queues.has_stealable(index))
        {
            (false, None)
        } else if !shared.run_queues.closed.load(Ordering::SeqCst) {
            match shared.run_queues.next_timer_wait(IDLE_PARK_TIMEOUT) {
                Some(wait) => (false, Some(wait)),
                None => (false, None),
            }
        } else {
            let mut state = shared.lock();
            if state.closed && local.is_empty() {
                shared.publish_signal(&mut state);
                (true, None)
            } else {
                let sleep = state
                    .should_park(index, false, attempted)
                    .then(|| state.next_park_timeout(IDLE_PARK_TIMEOUT));
                shared.publish_signal(&mut state);
                (false, sleep)
            }
        };
        if should_break {
            shared.run_queues.changed.clear_parked(bit);
            break;
        }
        if let Some(mut duration) = sleep_duration.filter(|d| !d.is_zero()) {
            let park_start = Instant::now();
            loop {
                std::thread::park_timeout(duration);
                let woke_for_work = (shared.run_queues.ready_mask.load(Ordering::Acquire) & watch)
                    != 0
                    || shared.run_queues.has_stealable(index)
                    || shared.run_queues.timer_due()
                    || shared.run_queues.closed.load(Ordering::Acquire);
                if woke_for_work || attempted {
                    let parked = park_start.elapsed();
                    if woke_for_work && parked < MAX_SOLO_SPIN_WINDOW {
                        spin_window = spin_window
                            .saturating_add(SOLO_SPIN_GROW_STEP)
                            .min(MAX_SOLO_SPIN_WINDOW);
                    } else if parked >= SOLO_SPIN_DECAY_IDLE {
                        spin_window = (spin_window / 2).max(MIN_SPIN_WINDOW);
                    }
                    break;
                }
                // Fallback timeout with no work: decay spin window and stay
                // parked without clearing `parked_mask` so we never disturb a
                // solo-awake sibling worker.
                spin_window = (spin_window / 2).max(MIN_SPIN_WINDOW);
                match shared.run_queues.next_timer_wait(IDLE_PARK_TIMEOUT) {
                    Some(next) if !next.is_zero() => duration = next,
                    _ => break,
                }
            }
        }
        shared.run_queues.changed.clear_parked(bit);
    }
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
    allow_continue: bool,
) -> Option<Claim> {
    match timer.arm_after(shared.config.quantum) {
        Ok(()) => {
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
                let snapshot = active.task.snapshot();
                active.mailbox.set_live_execution(snapshot);
                if !active.snapshot_published {
                    active.mailbox.exec_lock().execution = snapshot;
                    active.snapshot_published = true;
                }
                local.push_back(active);
                None
            } else {
                finish(shared, index, active, map_outcome(result), allow_continue)
            }
        }
        Err(_) => finish(
            shared,
            index,
            active,
            Err("scheduler timer failure".into()),
            allow_continue,
        ),
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
    yield_observed: bool,
    max_batch: usize,
) -> (bool, Option<Claim>) {
    let mut process = match claim.image.attach_trusted_scheduled() {
        Ok(process) => process,
        Err(_) => {
            if let Some(prepared) = claim.fused_direct.take() {
                prepared.release(fibers);
            }
            rollback_failed_start(shared, index, claim);
            return (false, None);
        }
    };
    let mailbox = claim.mailbox;
    let pid = mailbox.inner.pid;
    let capability = mailbox.inner.capability;
    let mut inline: Option<EffectSendTarget> = None;
    let mut cached_direct = claim.fused_direct;
    let mut current_message = Some(claim.message);
    let mut prechecked_direct = claim.prechecked_direct;
    // Envelope charges of messages that complete inside this batch are merged
    // into one reservation instead of refunding per message; the merged charge
    // releases once the batch ends (or is handed to a suspended task).
    let mut batch_charge: Option<Reservation> = None;
    // `closed` only transitions once; hoisting the load out of the batch loop
    // lets a closed scheduler finish the current batch (bounded by
    // `max_batch`) before the worker unwinds through `finish_process`.
    let scheduler_open = !shared.run_queues.closed.load(Ordering::Acquire);
    for batch_step in 0..max_batch {
        let message = current_message.take().expect("active fast batch message");
        // The direct gate is re-checked for every message — including the
        // first, which may carry a `fused_direct` lease from the claim —
        // unless `try_pop_batch_fast_with` already verified `can_run_direct`
        // with an active `cached_direct` lease and skipped the `claimed` RMW.
        let run_direct = if prechecked_direct {
            true
        } else if fibers.can_run_direct(
            &process,
            message.handler,
            message.max_steps,
            shared.config.slice_steps,
            yield_observed,
        ) {
            if cached_direct.is_none() {
                match fibers.prepare_direct(&process, &mailbox.inner.account) {
                    Ok(prepared) => cached_direct = Some(prepared),
                    Err(_) => {
                        rollback_failed_start(
                            shared,
                            index,
                            Claim {
                                image: process.detach(),
                                message,
                                mailbox,
                                fused_direct: None,
                                prechecked_direct: false,
                            },
                        );
                        return (batch_step > 0, None);
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
            // Claim → handler start: the message stops counting against
            // admission capacity only now, matching the old `in_flight`
            // reservation released by `mark_started`. Pre-checked batch
            // messages bypassed the `claimed` increment entirely.
            if !prechecked_direct {
                mailbox.mark_started();
            }
            prechecked_direct = false;
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
                    &mailbox.inner.cancellation,
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
            let prepared = match fibers.prepare(&process, &mailbox.inner.account) {
                Ok(prepared) => prepared,
                Err(_) => {
                    rollback_failed_start(
                        shared,
                        index,
                        Claim {
                            image: process.detach(),
                            message,
                            mailbox,
                            fused_direct: None,
                            prechecked_direct: false,
                        },
                    );
                    return (batch_step > 0, None);
                }
            };
            let inline_target = inline
                .get_or_insert_with(|| EffectSendTarget::new(shared, &mailbox, pid, capability));
            let Envelope {
                handler,
                arguments,
                max_steps,
                reply,
                _charge,
                ..
            } = message;
            mailbox.mark_started();
            if timer.arm_after(shared.config.quantum).is_err() {
                drop(prepared);
                finish_process(
                    shared,
                    index,
                    mailbox,
                    process,
                    None,
                    handler,
                    reply,
                    _charge,
                    Err("scheduler timer failure".into()),
                    false,
                );
                return (true, None);
            }
            let started_at = shared.slice_observations.get(index).map(|_| Instant::now());
            let outcome = prepared.run_first_slice(
                fibers,
                process,
                handler,
                arguments.as_slice(),
                max_steps,
                shared.config.slice_steps,
                &mailbox.inner.cancellation,
                Some(inline_target.clone()),
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
                    // Sibling pressure only counts actors queued for *this*
                    // worker; the global bit merely signals pending request
                    // completions and must not suppress batching.
                    let sibling_ready = (shared.run_queues.ready_mask.load(Ordering::Acquire)
                        & (1u32 << index))
                        != 0;
                    let can_batch_fast = !matches!(reply, Reply::Language(_))
                        && scheduler_open
                        && (!sibling_ready || batch_step < SIBLING_BATCH_THRESHOLD);
                    let fast_threshold = if sibling_ready {
                        SIBLING_BATCH_THRESHOLD
                    } else {
                        1
                    };
                    if let Some((next_msg, direct)) = can_batch_fast
                        .then(|| {
                            let has_direct_lease = cached_direct.is_some();
                            mailbox.try_pop_batch_fast_with(fast_threshold, |front| {
                                has_direct_lease
                                    && fibers.can_run_direct(
                                        &process,
                                        front.handler,
                                        front.max_steps,
                                        shared.config.slice_steps,
                                        yield_observed,
                                    )
                            })
                        })
                        .flatten()
                    {
                        let outcome = match mapped {
                            Ok(Some(value)) => value,
                            _ => RequestOutcome::Failed,
                        };
                        match &mut batch_charge {
                            Some(merged) => merged.merge(_charge.into_reservation()),
                            slot => *slot = Some(_charge.into_reservation()),
                        }
                        if let Reply::External(publisher) =
                            std::mem::replace(&mut reply, Reply::None)
                        {
                            complete_external(publisher, outcome);
                        }
                        prechecked_direct = direct;
                        current_message = Some(next_msg);
                        continue;
                    }

                    // Fused finish + claim for a non-empty mailbox: when
                    // replying to an in-VM `ask` (Language reply) or when no
                    // sibling is ready, keep ownership and pop the next message
                    // in place.
                    let can_fuse = scheduler_open
                        && (!sibling_ready || matches!(reply, Reply::Language(_)))
                        && !mailbox.is_stopping()
                        && mailbox.count() > 0
                        && mailbox.front_is_fast();
                    if can_fuse {
                        let committed_state = (!process.module().manifest().state.is_reference())
                            .then_some(process.state);
                        if matches!(reply, Reply::Language(_)) {
                            let mut state = shared.worker_lock(index, LockPhase::Finish);
                            let can_continue = !state.closed
                                && shared.run_queues.is_empty(index)
                                && !mailbox.is_stopping()
                                && mailbox.count() > 0;
                            if can_continue {
                                let outcome = match mapped {
                                    Ok(Some(value)) => value,
                                    _ => RequestOutcome::Failed,
                                };
                                let next_msg = mailbox.pop_for_claim();
                                {
                                    let mut exec = mailbox.exec_lock();
                                    exec.execution = Some(snapshot);
                                    exec.committed_state = committed_state;
                                }
                                if let Reply::Language(key) =
                                    std::mem::replace(&mut reply, Reply::None)
                                {
                                    state.finish(key, outcome);
                                }
                                let wake = state.maintain(shared.identity);
                                shared.publish_signal(&mut state);
                                match &mut batch_charge {
                                    Some(merged) => merged.merge(_charge.into_reservation()),
                                    slot => *slot = Some(_charge.into_reservation()),
                                }
                                drop(state);
                                wake.fire(&shared.run_queues.changed);
                                current_message = Some(next_msg);
                                continue;
                            }
                            drop(state);
                        } else if !shared.run_queues.closed.load(Ordering::Acquire)
                            && shared.run_queues.is_empty(index)
                            && !mailbox.is_stopping()
                            && mailbox.count() > 0
                        {
                            let outcome = match mapped {
                                Ok(Some(value)) => value,
                                _ => RequestOutcome::Failed,
                            };
                            let next_msg = mailbox.pop_for_claim();
                            {
                                let mut exec = mailbox.exec_lock();
                                exec.execution = Some(snapshot);
                                exec.committed_state = committed_state;
                            }
                            match &mut batch_charge {
                                Some(merged) => merged.merge(_charge.into_reservation()),
                                slot => *slot = Some(_charge.into_reservation()),
                            }
                            if let Reply::External(publisher) =
                                std::mem::replace(&mut reply, Reply::None)
                            {
                                complete_external(publisher, outcome);
                            }
                            current_message = Some(next_msg);
                            continue;
                        }
                    }
                }
                if let Some(prepared) = cached_direct.take() {
                    prepared.release(fibers);
                }
                drop(inline);
                // Refund merged envelope charges from earlier messages in this
                // batch before `finish_process` publishes the final reply so a
                // woken caller observes zero residual message charge.
                drop(batch_charge);
                // Suspended work in `local` must interleave with the hot
                // actor, so only fuse when no older task is queued behind it.
                let continued = finish_process(
                    shared,
                    index,
                    mailbox,
                    process,
                    Some(snapshot),
                    handler,
                    reply,
                    _charge,
                    mapped,
                    local.is_empty(),
                );
                return (true, continued);
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
                mailbox.set_live_execution(snapshot);
                mailbox.exec_lock().execution = snapshot;
                local.push_back(Running {
                    pid,
                    task,
                    mailbox,
                    handler,
                    max_steps,
                    reply,
                    snapshot_published: true,
                    _charge,
                });
                return (true, None);
            }
        }
    }
    unreachable!("batch loop exits via finish_process or suspension")
}

fn start(
    shared: &Arc<Shared>,
    index: usize,
    mut claim: Claim,
    pool: &StackPool,
    preemption: &PreemptionToken,
    fibers: &mut WorkerFibers,
) -> Option<Running> {
    if let Some(prepared) = claim.fused_direct.take() {
        prepared.release(fibers);
    }
    let pid = claim.mailbox.inner.pid;
    let capability = claim.mailbox.inner.capability;
    if claim.message.fast_eligible {
        let prepared = match claim.image.attach_trusted_scheduled() {
            Ok(process) => match fibers.prepare(&process, &claim.mailbox.inner.account) {
                Ok(prepared) => {
                    match prepared.into_ready_task(
                        fibers,
                        process,
                        claim.message.handler,
                        claim.message.arguments.as_slice(),
                        claim.message.max_steps,
                        &claim.mailbox.inner.cancellation,
                        Some(EffectSendTarget::new(
                            shared,
                            &claim.mailbox,
                            pid,
                            capability,
                        )),
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
                claim.mailbox.mark_started();
                let Envelope {
                    handler,
                    max_steps,
                    reply,
                    _charge,
                    ..
                } = claim.message;
                Some(Running {
                    pid,
                    task,
                    mailbox: claim.mailbox,
                    handler,
                    max_steps,
                    reply,
                    snapshot_published: false,
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
        let bound = pool.for_process(&claim.mailbox.inner.account)?;
        let process = claim.image.attach(&bound)?;
        let mut task = ScalarTask::new(process);
        task.enable_effects();
        match task.start_scheduled(
            claim.message.handler,
            claim.message.arguments.as_slice(),
            claim.message.max_steps,
            || (claim.mailbox.inner.cancellation.clone(), preemption.clone()),
            &bound,
            claim
                .message
                .lists
                .as_deref()
                .unwrap_or(ListPayload::empty_ref()),
            claim
                .message
                .bytes
                .get_or_insert_with(Box::default)
                .as_mut(),
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
            claim.mailbox.mark_started();
            let Envelope {
                handler,
                max_steps,
                reply,
                _charge,
                ..
            } = claim.message;
            Some(Running {
                pid,
                task,
                mailbox: claim.mailbox,
                handler,
                max_steps,
                reply,
                snapshot_published: false,
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
    let pid = claim.mailbox.inner.pid;
    let mut state = shared.worker_lock(index, LockPhase::Start);
    shared.run_queues.dec_active(index);
    let closed = state.closed;
    let actor = state
        .actor_mut(shared.identity, pid)
        .expect("failed claim actor");
    {
        let mut exec = actor.mailbox.exec_lock();
        exec.owner = None;
        exec.image = Some(claim.image);
    }
    actor.mailbox.restore(claim.message);
    let mut closed_termination = if closed {
        actor.termination.take()
    } else {
        None
    };
    let failed = actor.stopping || closed;
    if failed {
        shared.fast_slots[pid.slot as usize].clear(shared.fast_slot_contention());
    }
    let exit_reason = if closed {
        i64::from(wings_abi::STATUS_CANCELLED)
    } else {
        0
    };
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
    drop(claim.mailbox);
    let wake = state
        .finish_enqueue(pid, failed)
        .combine(state.maintain(shared.identity));
    shared.publish_signal(&mut state);
    drop(state);
    if let Some(mut publisher) = closed_termination.take() {
        publisher.set(SchedulerTermination::Closed);
    }
    wake.fire(&shared.run_queues.changed);
}

fn finish(
    shared: &Arc<Shared>,
    index: usize,
    active: Running,
    result: Result<Option<RequestOutcome>, String>,
    allow_continue: bool,
) -> Option<Claim> {
    let Running {
        pid: _,
        task,
        mailbox,
        handler,
        max_steps: _,
        reply,
        snapshot_published: _,
        _charge,
    } = active;
    mailbox.set_live_execution(None);
    let snapshot = task.snapshot();
    let process = task.into_process();
    finish_process(
        shared,
        index,
        mailbox,
        process,
        snapshot,
        handler,
        reply,
        _charge,
        result,
        allow_continue,
    )
}

/// Commits one completed message and releases ownership. On the healthy
/// non-Language-reply path, ownership release and re-enqueue stay on the
/// per-actor `exec_lock` and per-worker `RunQueues` without touching the
/// global `Mutex<State>`.
#[allow(clippy::too_many_arguments)]
fn finish_process(
    shared: &Arc<Shared>,
    index: usize,
    mailbox: SharedMailbox,
    process: ScalarProcess,
    snapshot: Option<ExecutionSnapshot>,
    handler: usize,
    mut reply: Reply,
    _charge: crate::resources::EnvelopeCharge,
    result: Result<Option<RequestOutcome>, String>,
    allow_continue: bool,
) -> Option<Claim> {
    let pid = mailbox.inner.pid;
    let committed_state =
        (!process.module().manifest().state.is_reference()).then_some(process.state);
    let mut image = Some(process.detach());

    // Fast path: healthy completion without a Language reply and without
    // actor stop / scheduler close. Never touches global `Mutex<State>`.
    if result.is_ok()
        && !matches!(reply, Reply::Language(_))
        && !shared.run_queues.closed.load(Ordering::Acquire)
        && !mailbox.is_stopping()
    {
        let mut exec = mailbox.exec_lock();
        if !exec.retired
            && !mailbox.is_stopping()
            && !shared.run_queues.closed.load(Ordering::Acquire)
        {
            assert_eq!(exec.owner, Some(index));
            exec.execution = snapshot;
            exec.committed_state = committed_state;
            if allow_continue
                && shared.run_queues.is_empty(index)
                && !mailbox.owner_is_empty()
                && mailbox.front_is_fast()
            {
                let next_msg = mailbox.pop_for_claim();
                drop(exec);
                drop(_charge);
                if let Reply::External(publisher) = std::mem::replace(&mut reply, Reply::None) {
                    let outcome = match result {
                        Ok(Some(value)) => value,
                        _ => RequestOutcome::Failed,
                    };
                    complete_external(publisher, outcome);
                }
                return Some(Claim {
                    image: image.take().expect("continued claim image"),
                    message: next_msg,
                    mailbox,
                    fused_direct: None,
                    prechecked_direct: false,
                });
            }
            exec.owner = None;
            exec.image = image.take();
            let reenqueued_worker = if !exec.queued && mailbox.release_active_if_empty_owner() {
                exec.queued = true;
                let last_worker = exec.last_worker;
                let slot_index = pid.slot as usize;
                Some(shared.run_queues.select_worker(slot_index, last_worker))
            } else {
                None
            };
            drop(exec);
            shared.run_queues.dec_active(index);
            drop(_charge);
            if let Some(worker) = reenqueued_worker {
                shared.run_queues.push_and_wake(worker, mailbox);
                if let Reply::External(publisher) = std::mem::replace(&mut reply, Reply::None) {
                    let outcome = match result {
                        Ok(Some(value)) => value,
                        _ => RequestOutcome::Failed,
                    };
                    complete_external(publisher, outcome);
                }
            } else {
                if let Reply::External(publisher) = std::mem::replace(&mut reply, Reply::None) {
                    let outcome = match result {
                        Ok(Some(value)) => value,
                        _ => RequestOutcome::Failed,
                    };
                    complete_external(publisher, outcome);
                }
                drop(mailbox);
            }
            return None;
        }
    }

    let byte_state = result
        .is_err()
        .then(|| image.as_ref().and_then(|image| image.byte_state_snapshot()))
        .flatten();
    let mut state = shared.worker_lock(index, LockPhase::Finish);
    let closed = state.closed;
    let failed = result.is_err() || closed || {
        state
            .actor(shared.identity, pid)
            .expect("finishing actor")
            .stopping
    };
    // Fused continuation: keep `exec.owner` and pop the next fast message in
    // the same critical section instead of re-linking the ready queue.
    let continuation = if !failed && allow_continue && shared.run_queues.is_empty(index) {
        let exec = mailbox.exec_lock();
        debug_assert_eq!(exec.owner, Some(index));
        if !mailbox.is_stopping() && mailbox.count() > 0 && mailbox.front_is_fast() {
            let next_msg = mailbox.pop_for_claim();
            drop(exec);
            Some(next_msg)
        } else {
            None
        }
    } else {
        None
    };
    let exit_reason;
    let mut termination = None;
    let diagnostic;
    let release;
    {
        let actor = state
            .actor_mut(shared.identity, pid)
            .expect("finishing actor");
        let mut exec = actor.mailbox.exec_lock();
        assert_eq!(exec.owner, Some(index));
        exit_reason = if closed {
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
        diagnostic = if result.is_err() && !actor.stopping && !closed {
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
        release = continuation.is_none();
        exec.execution = snapshot;
        exec.committed_state = committed_state;
        if release {
            exec.owner = None;
            exec.image = image.take();
        }
        drop(exec);
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
    }
    if release {
        shared.run_queues.dec_active(index);
    }
    let mut deferred_charge = Some(_charge);
    if failed {
        drop(deferred_charge.take());
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
    let wake = if continuation.is_some() {
        state.maintain(shared.identity)
    } else {
        state
            .finish_enqueue(pid, failed)
            .combine(state.maintain(shared.identity))
    };
    shared.publish_signal(&mut state);
    drop(state);
    drop(deferred_charge);
    let continued = continuation.map(|message| Claim {
        image: image.take().expect("continued claim image"),
        message,
        mailbox: mailbox.clone(),
        fused_direct: None,
        prechecked_direct: false,
    });
    drop(mailbox);
    if let Some((publisher, outcome)) = external_reply {
        complete_external(publisher, outcome);
    }
    if let Some((mut publisher, reason)) = termination {
        publisher.set(reason);
    }
    wake.fire(&shared.run_queues.changed);
    continued
}
