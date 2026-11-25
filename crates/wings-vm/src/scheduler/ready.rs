use super::*;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::sync::atomic::AtomicPtr;
use std::thread::Thread;

pub(super) const WAKE_GLOBAL_BIT: u32 = 1 << 31;

/// Cache-line isolated per-worker run queue (BEAM `ErtsRunQueue` + lock-free
/// single-slot fast handoff). 0 <-> 1 transitions bypass `lock` and the
/// heap-backed `VecDeque` completely via `fast_slot`.
#[repr(align(64))]
pub(super) struct WorkerRunQueue {
    fast_slot: AtomicPtr<SharedMailboxInner>,
    queue_len: AtomicUsize,
    pub(super) active: AtomicU32,
    lock: AtomicBool,
    queue: UnsafeCell<VecDeque<SharedMailbox>>,
}

// Safety: `fast_slot` owns an `Arc::into_raw` pointer exchanged atomically;
// `queue` is only accessed while `lock` is held with Acquire/Release ordering.
unsafe impl Send for WorkerRunQueue {}
unsafe impl Sync for WorkerRunQueue {}

impl Drop for WorkerRunQueue {
    fn drop(&mut self) {
        let raw = *self.fast_slot.get_mut();
        if !raw.is_null() {
            // Safety: `raw` was produced by `Arc::into_raw` in `push`/`remove_slot`.
            unsafe {
                drop(Arc::from_raw(raw));
            }
        }
    }
}

impl WorkerRunQueue {
    fn new(capacity: usize) -> Result<Self, String> {
        let mut queue = VecDeque::new();
        queue
            .try_reserve_exact(capacity)
            .map_err(|_| "scheduler worker run queue allocation failed")?;
        Ok(Self {
            fast_slot: AtomicPtr::new(std::ptr::null_mut()),
            queue_len: AtomicUsize::new(0),
            active: AtomicU32::new(0),
            lock: AtomicBool::new(false),
            queue: UnsafeCell::new(queue),
        })
    }

    #[inline]
    fn len(&self, ordering: Ordering) -> usize {
        usize::from(!self.fast_slot.load(ordering).is_null()) + self.queue_len.load(ordering)
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.fast_slot.load(Ordering::Acquire).is_null()
            && self.queue_len.load(Ordering::Acquire) == 0
    }

    #[inline]
    fn is_empty_seqcst(&self) -> bool {
        self.fast_slot.load(Ordering::SeqCst).is_null()
            && self.queue_len.load(Ordering::SeqCst) == 0
    }

    fn push(&self, mailbox: SharedMailbox, ready_mask: &AtomicU32, bit: u32) -> (bool, bool) {
        if self.queue_len.load(Ordering::Acquire) == 0 {
            let raw = Arc::into_raw(mailbox.inner) as *mut SharedMailboxInner;
            if self
                .fast_slot
                .compare_exchange(
                    std::ptr::null_mut(),
                    raw,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                if (ready_mask.load(Ordering::SeqCst) & bit) == 0 {
                    ready_mask.fetch_or(bit, Ordering::SeqCst);
                }
                let congested = self.active.load(Ordering::Relaxed) > 0
                    || self.queue_len.load(Ordering::Relaxed) > 0;
                return (true, congested);
            }
            let mailbox = SharedMailbox {
                // Safety: CAS failed so ownership of `raw` remains here.
                inner: unsafe { Arc::from_raw(raw) },
            };
            return self.push_overflow(mailbox, ready_mask, bit);
        }
        self.push_overflow(mailbox, ready_mask, bit)
    }

    fn push_overflow(
        &self,
        mailbox: SharedMailbox,
        ready_mask: &AtomicU32,
        bit: u32,
    ) -> (bool, bool) {
        locks::spin_lock(&self.lock, None);
        let queue = unsafe { &mut *self.queue.get() };
        queue.push_back(mailbox);
        let qlen = queue.len();
        self.queue_len.store(qlen, Ordering::SeqCst);
        if (ready_mask.load(Ordering::SeqCst) & bit) == 0 {
            ready_mask.fetch_or(bit, Ordering::SeqCst);
        }
        let total = qlen + usize::from(!self.fast_slot.load(Ordering::Relaxed).is_null());
        let congested = total > 1 || self.active.load(Ordering::Relaxed) > 0;
        self.lock.store(false, Ordering::Release);
        (total == 1, congested)
    }

    fn pop(&self, ready_mask: &AtomicU32, bit: u32) -> Option<SharedMailbox> {
        loop {
            if !self.fast_slot.load(Ordering::Acquire).is_null() {
                let raw = self.fast_slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
                if !raw.is_null() {
                    if self.queue_len.load(Ordering::Acquire) == 0 {
                        ready_mask.fetch_and(!bit, Ordering::SeqCst);
                        if !self.is_empty_seqcst() {
                            ready_mask.fetch_or(bit, Ordering::SeqCst);
                        }
                    }
                    return Some(SharedMailbox {
                        inner: unsafe { Arc::from_raw(raw) },
                    });
                }
            }
            if self.queue_len.load(Ordering::Acquire) > 0 {
                locks::spin_lock(&self.lock, None);
                let queue = unsafe { &mut *self.queue.get() };
                let item = queue.pop_front();
                let qlen = queue.len();
                self.queue_len.store(qlen, Ordering::Release);
                self.lock.store(false, Ordering::Release);
                if item.is_some() {
                    return item;
                }
            }
            if (ready_mask.load(Ordering::Acquire) & bit) != 0 {
                ready_mask.fetch_and(!bit, Ordering::SeqCst);
                if !self.is_empty_seqcst() {
                    ready_mask.fetch_or(bit, Ordering::SeqCst);
                    continue;
                }
            } else if !self.is_empty() {
                continue;
            }
            return None;
        }
    }

    fn try_steal(&self, ready_mask: &AtomicU32, bit: u32) -> Option<SharedMailbox> {
        let total = self.len(Ordering::Acquire);
        if total == 0 || (total == 1 && self.active.load(Ordering::Acquire) == 0) {
            return None;
        }
        if !self.fast_slot.load(Ordering::Acquire).is_null() {
            let raw = self.fast_slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
            if !raw.is_null() {
                if self.is_empty_seqcst() {
                    ready_mask.fetch_and(!bit, Ordering::SeqCst);
                    if !self.is_empty_seqcst() {
                        ready_mask.fetch_or(bit, Ordering::SeqCst);
                    }
                }
                return Some(SharedMailbox {
                    inner: unsafe { Arc::from_raw(raw) },
                });
            }
        }
        if self.queue_len.load(Ordering::Acquire) > 0
            && self
                .lock
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        {
            let queue = unsafe { &mut *self.queue.get() };
            let has_fast = !self.fast_slot.load(Ordering::Relaxed).is_null();
            let total = usize::from(has_fast) + queue.len();
            let item =
                if total > 1 || (!queue.is_empty() && self.active.load(Ordering::Relaxed) > 0) {
                    queue.pop_front()
                } else {
                    None
                };
            let qlen = queue.len();
            self.queue_len.store(qlen, Ordering::Release);
            self.lock.store(false, Ordering::Release);
            if item.is_some() {
                if qlen == 0 && self.is_empty_seqcst() {
                    ready_mask.fetch_and(!bit, Ordering::SeqCst);
                    if !self.is_empty_seqcst() {
                        ready_mask.fetch_or(bit, Ordering::SeqCst);
                    }
                }
                return item;
            }
        }
        None
    }

    fn load(&self) -> usize {
        self.len(Ordering::Relaxed) + self.active.load(Ordering::Relaxed) as usize
    }

    fn remove_slot(&self, slot: usize, ready_mask: &AtomicU32, bit: u32) -> Option<SharedMailbox> {
        if !self.fast_slot.load(Ordering::Acquire).is_null() {
            let raw = self.fast_slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
            if !raw.is_null() {
                let mailbox = SharedMailbox {
                    inner: unsafe { Arc::from_raw(raw) },
                };
                if mailbox.inner.pid.slot as usize == slot {
                    if self.is_empty_seqcst() {
                        ready_mask.fetch_and(!bit, Ordering::SeqCst);
                        if !self.is_empty_seqcst() {
                            ready_mask.fetch_or(bit, Ordering::SeqCst);
                        }
                    }
                    return Some(mailbox);
                }
                let raw = Arc::into_raw(mailbox.inner) as *mut SharedMailboxInner;
                if self
                    .fast_slot
                    .compare_exchange(
                        std::ptr::null_mut(),
                        raw,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    let mailbox = SharedMailbox {
                        inner: unsafe { Arc::from_raw(raw) },
                    };
                    locks::spin_lock(&self.lock, None);
                    let queue = unsafe { &mut *self.queue.get() };
                    queue.push_front(mailbox);
                    self.queue_len.store(queue.len(), Ordering::Release);
                    self.lock.store(false, Ordering::Release);
                }
            }
        }
        locks::spin_lock(&self.lock, None);
        let queue = unsafe { &mut *self.queue.get() };
        let removed = queue
            .iter()
            .position(|mailbox| mailbox.inner.pid.slot as usize == slot)
            .and_then(|pos| queue.remove(pos));
        let qlen = queue.len();
        self.queue_len.store(qlen, Ordering::Release);
        self.lock.store(false, Ordering::Release);
        if qlen == 0 && self.is_empty_seqcst() {
            ready_mask.fetch_and(!bit, Ordering::SeqCst);
            if !self.is_empty_seqcst() {
                ready_mask.fetch_or(bit, Ordering::SeqCst);
            }
        }
        removed
    }
}

pub(super) struct RunQueues {
    queues: Box<[WorkerRunQueue]>,
    pub(super) ready_mask: CachePadded<AtomicU32>,
    epoch: Instant,
    pub(super) next_timer_ns: AtomicU64,
    pub(super) closed: AtomicBool,
    pub(super) changed: WorkerWakeHub,
}

impl RunQueues {
    pub(super) fn new(workers: usize, capacity: usize) -> Result<Arc<Self>, String> {
        let mut queues = Vec::new();
        queues
            .try_reserve_exact(workers)
            .map_err(|_| "scheduler run queue vector allocation failed")?;
        for _ in 0..workers {
            queues.push(WorkerRunQueue::new(capacity)?);
        }
        Ok(Arc::new(Self {
            queues: queues.into_boxed_slice(),
            ready_mask: CachePadded(AtomicU32::new(0)),
            epoch: Instant::now(),
            next_timer_ns: AtomicU64::new(u64::MAX),
            closed: AtomicBool::new(false),
            changed: WorkerWakeHub::new(),
        }))
    }

    #[inline]
    pub(super) fn publish_next_timer(&self, deadline: Option<Instant>) {
        let next = match deadline {
            Some(deadline) => deadline
                .saturating_duration_since(self.epoch)
                .as_nanos()
                .min((u64::MAX - 1) as u128) as u64,
            None => u64::MAX,
        };
        self.next_timer_ns.store(next, Ordering::Release);
    }

    #[inline]
    pub(super) fn timer_due(&self) -> bool {
        let next = self.next_timer_ns.load(Ordering::Acquire);
        next != u64::MAX && self.elapsed_ns() >= next
    }

    #[inline]
    pub(super) fn next_timer_wait(&self, default: Duration) -> Option<Duration> {
        let next = self.next_timer_ns.load(Ordering::SeqCst);
        if next == u64::MAX {
            return Some(default);
        }
        let now = self.elapsed_ns();
        if now >= next {
            None
        } else {
            Some(Duration::from_nanos(next - now).min(default))
        }
    }

    #[inline]
    fn elapsed_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos().min((u64::MAX - 1) as u128) as u64
    }

    #[inline]
    pub(super) fn is_empty(&self, worker: usize) -> bool {
        self.queues[worker].is_empty()
    }

    #[inline]
    pub(super) fn is_empty_seqcst(&self, worker: usize) -> bool {
        self.queues[worker].is_empty_seqcst()
    }

    #[inline]
    pub(super) fn local_len(&self, worker: usize) -> usize {
        self.queues[worker].len(Ordering::Acquire)
    }

    #[inline]
    pub(super) fn inc_active(&self, worker: usize) {
        let atomic = &self.queues[worker].active;
        atomic.store(atomic.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }

    #[inline]
    pub(super) fn dec_active(&self, worker: usize) {
        let atomic = &self.queues[worker].active;
        atomic.store(
            atomic.load(Ordering::Relaxed).saturating_sub(1),
            Ordering::Relaxed,
        );
    }

    #[cfg(test)]
    pub(super) fn set_active(&self, worker: usize, value: u32) {
        self.queues[worker].active.store(value, Ordering::Relaxed);
    }

    #[inline]
    pub(super) fn is_solo_awake(&self, worker: usize) -> bool {
        let workers = self.queues.len();
        if workers <= 1 {
            return true;
        }
        let all = if workers >= 32 {
            u32::MAX
        } else {
            (1u32 << workers) - 1
        };
        let others = all & !(1u32 << worker);
        (self.changed.parked_mask() & others) == others
    }

    pub(super) fn select_worker(&self, slot_index: usize, last_worker: Option<usize>) -> usize {
        let workers = self.queues.len();
        if workers <= 1 {
            return 0;
        }
        let parked = self.changed.parked_mask();
        let all_workers_mask = if workers >= 32 {
            u32::MAX
        } else {
            (1u32 << workers) - 1
        };
        if let Some(preferred) = last_worker.filter(|&w| w < workers) {
            if (parked & (1u32 << preferred)) == 0
                && self.queues[preferred].active.load(Ordering::Relaxed) == 0
            {
                return preferred;
            }
            // BEAM-style scheduler compaction (+scl true): never wake a parked
            // worker when another awake worker is completely idle, so low-rate
            // traffic stays compacted on the active worker(s) without futex
            // ping-pong.
            for candidate in 0..workers {
                if (parked & (1u32 << candidate)) == 0
                    && self.queues[candidate].active.load(Ordering::Relaxed) == 0
                    && self.queues[candidate].len(Ordering::Relaxed) == 0
                {
                    return candidate;
                }
            }
            if (parked & all_workers_mask) == all_workers_mask
                && self.queues[0].active.load(Ordering::Relaxed) == 0
                && self.queues[0].len(Ordering::Relaxed) == 0
            {
                return 0;
            }
            if self.queues[preferred].active.load(Ordering::Relaxed) == 0 {
                return preferred;
            }
            for offset in 1..workers {
                let candidate = (preferred + offset) % workers;
                if self.queues[candidate].active.load(Ordering::Relaxed) == 0
                    && self.queues[candidate].len(Ordering::Relaxed) == 0
                {
                    return candidate;
                }
            }
            let mut best = preferred;
            let mut best_load = self.queues[preferred].load();
            for offset in 1..workers {
                let candidate = (preferred + offset) % workers;
                let load = self.queues[candidate].load();
                if load < best_load {
                    best = candidate;
                    best_load = load;
                }
            }
            return best;
        }
        // BEAM-style scheduler compaction (+scl true) on initial placement:
        // when all workers are completely idle (or when any worker is parked),
        // compact onto the lowest-numbered awake idle worker (or worker 0) so
        // sequential / low-concurrency actors stay on worker 0 and keep sibling
        // workers parked. When multiple workers are already active/queued and
        // all awake, preserve slot affinity.
        let all_idle = self
            .queues
            .iter()
            .all(|q| q.active.load(Ordering::Relaxed) == 0 && q.len(Ordering::Relaxed) == 0);
        if all_idle || (parked & all_workers_mask) != 0 {
            for candidate in 0..workers {
                if (parked & (1u32 << candidate)) == 0
                    && self.queues[candidate].active.load(Ordering::Relaxed) == 0
                    && self.queues[candidate].len(Ordering::Relaxed) == 0
                {
                    return candidate;
                }
            }
            if self.queues[0].active.load(Ordering::Relaxed) == 0
                && self.queues[0].len(Ordering::Relaxed) == 0
            {
                return 0;
            }
        }
        let preferred = slot_index % workers;
        if self.queues[preferred].active.load(Ordering::Relaxed) == 0 {
            return preferred;
        }
        for offset in 1..workers {
            let candidate = (preferred + offset) % workers;
            if self.queues[candidate].active.load(Ordering::Relaxed) == 0
                && self.queues[candidate].len(Ordering::Relaxed) == 0
            {
                return candidate;
            }
        }
        let mut best = preferred;
        let mut best_load = self.queues[preferred].load();
        for offset in 1..workers {
            let candidate = (preferred + offset) % workers;
            let load = self.queues[candidate].load();
            if load < best_load {
                best = candidate;
                best_load = load;
            }
        }
        best
    }

    pub(super) fn push_and_wake(&self, worker: usize, mailbox: SharedMailbox) {
        let bit = 1u32 << worker;
        let (was_empty, congested) = self.queues[worker].push(mailbox, &self.ready_mask, bit);
        if was_empty {
            self.changed.wake_bits(bit);
        }
        if congested {
            self.changed.wake_one_other(bit);
        }
    }

    pub(super) fn push_deferred(&self, worker: usize, mailbox: SharedMailbox) -> WakeSignal {
        let bit = 1u32 << worker;
        let (_, congested) = self.queues[worker].push(mailbox, &self.ready_mask, bit);
        if congested {
            WakeSignal::ALL
        } else {
            WakeSignal::worker(worker)
        }
    }

    #[inline]
    pub(super) fn has_stealable(&self, worker: usize) -> bool {
        let mut mask =
            self.ready_mask.load(Ordering::Acquire) & !(1u32 << worker) & !WAKE_GLOBAL_BIT;
        while mask != 0 {
            let other = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some(queue) = self.queues.get(other) {
                let len = queue.len(Ordering::Relaxed);
                if len > 1 || (len > 0 && queue.active.load(Ordering::Relaxed) > 0) {
                    return true;
                }
            }
        }
        false
    }

    /// Lock-free (wrt global `Mutex<State>`) enqueue for a fast-pushed mailbox
    /// whose producer transitioned `active_or_queued` from `false` to `true`.
    pub(super) fn enqueue_fast_pushed(&self, mailbox: SharedMailbox) {
        if self.closed.load(Ordering::Acquire) || mailbox.is_retired() || mailbox.is_stopping() {
            let exec = mailbox.exec_lock();
            if exec.owner.is_none() {
                mailbox.set_active_or_queued(false);
            }
            return;
        }
        // `try_push_fast` exclusively won the `active_or_queued: false -> true`
        // transition, so no other producer or worker owns or is enqueueing this
        // actor. Skip `exec_lock()` on the fast path and route using the atomic
        // `last_worker_hint`; `try_acquire_mailbox` still validates
        // `retired`/`stopping`/`closed` under `exec_lock` before execution.
        let last_worker = mailbox.last_worker_hint();
        let slot_index = mailbox.inner.pid.slot as usize;
        let worker = self.select_worker(slot_index, last_worker);
        self.push_and_wake(worker, mailbox);
    }

    /// Enqueue a mailbox from the control plane (`State::enqueue`), returning
    /// a `WakeSignal` to fire after `Mutex<State>` is released.
    pub(super) fn enqueue_mailbox(&self, mailbox: SharedMailbox) -> WakeSignal {
        let mut exec = mailbox.exec_lock();
        if self.closed.load(Ordering::Acquire)
            || exec.retired
            || mailbox.is_stopping()
            || exec.queued
            || exec.owner.is_some()
        {
            return WakeSignal::NONE;
        }
        if !mailbox.mark_enqueued_if_nonempty() {
            return WakeSignal::NONE;
        }
        exec.queued = true;
        let last_worker = exec.last_worker;
        let slot_index = mailbox.inner.pid.slot as usize;
        drop(exec);
        let worker = self.select_worker(slot_index, last_worker);
        self.push_deferred(worker, mailbox)
    }

    pub(super) fn finish_enqueue_mailbox(
        &self,
        mailbox: SharedMailbox,
        failed: bool,
    ) -> WakeSignal {
        let mut exec = mailbox.exec_lock();
        assert!(exec.owner.is_none());
        if failed || self.closed.load(Ordering::Acquire) || exec.retired || mailbox.is_stopping() {
            mailbox.set_active_or_queued(false);
            return WakeSignal::NONE;
        }
        if exec.queued {
            return WakeSignal::NONE;
        }
        if !mailbox.keep_active_if_nonempty() {
            return WakeSignal::NONE;
        }
        exec.queued = true;
        let last_worker = exec.last_worker;
        let slot_index = mailbox.inner.pid.slot as usize;
        drop(exec);
        let worker = self.select_worker(slot_index, last_worker);
        self.push_deferred(worker, mailbox)
    }

    fn try_acquire_mailbox(
        &self,
        worker: usize,
        mailbox: SharedMailbox,
    ) -> Option<run::AcquiredActor> {
        let mut exec = mailbox.exec_lock();
        exec.queued = false;
        if exec.retired
            || mailbox.is_stopping()
            || self.closed.load(Ordering::Acquire)
            || exec.owner.is_some()
        {
            if exec.owner.is_none() {
                mailbox.set_active_or_queued(false);
            }
            return None;
        }
        if mailbox.owner_is_empty() {
            mailbox.set_active_or_queued(false);
            if mailbox.owner_is_empty() {
                return None;
            }
            mailbox.set_active_or_queued(true);
        }
        exec.owner = Some(worker);
        if exec.last_worker != Some(worker) {
            if exec.last_worker.is_some() {
                exec.migrations += 1;
            }
            exec.last_worker = Some(worker);
            mailbox.set_last_worker_hint(worker);
        }
        let image = exec.image.take().expect("unowned actor image");
        drop(exec);
        self.inc_active(worker);
        Some(run::AcquiredActor { image, mailbox })
    }

    /// Worker-local claim without touching the global `Mutex<State>`.
    pub(super) fn claim_local(&self, worker: usize) -> Option<run::AcquiredActor> {
        let bit = 1u32 << worker;
        while let Some(mailbox) = self.queues[worker].pop(&self.ready_mask, bit) {
            if let Some(acquired) = self.try_acquire_mailbox(worker, mailbox) {
                return Some(acquired);
            }
        }
        None
    }

    /// BEAM-style work-stealing claim: pop from this worker's own run queue
    /// first, then non-blockingly steal from any other congested worker queue.
    pub(super) fn claim_or_steal(&self, worker: usize) -> Option<run::AcquiredActor> {
        if let Some(acquired) = self.claim_local(worker) {
            return Some(acquired);
        }
        let workers = self.queues.len();
        if workers <= 1 {
            return None;
        }
        let other_mask =
            self.ready_mask.load(Ordering::Acquire) & !(1u32 << worker) & !WAKE_GLOBAL_BIT;
        if other_mask == 0 {
            return None;
        }
        for offset in 1..workers {
            let victim = (worker + offset) % workers;
            let victim_bit = 1u32 << victim;
            if other_mask & victim_bit == 0 {
                continue;
            }
            while let Some(mailbox) = self.queues[victim].try_steal(&self.ready_mask, victim_bit) {
                if let Some(acquired) = self.try_acquire_mailbox(worker, mailbox) {
                    return Some(acquired);
                }
            }
        }
        None
    }

    #[cfg(test)]
    pub(super) fn pop_ready_slot(&self, worker: usize) -> Option<usize> {
        let bit = 1u32 << worker;
        while let Some(mailbox) = self.queues[worker].pop(&self.ready_mask, bit) {
            let mut exec = mailbox.exec_lock();
            exec.queued = false;
            if exec.retired {
                continue;
            }
            if exec.owner.is_none() {
                mailbox.set_active_or_queued(false);
            }
            let slot = mailbox.inner.pid.slot as usize;
            drop(exec);
            return Some(slot);
        }
        None
    }

    pub(super) fn remove_ready_slot(&self, slot: usize) {
        for (worker, queue) in self.queues.iter().enumerate() {
            if let Some(mailbox) = queue.remove_slot(slot, &self.ready_mask, 1u32 << worker) {
                let mut exec = mailbox.exec_lock();
                exec.queued = false;
                if exec.owner.is_none() {
                    mailbox.set_active_or_queued(false);
                }
                return;
            }
        }
    }
}

pub(super) struct WorkerWakeHub {
    parked_mask: CachePadded<AtomicU32>,
    threads: [OnceLock<Thread>; 16],
}

impl WorkerWakeHub {
    pub(super) fn new() -> Self {
        Self {
            parked_mask: CachePadded(AtomicU32::new(0)),
            threads: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    pub(super) fn register(&self, worker: usize) {
        if let Some(slot) = self.threads.get(worker) {
            let _ = slot.set(std::thread::current());
        }
    }

    #[inline]
    pub(super) fn parked_mask(&self) -> u32 {
        self.parked_mask.load(Ordering::Relaxed)
    }

    pub(super) fn mark_parked(&self, bit: u32) {
        self.parked_mask.fetch_or(bit, Ordering::SeqCst);
    }

    pub(super) fn clear_parked(&self, bit: u32) {
        self.parked_mask.fetch_and(!bit, Ordering::SeqCst);
    }

    pub(super) fn wake_bits(&self, bits: u32) {
        let mut mask = bits & self.parked_mask.load(Ordering::SeqCst);
        while mask != 0 {
            let worker = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some(thread) = self.threads.get(worker).and_then(OnceLock::get) {
                thread.unpark();
            }
        }
    }

    pub(super) fn wake_one_other(&self, exclude_bit: u32) {
        std::sync::atomic::fence(Ordering::SeqCst);
        let mask = self.parked_mask.load(Ordering::Relaxed) & !exclude_bit;
        if mask != 0 {
            let worker = mask.trailing_zeros() as usize;
            if let Some(thread) = self.threads.get(worker).and_then(OnceLock::get) {
                thread.unpark();
            }
        }
    }

    #[cfg(test)]
    pub(super) fn notify_all(&self) {
        self.wake_bits(u32::MAX);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct WakeSignal(u32);

impl WakeSignal {
    pub(super) const NONE: Self = Self(0);
    pub(super) const ALL: Self = Self(u32::MAX);

    pub(super) fn worker(index: usize) -> Self {
        Self(1u32 << index)
    }

    pub(super) fn combine(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(super) fn fire(self, hub: &WorkerWakeHub) {
        if self.0 != 0 {
            hub.wake_bits(self.0);
        }
    }
}

impl State {
    pub(super) fn global_signal_needed(&self) -> bool {
        self.closed || !self.requests.is_empty_fast() || self.pending_monitors.is_some()
    }

    pub(super) fn wake_all_parked(&self) -> WakeSignal {
        WakeSignal::ALL
    }

    pub(super) fn should_park(&self, worker: usize, progress: bool, attempted: bool) -> bool {
        !progress
            && (attempted
                || (self.run_queues.is_empty(worker) && !self.run_queues.has_stealable(worker)))
    }

    pub(super) fn next_park_timeout(&self, default: Duration) -> Duration {
        match self.timers.as_ref() {
            Some(head) => head
                .deadline
                .saturating_duration_since(Instant::now())
                .min(default),
            None => default,
        }
    }

    pub(super) fn reserve_slot(&mut self) -> Result<usize, String> {
        let index = self.free.ok_or("scheduler process table full")?;
        let slot = &mut self.slots[index];
        self.free = slot.free_next.take();
        assert!(!slot.creating && slot.actor.is_none() && slot.generation != 0);
        slot.creating = true;
        Ok(index)
    }

    pub(super) fn release_slot(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        assert!(slot.actor.is_none());
        slot.creating = false;
        if slot.generation != 0 {
            slot.free_next = self.free;
            self.free = Some(index);
        }
    }

    pub(super) fn enqueue(&mut self, pid: SchedulerPid) -> WakeSignal {
        let index = pid.slot as usize;
        let slot = &self.slots[index];
        if self.closed || slot.generation != pid.generation {
            return WakeSignal::NONE;
        }
        let Some(actor) = slot.actor.as_ref() else {
            return WakeSignal::NONE;
        };
        if actor.stopping {
            return WakeSignal::NONE;
        }
        self.run_queues.enqueue_mailbox((*actor.mailbox).clone())
    }

    pub(super) fn finish_enqueue(&mut self, pid: SchedulerPid, failed: bool) -> WakeSignal {
        let index = pid.slot as usize;
        let slot = &self.slots[index];
        if slot.generation != pid.generation {
            return WakeSignal::NONE;
        }
        let Some(actor) = slot.actor.as_ref() else {
            return WakeSignal::NONE;
        };
        self.run_queues.finish_enqueue_mailbox(
            (*actor.mailbox).clone(),
            failed || self.closed || actor.stopping,
        )
    }

    #[cfg(test)]
    pub(super) fn remove_ready(&mut self, index: usize) {
        self.run_queues.remove_ready_slot(index);
    }

    #[cfg(test)]
    pub(super) fn pop_ready(&mut self, worker: usize) -> Option<usize> {
        self.run_queues.pop_ready_slot(worker)
    }
}
