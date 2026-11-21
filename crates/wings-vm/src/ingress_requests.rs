use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use wings_abi::ValueType;

use crate::{RequestOutcome, Reservation, ResourceAccount, Resources};

const MAX_BITMAP_WORDS: usize = 4;
const MAX_BITMAP_SLOTS: usize = MAX_BITMAP_WORDS * 64;

pub struct IngressRequest {
    table: Arc<ReplyTable>,
    reply_type: ValueType,
    slot: usize,
    generation: u64,
    active: bool,
    deadline: Instant,
}

pub(crate) struct ReplyPublisher {
    table: Arc<ReplyTable>,
    slot: usize,
    generation: u64,
}

pub(crate) struct ReplyTable {
    slots: Box<[ReplySlot]>,
    ready: Box<[AtomicBool]>,
    free: [AtomicU64; MAX_BITMAP_WORDS],
    waiters: AtomicUsize,
    wait_lock: Mutex<()>,
    changed: Condvar,
    account: ResourceAccount,
    _charge: Reservation,
}

struct ReplySlot {
    locked: AtomicBool,
    state: UnsafeCell<SlotState>,
}

// Safety: `SlotState` is only accessed while `locked` is held with Acquire/Release ordering,
// and `SlotState` (`u64` + `Option<Pending>`) is `Send`.
unsafe impl Send for ReplySlot {}
unsafe impl Sync for ReplySlot {}

struct SlotState {
    generation: u64,
    pending: Option<Pending>,
}

struct SlotGuard<'slot> {
    slot: &'slot ReplySlot,
}

impl Deref for SlotGuard<'_> {
    type Target = SlotState;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.slot.state.get() }
    }
}

impl DerefMut for SlotGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.slot.state.get() }
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.slot.locked.store(false, Ordering::Release);
    }
}

impl ReplySlot {
    fn lock(&self) -> SlotGuard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        SlotGuard { slot: self }
    }
}

struct Pending {
    deadline: Instant,
    outcome: Option<RequestOutcome>,
    caller: bool,
    publisher: bool,
    charge: Reservation,
}

impl ReplyTable {
    pub(crate) fn new(account: &ResourceAccount, capacity: usize) -> Result<Arc<Self>, String> {
        if capacity > MAX_BITMAP_SLOTS {
            return Err("ingress result capacity exceeds 256 slots".into());
        }
        let charge = account.reserve(Resources {
            bytes: (std::mem::size_of::<Self>()
                + capacity * (std::mem::size_of::<ReplySlot>() + std::mem::size_of::<AtomicBool>())
                + 128usize.max(crate::native_sync::shared_bytes(1, 1))) as u64,
            ..Resources::default()
        })?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| "ingress result storage allocation failed")?;
        slots.resize_with(capacity, || ReplySlot {
            locked: AtomicBool::new(false),
            state: UnsafeCell::new(SlotState {
                generation: 1,
                pending: None,
            }),
        });
        let mut ready = Vec::new();
        ready
            .try_reserve_exact(capacity)
            .map_err(|_| "ingress result readiness allocation failed")?;
        ready.resize_with(capacity, || AtomicBool::new(false));
        let mut mask = [0u64; MAX_BITMAP_WORDS];
        for index in 0..capacity {
            mask[index / 64] |= 1u64 << (index % 64);
        }
        Ok(Arc::new(Self {
            slots: slots.into_boxed_slice(),
            ready: ready.into_boxed_slice(),
            free: mask.map(AtomicU64::new),
            waiters: AtomicUsize::new(0),
            wait_lock: crate::native_sync::mutex(()),
            changed: crate::native_sync::condvar(),
            account: account.clone(),
            _charge: charge,
        }))
    }

    fn claim_free_slot(&self) -> Option<usize> {
        for (word_index, word) in self.free.iter().enumerate() {
            let mut current = word.load(Ordering::Acquire);
            while current != 0 {
                let bit = current.trailing_zeros() as usize;
                let mask = 1u64 << bit;
                match word.compare_exchange_weak(
                    current,
                    current & !mask,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(word_index * 64 + bit),
                    Err(actual) => current = actual,
                }
            }
        }
        None
    }

    fn release_free_slot(&self, index: usize) {
        self.free[index / 64].fetch_or(1u64 << (index % 64), Ordering::Release);
    }

    fn notify_waiters(&self) {
        if self.waiters.load(Ordering::SeqCst) != 0 {
            let _guard = self
                .wait_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.changed.notify_all();
        }
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        deadline: Instant,
        account: &ResourceAccount,
        reply_type: ValueType,
    ) -> Result<(IngressRequest, ReplyPublisher), String> {
        if !(self.account.same(account) || account.is_child_of(&self.account))
            || !reply_type.is_supported_reply()
        {
            return Err(
                "result table requires its owner or direct child and a supported reply".into(),
            );
        }
        let charge = account.reserve(Resources {
            requests: 1,
            ..Resources::default()
        })?;
        let mut charge = Some(charge);
        while let Some(index) = self.claim_free_slot() {
            let mut slot = self.slots[index].lock();
            if slot.generation == 0 {
                drop(slot);
                continue;
            }
            let generation = slot.generation;
            self.ready[index].store(false, Ordering::Release);
            slot.pending = Some(Pending {
                deadline,
                outcome: None,
                caller: true,
                publisher: true,
                charge: charge.take().expect("request admission charge"),
            });
            drop(slot);
            return Ok((
                IngressRequest {
                    table: self.clone(),
                    reply_type,
                    slot: index,
                    generation,
                    active: true,
                    deadline,
                },
                ReplyPublisher {
                    table: self.clone(),
                    slot: index,
                    generation,
                },
            ));
        }
        Err("ingress_requests_full".into())
    }

    fn finish(&self, index: usize, generation: u64, outcome: RequestOutcome) {
        let mut slot = self.slots[index].lock();
        if slot.generation != generation {
            return;
        }
        let Some(pending) = &mut slot.pending else {
            return;
        };
        if !pending.publisher {
            return;
        }
        if pending.outcome.is_none() {
            pending.outcome = Some(if Instant::now() >= pending.deadline {
                RequestOutcome::TimedOut
            } else {
                outcome
            });
        }
        pending.publisher = false;
        self.ready[index].store(true, Ordering::SeqCst);
        let (retired, reusable) = retire(&mut slot);
        drop(slot);
        if reusable {
            self.release_free_slot(index);
        }
        drop(retired);
        self.notify_waiters();
    }
}

fn retire(slot: &mut SlotState) -> (Option<Reservation>, bool) {
    if slot
        .pending
        .as_ref()
        .is_some_and(|pending| !pending.caller && !pending.publisher)
    {
        let charge = slot.pending.take().map(|pending| pending.charge);
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        (charge, slot.generation != 0)
    } else {
        (None, false)
    }
}

impl IngressRequest {
    pub fn reply_type(&self) -> ValueType {
        self.reply_type
    }

    pub fn try_take(&mut self) -> Result<Option<RequestOutcome>, String> {
        if !self.active {
            return Err("ingress request result has already been consumed".into());
        }
        if !self.table.ready[self.slot].load(Ordering::Acquire) && Instant::now() < self.deadline {
            return Ok(None);
        }
        let mut slot = self.table.slots[self.slot].lock();
        let pending = slot.pending.as_mut().expect("live ingress request");
        if pending.outcome.is_none() && Instant::now() >= pending.deadline {
            pending.outcome = Some(RequestOutcome::TimedOut);
        }
        let outcome = if !pending.publisher {
            pending.outcome.take()
        } else {
            pending.outcome.clone()
        };
        if outcome.is_some() {
            pending.caller = false;
            self.active = false;
        }
        let (retired, reusable) = retire(&mut slot);
        drop(slot);
        if reusable {
            self.table.release_free_slot(self.slot);
        }
        drop(retired);
        Ok(outcome)
    }

    pub fn wait(&mut self, max_wait: Duration) -> Result<Option<RequestOutcome>, String> {
        let end = Instant::now()
            .checked_add(max_wait)
            .ok_or("ingress result wait deadline overflow")?;
        if !self.active {
            return Err("ingress request result has already been consumed".into());
        }
        if self.table.ready[self.slot].load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return self.try_take();
        }
        struct WaiterCount<'table>(&'table AtomicUsize);
        impl Drop for WaiterCount<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let mut guard = self
            .table
            .wait_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.table.waiters.fetch_add(1, Ordering::SeqCst);
        let _waiter = WaiterCount(&self.table.waiters);
        loop {
            let ready = self.table.ready[self.slot].load(Ordering::SeqCst);
            let now = Instant::now();
            if ready || now >= self.deadline {
                drop(_waiter);
                drop(guard);
                return self.try_take();
            }
            if now >= end {
                return Ok(None);
            }
            let duration = end.min(self.deadline).saturating_duration_since(now);
            (guard, _) = self
                .table
                .changed
                .wait_timeout(guard, duration)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    pub fn cancel(&self) -> Result<(), String> {
        if !self.active {
            return Err("ingress request result has already been consumed".into());
        }
        let mut slot = self.table.slots[self.slot].lock();
        let pending = slot.pending.as_mut().expect("live ingress request");
        if pending.outcome.is_none() {
            pending.outcome = Some(if Instant::now() >= pending.deadline {
                RequestOutcome::TimedOut
            } else {
                RequestOutcome::Cancelled
            });
        }
        self.table.ready[self.slot].store(true, Ordering::SeqCst);
        drop(slot);
        self.table.notify_waiters();
        Ok(())
    }
}

impl Drop for IngressRequest {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut slot = self.table.slots[self.slot].lock();
        if slot.generation != self.generation {
            return;
        }
        if let Some(pending) = &mut slot.pending {
            pending.caller = false;
            if pending.outcome.is_none() {
                pending.outcome = Some(RequestOutcome::Cancelled);
            }
        }
        let (retired, reusable) = retire(&mut slot);
        drop(slot);
        if reusable {
            self.table.release_free_slot(self.slot);
        }
        drop(retired);
    }
}

impl ReplyPublisher {
    pub(crate) fn complete_string(self, value: crate::StringReply) {
        self.table.finish(
            self.slot,
            self.generation,
            RequestOutcome::ReplyString(value),
        );
    }
    pub(crate) fn complete_list(self, value: crate::IntListReply) {
        self.table
            .finish(self.slot, self.generation, RequestOutcome::ReplyList(value));
    }

    pub(crate) fn complete_bytes(self, value: crate::BytesReply) {
        self.table.finish(
            self.slot,
            self.generation,
            RequestOutcome::ReplyBytes(value),
        );
    }
    pub(crate) fn complete(self, value: i64) {
        self.table
            .finish(self.slot, self.generation, RequestOutcome::Reply(value));
    }

    pub(crate) fn complete_record(self, value: crate::RecordReply) {
        self.table.finish(
            self.slot,
            self.generation,
            RequestOutcome::ReplyRecord(value),
        );
    }
}

impl Drop for ReplyPublisher {
    fn drop(&mut self) {
        self.table
            .finish(self.slot, self.generation, RequestOutcome::Failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unready_result_poll_does_not_lock_slots_and_reuse_clears_readiness() {
        let account = ResourceAccount::vm(Resources {
            bytes: 8192,
            requests: 1,
            ..Resources::default()
        });
        let table = ReplyTable::new(&account, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        for cancel in [false, true] {
            let (mut request, publisher) = table.start(deadline, &account, ValueType::Int).unwrap();
            assert!(!table.ready[0].load(Ordering::Acquire));
            {
                let slot = table.slots[0].lock();
                assert!(slot.pending.as_ref().unwrap().outcome.is_none());
                assert_eq!(request.try_take().unwrap(), None);
            }
            if cancel {
                request.cancel().unwrap();
                assert!(table.ready[0].load(Ordering::Acquire));
                assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Cancelled));
                assert_eq!(account.used().requests, 1);
                drop(publisher);
            } else {
                publisher.complete(17);
                assert!(table.ready[0].load(Ordering::Acquire));
                assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Reply(17)));
            }
            assert!(request.try_take().is_err());
            assert_eq!(account.used().requests, 0);
        }
        drop(table);
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn unready_result_still_observes_deadline_and_retains_publisher_capacity() {
        let account = ResourceAccount::vm(Resources {
            bytes: 8192,
            requests: 1,
            ..Resources::default()
        });
        let table = ReplyTable::new(&account, 1).unwrap();
        let (mut request, publisher) = table
            .start(Instant::now(), &account, ValueType::BytesRef)
            .unwrap();
        assert!(!table.ready[0].load(Ordering::Acquire));
        assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::TimedOut));
        assert_eq!(account.used().requests, 1);
        assert!(
            table
                .start(Instant::now(), &account, ValueType::Int)
                .is_err()
        );
        drop(publisher);
        assert_eq!(account.used().requests, 0);
        drop((request, table));
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn readiness_storage_is_admitted_before_allocation_at_each_tier() {
        let required = (std::mem::size_of::<ReplyTable>()
            + 64 * (std::mem::size_of::<ReplySlot>() + std::mem::size_of::<AtomicBool>())
            + 128) as u64;
        for restricted in 0..3 {
            let limits = |level| Resources {
                bytes: required - u64::from(level == restricted),
                requests: 1,
                ..Resources::default()
            };
            let vm = ResourceAccount::vm(limits(0));
            let tenant = vm.tenant(limits(1)).unwrap();
            let process = tenant.process(limits(2)).unwrap();
            assert!(ReplyTable::new(&process, 64).is_err());
            assert_eq!(vm.used(), Resources::default());
        }
        let limits = Resources {
            bytes: required,
            requests: 1,
            ..Resources::default()
        };
        let account = ResourceAccount::vm(limits);
        let table = ReplyTable::new(&account, 64).unwrap();
        assert_eq!(account.used().bytes, required);
        drop(table);
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn readiness_is_per_slot_and_concurrent_publication_is_consumed_once() {
        let account = ResourceAccount::vm(Resources {
            bytes: 1 << 20,
            requests: 64,
            ..Resources::default()
        });
        let table = ReplyTable::new(&account, 64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut requests = Vec::new();
        let mut publishers = Vec::new();
        for index in 0..64 {
            let (request, publisher) = table.start(deadline, &account, ValueType::Int).unwrap();
            assert_eq!(request.slot, index);
            requests.push(request);
            publishers.push(publisher);
        }
        let gate = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let gate = &gate;
            scope.spawn(move || {
                gate.wait();
                for (index, publisher) in publishers.into_iter().enumerate().rev() {
                    publisher.complete(index as i64);
                    std::thread::yield_now();
                }
            });
            let guards: Vec<_> = table.slots.iter().map(ReplySlot::lock).collect();
            gate.wait();
            for request in &mut requests {
                assert_eq!(request.try_take().unwrap(), None);
            }
            drop(guards);
            for (index, request) in requests.iter_mut().enumerate() {
                loop {
                    assert!(Instant::now() < deadline);
                    if let Some(outcome) = request.try_take().unwrap() {
                        assert_eq!(outcome, RequestOutcome::Reply(index as i64));
                        assert!(request.try_take().is_err());
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        });
        assert_eq!(account.used().requests, 0);
        drop((requests, table));
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn positive_deadline_wait_and_publication_respect_operation_bounds_and_reuse() {
        for complete in [false, true] {
            for late in [false, true] {
                let limits = Resources {
                    bytes: 8192,
                    requests: 1,
                    ..Resources::default()
                };
                let account = ResourceAccount::vm(limits);
                let table = ReplyTable::new(&account, 1).unwrap();
                for _ in 0..16 {
                    let deadline = Instant::now() + Duration::from_millis(1);
                    let (mut request, publisher) =
                        table.start(deadline, &account, ValueType::Int).unwrap();
                    let race = std::sync::Barrier::new(2);
                    let released = std::sync::Barrier::new(2);
                    let (terminal, before, after, observed) = std::thread::scope(|scope| {
                        let race = &race;
                        let released = &released;
                        let producer = scope.spawn(move || {
                            race.wait();
                            if late {
                                while Instant::now() < deadline {
                                    std::thread::yield_now();
                                }
                                released.wait();
                            }
                            let before = Instant::now();
                            if complete {
                                publisher.complete(7);
                            } else {
                                drop(publisher);
                            }
                            (before, Instant::now())
                        });
                        race.wait();
                        let terminal = request.wait(Duration::from_secs(1)).unwrap().unwrap();
                        let observed = Instant::now();
                        if late {
                            assert_eq!(terminal, RequestOutcome::TimedOut);
                            assert_eq!(account.used().requests, 1);
                            let before = account.used();
                            assert!(table.start(deadline, &account, ValueType::Int).is_err());
                            assert_eq!(account.used(), before);
                            released.wait();
                        }
                        let (before, after) = producer.join().unwrap();
                        (terminal, before, after, observed)
                    });
                    let published = if complete {
                        RequestOutcome::Reply(7)
                    } else {
                        RequestOutcome::Failed
                    };
                    if after < deadline {
                        assert_eq!(terminal, published);
                    } else if before >= deadline {
                        assert_eq!(terminal, RequestOutcome::TimedOut);
                    } else {
                        assert!(terminal == published || terminal == RequestOutcome::TimedOut);
                    }
                    if terminal == RequestOutcome::TimedOut {
                        assert!(observed >= deadline);
                    }
                    assert!(request.try_take().is_err());
                    assert!(request.cancel().is_err());
                    assert_eq!(account.used().requests, 0);
                    let (mut next, publisher) = table
                        .start(
                            Instant::now() + Duration::from_secs(60),
                            &account,
                            ValueType::Int,
                        )
                        .unwrap();
                    assert_eq!(next.try_take().unwrap(), None);
                    publisher.complete(11);
                    assert_eq!(next.try_take().unwrap(), Some(RequestOutcome::Reply(11)));
                    assert_eq!(account.used().requests, 0);
                }
                drop(table);
                assert_eq!(account.used(), Resources::default());
            }
        }
    }

    #[test]
    fn positive_deadline_cancel_publication_races_keep_a_legal_first_terminal() {
        let limits = Resources {
            bytes: 8192,
            requests: 1,
            ..Resources::default()
        };
        let account = ResourceAccount::vm(limits);
        let table = ReplyTable::new(&account, 1).unwrap();
        for late in [false, true] {
            for _ in 0..32 {
                let deadline = Instant::now() + Duration::from_millis(1);
                let (mut request, publisher) =
                    table.start(deadline, &account, ValueType::Int).unwrap();
                let race = std::sync::Barrier::new(2);
                let (
                    terminal,
                    published_before,
                    published_after,
                    cancelled_before,
                    cancelled_after,
                    observed,
                ) = std::thread::scope(|scope| {
                    let race = &race;
                    let producer = scope.spawn(move || {
                        race.wait();
                        if late {
                            while Instant::now() < deadline {
                                std::thread::yield_now();
                            }
                        }
                        let before = Instant::now();
                        publisher.complete(7);
                        (before, Instant::now())
                    });
                    race.wait();
                    if late {
                        while Instant::now() < deadline {
                            std::thread::yield_now();
                        }
                    }
                    let cancelled_before = Instant::now();
                    request.cancel().unwrap();
                    let cancelled_after = Instant::now();
                    let terminal = request.try_take().unwrap().unwrap();
                    let observed = Instant::now();
                    let (published_before, published_after) = producer.join().unwrap();
                    (
                        terminal,
                        published_before,
                        published_after,
                        cancelled_before,
                        cancelled_after,
                        observed,
                    )
                });
                match terminal {
                    RequestOutcome::Reply(7) => assert!(published_before < deadline),
                    RequestOutcome::Cancelled => assert!(cancelled_before < deadline),
                    RequestOutcome::TimedOut => assert!(observed >= deadline),
                    unexpected => panic!("unexpected terminal: {unexpected:?}"),
                }
                if published_after < deadline && published_after < cancelled_before {
                    assert_eq!(terminal, RequestOutcome::Reply(7));
                }
                if cancelled_after < deadline && cancelled_after < published_before {
                    assert_eq!(terminal, RequestOutcome::Cancelled);
                }
                if published_before >= deadline && cancelled_before >= deadline {
                    assert_eq!(terminal, RequestOutcome::TimedOut);
                }
                assert!(request.try_take().is_err());
                assert!(request.cancel().is_err());
                assert_eq!(account.used().requests, 0);
            }
        }
        drop(table);
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn concurrent_result_cancel_publish_and_drop_keep_one_terminal_and_release_each_slot() {
        for expired in [false, true] {
            for complete in [false, true] {
                let limits = Resources {
                    bytes: 8192,
                    requests: 1,
                    ..Resources::default()
                };
                let account = ResourceAccount::vm(limits);
                let table = ReplyTable::new(&account, 1).unwrap();
                for _ in 0..16 {
                    let deadline = Instant::now()
                        + if expired {
                            Duration::ZERO
                        } else {
                            Duration::from_secs(60)
                        };
                    let (mut request, publisher) =
                        table.start(deadline, &account, ValueType::Int).unwrap();
                    let race = std::sync::Barrier::new(2);
                    let terminal = std::thread::scope(|scope| {
                        let race = &race;
                        let producer = scope.spawn(move || {
                            race.wait();
                            if complete {
                                publisher.complete(7);
                            } else {
                                drop(publisher);
                            }
                        });
                        race.wait();
                        request.cancel().unwrap();
                        let terminal = request.try_take().unwrap().unwrap();
                        assert!(request.try_take().is_err());
                        assert!(request.cancel().is_err());
                        producer.join().unwrap();
                        terminal
                    });
                    if expired {
                        assert_eq!(terminal, RequestOutcome::TimedOut);
                    } else if complete {
                        assert!(matches!(
                            terminal,
                            RequestOutcome::Cancelled | RequestOutcome::Reply(7)
                        ));
                    } else {
                        assert!(matches!(
                            terminal,
                            RequestOutcome::Cancelled | RequestOutcome::Failed
                        ));
                    }
                    assert_eq!(account.used().requests, 0);
                    let (mut next, publisher) = table
                        .start(
                            Instant::now() + Duration::from_secs(60),
                            &account,
                            ValueType::Int,
                        )
                        .unwrap();
                    assert_eq!(next.try_take().unwrap(), None);
                    publisher.complete(11);
                    assert_eq!(next.try_take().unwrap(), Some(RequestOutcome::Reply(11)));
                    assert_eq!(account.used().requests, 0);
                }
                drop(table);
                assert_eq!(account.used(), Resources::default());
            }
        }
    }

    #[test]
    fn shared_results_charge_original_process_and_reject_foreign_paths_and_types() {
        let limits = Resources {
            bytes: 1 << 20,
            requests: 4,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant_accounted(limits).unwrap();
        let first = tenant.process_accounted(limits).unwrap();
        let second = tenant.process_accounted(limits).unwrap();
        let other_tenant = vm.tenant_accounted(limits).unwrap();
        let foreign = other_tenant.process_accounted(limits).unwrap();
        let table = ReplyTable::new(&tenant, 2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let baseline = vm.used();
        assert!(table.start(deadline, &foreign, ValueType::Int).is_err());
        assert!(table.start(deadline, &first, ValueType::RequestId).is_err());
        assert_eq!(vm.used(), baseline);
        let (mut one, publisher_one) = table.start(deadline, &first, ValueType::Bool).unwrap();
        let (mut two, publisher_two) = table.start(deadline, &second, ValueType::Int).unwrap();
        assert_eq!(first.used().requests, 1);
        assert_eq!(second.used().requests, 1);
        let full = vm.used();
        assert!(table.start(deadline, &first, ValueType::Unit).is_err());
        assert_eq!(vm.used(), full);
        one.cancel().unwrap();
        assert_eq!(one.try_take().unwrap(), Some(RequestOutcome::Cancelled));
        assert_eq!(first.used().requests, 1);
        assert!(table.start(deadline, &second, ValueType::Unit).is_err());
        drop(publisher_one);
        assert_eq!(first.used().requests, 0);
        publisher_two.complete(7);
        assert_eq!(two.try_take().unwrap(), Some(RequestOutcome::Reply(7)));
        assert_eq!(second.used().requests, 0);
        drop((
            one,
            two,
            table,
            first,
            second,
            foreign,
            tenant,
            other_tenant,
        ));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn shared_results_respect_each_tier_request_admission_without_partial_slots() {
        for level in 0..3 {
            let limits = Resources {
                bytes: 1 << 20,
                requests: 1,
                ..Resources::default()
            };
            let vm = ResourceAccount::vm(limits);
            let tenant = vm.tenant_accounted(limits).unwrap();
            let account = tenant.process_accounted(limits).unwrap();
            let levels = [&vm, &tenant, &account];
            let table = ReplyTable::new(&tenant, 1).unwrap();
            let guard = levels[level]
                .reserve(Resources {
                    requests: 1,
                    ..Resources::default()
                })
                .unwrap();
            let baseline = levels.map(ResourceAccount::used);
            let deadline = Instant::now() + Duration::from_secs(60);
            assert!(table.start(deadline, &account, ValueType::Int).is_err());
            assert_eq!(levels.map(ResourceAccount::used), baseline);
            drop(guard);
            let (mut result, publisher) = table.start(deadline, &account, ValueType::Int).unwrap();
            publisher.complete(3);
            assert_eq!(result.try_take().unwrap(), Some(RequestOutcome::Reply(3)));
            assert_eq!(account.used().requests, 0);
            drop((result, table, account, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn retired_generation_never_wraps_and_old_publisher_cannot_finish_reused_slot() {
        let limits = Resources {
            bytes: 8192,
            requests: 2,
            ..Resources::default()
        };
        let account = ResourceAccount::vm(limits);
        let table = ReplyTable::new(&account, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let (mut first, old) = table.start(deadline, &account, ValueType::Int).unwrap();
        table.finish(old.slot, old.generation, RequestOutcome::Reply(1));
        assert_eq!(first.try_take().unwrap(), Some(RequestOutcome::Reply(1)));
        let (mut second, publisher) = table.start(deadline, &account, ValueType::Int).unwrap();
        drop(old);
        assert_eq!(second.try_take().unwrap(), None);
        publisher.complete(2);
        assert_eq!(second.try_take().unwrap(), Some(RequestOutcome::Reply(2)));
        table.slots[0].lock().generation = u64::MAX;
        let (mut last, publisher) = table.start(deadline, &account, ValueType::Int).unwrap();
        publisher.complete(3);
        assert_eq!(last.try_take().unwrap(), Some(RequestOutcome::Reply(3)));
        assert!(table.start(deadline, &account, ValueType::Int).is_err());
        drop((first, second, last, table));
        assert_eq!(account.used(), Resources::default());
    }
}
