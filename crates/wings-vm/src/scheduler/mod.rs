use std::cell::UnsafeCell;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::host_values::HostMessage;
use crate::ingress_requests::{ReplyPublisher, ReplyTable};
use crate::message_values::{BytePayload, ListPayload, MessageArguments};
use crate::{
    CancellationToken, DetachedProcess, ExecutionFault, ExecutionSnapshot, HandlerId,
    IngressRequest, MessageValue, Reservation, ResourceAccount, ResourceLevel, Resources,
    ScalarModule, StackPool,
};

mod cpu;
mod locks;
mod ready;
mod run;
mod slices;
mod state;
mod supervisor;
mod yields;
pub use cpu::SchedulerWorkerCpu;
pub use locks::{
    SchedulerContentionMetrics, SchedulerContentionStatistics, SchedulerLockMetrics,
    SchedulerLockStatistics,
};
pub use slices::{SchedulerSliceOutcome, SchedulerSliceSample, SchedulerSliceStatistics};
pub use supervisor::{
    SchedulerChildId, SchedulerRestartStatus, SchedulerSupervisor, SchedulerSupervisorEvent,
    SchedulerTermination,
};
pub use yields::{SchedulerYieldSample, SchedulerYieldStatistics};
#[cfg(test)]
mod tests;

const MAX_ACTORS: usize = 16384;
const MAX_REQUESTS: usize = 256;
const MAX_MAILBOX: usize = 256;
const MAX_TIMERS: usize = 1024;
const MAX_MONITORS: usize = 1024;
const THREAD_STACK: usize = 2 << 20;
static NEXT_SCHEDULER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub struct SchedulerConfig {
    pub workers: usize,
    pub capacity: usize,
    pub requests: usize,
    pub slice_steps: u64,
    pub quantum: Duration,
    pub yield_samples: usize,
    pub slice_samples: usize,
    pub lock_statistics: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            capacity: 64,
            requests: 64,
            slice_steps: 64,
            quantum: Duration::from_millis(1),
            yield_samples: 0,
            slice_samples: 0,
            lock_statistics: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerPid {
    scheduler: u64,
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub owner: Option<usize>,
    pub last_worker: Option<usize>,
    pub migrations: u64,
    pub queued: usize,
    pub stopping: bool,
    pub execution: Option<ExecutionSnapshot>,
    pub committed_state: Option<i64>,
    pub resources: Resources,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerExitReason {
    Managed(ExecutionFault),
    ExecutionControl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerExitSnapshot {
    pub pid: SchedulerPid,
    pub worker: usize,
    pub reason: SchedulerExitReason,
    pub execution: Option<ExecutionSnapshot>,
    pub state_type: wings_abi::ValueType,
    pub committed_state: Option<i64>,
    pub byte_state: Option<crate::ByteStateSnapshot>,
    pub queued: usize,
    pub resources_before_cleanup: Resources,
    handler: [u8; 256],
    handler_len: usize,
}

impl SchedulerExitSnapshot {
    pub fn handler(&self) -> &str {
        std::str::from_utf8(&self.handler[..self.handler_len]).expect("validated handler name")
    }
}

pub struct Scheduler {
    handle: SchedulerHandle,
    threads: Vec<JoinHandle<()>>,
    _threads_charge: Reservation,
}

#[derive(Clone)]
pub struct SchedulerHandle {
    shared: Arc<Shared>,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    closed: AtomicBool,
    fast_slots: Box<[FastSlot]>,
    changed: ready::WorkerWakeHub,
    ready_mask: AtomicU32,
    tenant: ResourceAccount,
    replies: Arc<ReplyTable>,
    config: SchedulerConfig,
    identity: u64,
    observations: Box<[yields::WorkerObservation]>,
    slice_observations: Box<[slices::WorkerSliceObservation]>,
    lock_observations: Box<[locks::WorkerLockObservation]>,
    contention_observation: Option<Arc<locks::LockContentionObservation>>,
    _charge: Reservation,
}

pub(crate) struct State {
    closed: bool,
    ready_bits: u32,
    published_signals: u32,
    worker_active: [u32; 16],
    last_exit: Option<SchedulerExitSnapshot>,
    modules: Option<(Box<[ScalarModule]>, Reservation)>,
    slots: Box<[Slot]>,
    requests: state::RequestTable,
    timers: Option<Box<ScheduledTimer>>,
    timer_count: usize,
    monitors: Option<Box<MonitorEntry>>,
    pending_monitors: Option<Box<MonitorEntry>>,
    monitor_count: usize,
    recent_exits: [(i64, i64); 32],
    recent_exit_cursor: usize,
    ready: Box<[ready::ReadyQueue]>,
    free: Option<usize>,
}

struct ScheduledTimer {
    target: SchedulerPid,
    deadline: Instant,
    envelope: Envelope,
    _node_charge: Reservation,
    next: Option<Box<ScheduledTimer>>,
}

struct MonitorEntry {
    owner: SchedulerPid,
    target_capability: i64,
    handler_index: usize,
    fast_eligible: bool,
    max_steps: u64,
    reason: Option<i64>,
    envelope_charge: crate::resources::EnvelopeCharge,
    _node_charge: Reservation,
    next: Option<Box<MonitorEntry>>,
}

struct RetiredActor {
    actor: Actor,
    timers: Option<Box<ScheduledTimer>>,
    monitors: Option<Box<MonitorEntry>>,
}

#[cfg(test)]
impl RetiredActor {
    fn into_actor(mut self) -> Actor {
        clear_timers(&mut self.timers);
        clear_monitors(&mut self.monitors);
        self.actor
    }
}

fn clear_timers(head: &mut Option<Box<ScheduledTimer>>) {
    while let Some(mut node) = head.take() {
        *head = node.next.take();
    }
}

fn clear_monitors(head: &mut Option<Box<MonitorEntry>>) {
    while let Some(mut node) = head.take() {
        *head = node.next.take();
    }
}

struct Slot {
    generation: u32,
    creating: bool,
    actor: Option<Actor>,
    free_next: Option<usize>,
    ready_previous: Option<usize>,
    ready_next: Option<usize>,
    ready_worker: Option<usize>,
}

struct Actor {
    module: ScalarModule,
    account: ResourceAccount,
    image: Option<DetachedProcess>,
    owner: Option<usize>,
    last_worker: Option<usize>,
    migrations: u64,
    execution: Option<ExecutionSnapshot>,
    committed_state: Option<i64>,
    stopping: bool,
    cancellation: CancellationToken,
    capability: i64,
    heap_records: Option<usize>,
    mailbox: ActorMailbox,
    _charge: Reservation,
    termination: Option<supervisor::ExitPublisher>,
}

struct ActorMailbox(SharedMailbox);

impl Deref for ActorMailbox {
    type Target = SharedMailbox;
    fn deref(&self) -> &SharedMailbox {
        &self.0
    }
}

impl Drop for ActorMailbox {
    fn drop(&mut self) {
        self.0.retire_clear();
    }
}

const FAST_SLOT_WRITER: u32 = 1 << 31;

struct FastSlot {
    state: AtomicU32,
    generation: AtomicU32,
    entry: UnsafeCell<Option<Weak<SharedMailboxInner>>>,
}

unsafe impl Send for FastSlot {}
unsafe impl Sync for FastSlot {}

impl Default for FastSlot {
    fn default() -> Self {
        Self {
            state: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            entry: UnsafeCell::new(None),
        }
    }
}

impl FastSlot {
    fn write_lock(
        &self,
        observation: Option<&locks::ContentionCounters>,
    ) -> FastSlotWriteGuard<'_> {
        if self
            .state
            .compare_exchange_weak(0, FAST_SLOT_WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return FastSlotWriteGuard(self);
        }
        let mut spins = 0u32;
        let mut failures = 0u64;
        let mut yields = 0u64;
        let contended_at = observation.map(|_| Instant::now());
        loop {
            let current = self.state.load(Ordering::Relaxed);
            if (current & FAST_SLOT_WRITER) == 0 {
                match self.state.compare_exchange_weak(
                    current,
                    current | FAST_SLOT_WRITER,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(_) => {
                        failures = failures.saturating_add(1);
                        continue;
                    }
                }
            }
            failures = failures.saturating_add(1);
            if spins < 16 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                yields = yields.saturating_add(1);
                std::thread::yield_now();
            }
        }
        while (self.state.load(Ordering::Acquire) & !FAST_SLOT_WRITER) != 0 {
            if spins < 16 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                yields = yields.saturating_add(1);
                std::thread::yield_now();
            }
        }
        if let (Some(counters), Some(started)) = (observation, contended_at) {
            counters.record_contended(failures.max(1), yields, started.elapsed());
        }
        FastSlotWriteGuard(self)
    }

    fn read_lock(&self, observation: Option<&locks::ContentionCounters>) -> FastSlotReadGuard<'_> {
        let mut spins = 0u32;
        let mut failures = 0u64;
        let mut yields = 0u64;
        let mut contended_at = None;
        loop {
            let current = self.state.load(Ordering::Relaxed);
            if (current & FAST_SLOT_WRITER) == 0 && current < FAST_SLOT_WRITER - 1 {
                if self
                    .state
                    .compare_exchange_weak(
                        current,
                        current + 1,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
                std::hint::spin_loop();
                continue;
            }
            failures = failures.saturating_add(1);
            if observation.is_some() && contended_at.is_none() {
                contended_at = Some(Instant::now());
            }
            while (self.state.load(Ordering::Relaxed) & FAST_SLOT_WRITER) != 0 {
                if spins < 16 {
                    std::hint::spin_loop();
                    spins += 1;
                } else {
                    yields = yields.saturating_add(1);
                    std::thread::yield_now();
                }
            }
        }
        if let (Some(counters), Some(started)) = (observation, contended_at) {
            counters.record_contended(failures, yields, started.elapsed());
        }
        FastSlotReadGuard(self)
    }

    fn set(
        &self,
        generation: u32,
        mailbox: &SharedMailbox,
        observation: Option<&locks::ContentionCounters>,
    ) {
        let mut guard = self.write_lock(observation);
        *guard = Some(Arc::downgrade(&mailbox.inner));
        self.generation.store(generation, Ordering::Release);
    }

    fn clear(&self, observation: Option<&locks::ContentionCounters>) {
        let current = {
            let mut guard = self.write_lock(observation);
            self.generation.store(0, Ordering::Release);
            guard.take()
        };
        drop(current);
    }

    fn get(
        &self,
        generation: u32,
        observation: Option<&locks::ContentionCounters>,
    ) -> Option<SharedMailbox> {
        if self.generation.load(Ordering::Acquire) != generation {
            return None;
        }
        let guard = self.read_lock(observation);
        if self.generation.load(Ordering::Relaxed) != generation {
            return None;
        }
        guard
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|inner| SharedMailbox { inner })
            .filter(|mailbox| !mailbox.is_retired())
    }
}

struct FastSlotReadGuard<'slot>(&'slot FastSlot);

impl std::ops::Deref for FastSlotReadGuard<'_> {
    type Target = Option<Weak<SharedMailboxInner>>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.entry.get() }
    }
}

impl Drop for FastSlotReadGuard<'_> {
    fn drop(&mut self) {
        self.0.state.fetch_sub(1, Ordering::Release);
    }
}

struct FastSlotWriteGuard<'slot>(&'slot FastSlot);

impl std::ops::Deref for FastSlotWriteGuard<'_> {
    type Target = Option<Weak<SharedMailboxInner>>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.entry.get() }
    }
}

impl std::ops::DerefMut for FastSlotWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.0.entry.get() }
    }
}

impl Drop for FastSlotWriteGuard<'_> {
    fn drop(&mut self) {
        self.0.state.store(0, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct SharedMailbox {
    inner: Arc<SharedMailboxInner>,
}

struct SharedMailboxInner {
    module: ScalarModule,
    account: ResourceAccount,
    capacity: usize,
    count_atomic: AtomicUsize,
    // Packed lock-free hint for the head envelope:
    // bit 63 = fast_eligible, bits 31..62 = min(max_steps, u32::MAX),
    // bits 0..30 = handler index (only meaningful while bit 63 is set).
    front_hint: AtomicU64,
    in_flight: AtomicUsize,
    stopping: AtomicBool,
    active_or_queued: AtomicBool,
    retired: AtomicBool,
    locked: AtomicBool,
    live_execution_locked: AtomicBool,
    live_execution: UnsafeCell<Option<ExecutionSnapshot>>,
    contention_observation: Option<Arc<locks::LockContentionObservation>>,
    state: UnsafeCell<Mailbox>,
}

unsafe impl Send for SharedMailboxInner {}
unsafe impl Sync for SharedMailboxInner {}

struct Mailbox {
    slots: Box<[Option<Envelope>]>,
    head: usize,
    count: usize,
}

pub(crate) struct Envelope {
    handler: usize,
    arguments: MessageArguments,
    max_steps: u64,
    // List/byte payloads stay boxed behind `None` for scalar messages so the
    // envelope copies at claim, restore, and batch-pop time stay small.
    lists: Option<Box<ListPayload>>,
    bytes: Option<Box<BytePayload>>,
    reply: Reply,
    fast_eligible: bool,
    _charge: crate::resources::EnvelopeCharge,
}

fn box_list_payload(payload: ListPayload) -> Option<Box<ListPayload>> {
    (payload.mask != 0 || !payload.is_empty()).then(|| Box::new(payload))
}

fn box_byte_payload(payload: BytePayload) -> Option<Box<BytePayload>> {
    (!payload.is_trivially_empty()).then(|| Box::new(payload))
}

fn envelope_fast_eligible(handler: &HandlerId, lists: &ListPayload, bytes: &BytePayload) -> bool {
    crate::execution::WorkerFibers::can_run_fast(handler, lists, bytes)
}

enum Reply {
    None,
    External(ReplyPublisher),
    Language(RequestKey),
}

#[derive(Clone, Copy)]
struct RequestKey {
    slot: usize,
    generation: u64,
}

struct RequestSlot {
    generation: u64,
    pending: Option<Pending>,
}

struct Pending {
    owner: SchedulerPid,
    target: SchedulerPid,
    capability: i64,
    deadline: Instant,
    outcome: Option<crate::RequestOutcome>,
    publisher: bool,
    completion: Option<Completion>,
    _charge: Reservation,
}

struct Completion {
    handler: HandlerId,
    max_steps: u64,
    charge: Reservation,
}

impl Scheduler {
    pub fn new(tenant: &ResourceAccount, config: SchedulerConfig) -> Result<Self, String> {
        Self::new_with_stack_check(tenant, config, crate::native_stack::check_current)
    }

    fn new_with_stack_check(
        tenant: &ResourceAccount,
        config: SchedulerConfig,
        stack_check: fn(usize) -> Result<(), &'static str>,
    ) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant
            || !(1..=16).contains(&config.workers)
            || !(1..=MAX_ACTORS).contains(&config.capacity)
            || !(1..=MAX_REQUESTS).contains(&config.requests)
            || config.slice_steps == 0
            || config.quantum.is_zero()
            || config.quantum > Duration::from_secs(1)
            || config.yield_samples > 65536
            || config.slice_samples > 65536
        {
            return Err("scheduler requires a tenant, 1..=16 workers, 1..=16384 actor slots, 1..=256 request slots, nonzero bounded slices and 0..=65536 yield/slice samples per worker".into());
        }
        let bytes = std::mem::size_of::<Shared>()
            + config.capacity * (std::mem::size_of::<Slot>() + std::mem::size_of::<FastSlot>())
            + config.requests * std::mem::size_of::<RequestSlot>()
            + config.workers * std::mem::size_of::<ready::ReadyQueue>()
            + if config.yield_samples == 0 {
                0
            } else {
                config.workers * std::mem::size_of::<yields::WorkerObservation>()
            }
            + if config.lock_statistics {
                config.workers * std::mem::size_of::<locks::WorkerLockObservation>()
                    + std::mem::size_of::<locks::LockContentionObservation>()
                    + 2 * std::mem::size_of::<usize>()
            } else {
                0
            }
            + if config.slice_samples == 0 {
                0
            } else {
                config.workers * std::mem::size_of::<slices::WorkerSliceObservation>()
            }
            + 256usize.max(crate::native_sync::shared_bytes(1, 1));
        let charge = tenant.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let thread_bytes = config.workers
            * (THREAD_STACK
                + 65536
                + config.capacity * std::mem::size_of::<Option<run::Running>>());
        let threads_charge = tenant.reserve(Resources {
            bytes: thread_bytes as u64,
            ..Resources::default()
        })?;
        let identity = next_identity(&NEXT_SCHEDULER)?;
        let contention_observation = config
            .lock_statistics
            .then(|| Arc::new(locks::LockContentionObservation::default()));
        let mut observations = Vec::new();
        if config.yield_samples != 0 {
            observations
                .try_reserve_exact(config.workers)
                .map_err(|error| error.to_string())?;
            if observations.capacity() != config.workers {
                return Err("allocator changed scheduler observer capacity".into());
            }
            for _ in 0..config.workers {
                observations.push(yields::WorkerObservation::new(
                    tenant,
                    config.yield_samples,
                )?);
            }
        }
        let mut next_slot = 0;
        let mut slice_observations = Vec::new();
        if config.slice_samples != 0 {
            slice_observations
                .try_reserve_exact(config.workers)
                .map_err(|_| "scheduler slice observer allocation failed")?;
            if slice_observations.capacity() != config.workers {
                return Err("allocator changed scheduler slice observer capacity".into());
            }
            for _ in 0..config.workers {
                slice_observations.push(slices::WorkerSliceObservation::new(
                    tenant,
                    config.slice_samples,
                )?);
            }
        }
        let shared = Arc::new(Shared {
            state: crate::native_sync::mutex(State {
                closed: false,
                ready_bits: 0,
                published_signals: 0,
                worker_active: [0; 16],
                last_exit: None,
                modules: None,
                slots: storage(config.capacity, || {
                    next_slot += 1;
                    Slot {
                        generation: 1,
                        creating: false,
                        actor: None,
                        free_next: (next_slot < config.capacity).then_some(next_slot),
                        ready_previous: None,
                        ready_next: None,
                        ready_worker: None,
                    }
                })?,
                requests: state::RequestTable::new(storage(config.requests, || RequestSlot {
                    generation: 1,
                    pending: None,
                })?),
                timers: None,
                timer_count: 0,
                monitors: None,
                pending_monitors: None,
                monitor_count: 0,
                recent_exits: [(0, 0); 32],
                recent_exit_cursor: 0,
                ready: storage(config.workers, ready::ReadyQueue::default)?,
                free: Some(0),
            }),
            closed: AtomicBool::new(false),
            fast_slots: storage(config.capacity, FastSlot::default)?,
            changed: ready::WorkerWakeHub::new(),
            ready_mask: AtomicU32::new(0),
            tenant: tenant.clone(),
            replies: ReplyTable::new(tenant, config.requests)?,
            config,
            identity,
            observations: observations.into_boxed_slice(),
            slice_observations: slice_observations.into_boxed_slice(),
            lock_observations: storage(
                if config.lock_statistics {
                    config.workers
                } else {
                    0
                },
                locks::WorkerLockObservation::default,
            )?,
            contention_observation,
            _charge: charge,
        });
        let mut scheduler = Self {
            handle: SchedulerHandle { shared },
            threads: Vec::new(),
            _threads_charge: threads_charge,
        };
        scheduler
            .threads
            .try_reserve_exact(config.workers)
            .map_err(|error| error.to_string())?;
        let (ready_sender, ready_receiver) = mpsc::sync_channel(config.workers);
        for index in 0..config.workers {
            let shared = scheduler.handle.shared.clone();
            let ready = ready_sender.clone();
            let thread = std::thread::Builder::new()
                .name(format!("wings-worker-{index}"))
                .stack_size(THREAD_STACK)
                .spawn(move || run::worker(shared, index, ready, stack_check))
                .map_err(|error| format!("scheduler thread startup: {error}"))?;
            scheduler.threads.push(thread);
        }
        drop(ready_sender);
        for _ in 0..config.workers {
            ready_receiver
                .recv()
                .map_err(|_| "scheduler worker failed during startup")??;
        }
        Ok(scheduler)
    }

    pub fn handle(&self) -> SchedulerHandle {
        self.handle.clone()
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.handle.shared.close();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let mut state = self
            .handle
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for index in 0..state.slots.len() {
            self.handle.shared.fast_slots[index].clear(self.handle.shared.fast_slot_contention());
            let mut actor = state.slots[index].actor.take();
            if let Some(actor_ref) = actor.as_ref() {
                actor_ref.mailbox.retire_clear();
            }
            if let Some(publisher) = actor.as_mut().and_then(|actor| actor.termination.as_mut()) {
                publisher.set(SchedulerTermination::Closed);
            }
            state.slots[index].creating = false;
            drop(state);
            drop(actor);
            state = self
                .handle
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
        }
        let mut timers = state.timers.take();
        state.timer_count = 0;
        let mut monitors = state.monitors.take();
        let mut pending_monitors = state.pending_monitors.take();
        state.monitor_count = 0;
        state.requests.clear_all();
        drop(state);
        clear_timers(&mut timers);
        clear_monitors(&mut monitors);
        clear_monitors(&mut pending_monitors);
        let mut state = self
            .handle
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.modules = None;
    }
}

impl SchedulerHandle {
    pub fn register_modules(&self, modules: &[ScalarModule]) -> Result<(), String> {
        if modules.is_empty() || modules.len() > wings_artifact::MAX_BUNDLE_ACTORS {
            return Err("scheduler module registry requires 1..=32 modules".into());
        }
        let bytes = std::mem::size_of_val(modules);
        let charge = self.shared.tenant.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(modules.len())
            .map_err(|error| error.to_string())?;
        if owned.capacity() != modules.len() {
            return Err("allocator changed scheduler module registry capacity".into());
        }
        owned.extend_from_slice(modules);
        let mut state = self.shared.lock();
        if state.closed {
            return Err("scheduler is closed".into());
        }
        state.modules = Some((owned.into_boxed_slice(), charge));
        Ok(())
    }

    pub fn take_slice_sample(&self, worker: usize) -> Result<Option<SchedulerSliceSample>, String> {
        if worker >= self.shared.config.workers {
            return Err("scheduler worker index out of range".into());
        }
        Ok(self
            .shared
            .slice_observations
            .get(worker)
            .and_then(slices::WorkerSliceObservation::take))
    }

    pub fn slice_statistics(
        &self,
        worker: usize,
    ) -> Result<Option<SchedulerSliceStatistics>, String> {
        if worker >= self.shared.config.workers {
            return Err("scheduler worker index out of range".into());
        }
        Ok(self
            .shared
            .slice_observations
            .get(worker)
            .map(slices::WorkerSliceObservation::statistics))
    }

    pub fn lock_statistics(
        &self,
        worker: usize,
    ) -> Result<Option<SchedulerLockStatistics>, String> {
        if worker >= self.shared.config.workers {
            return Err("scheduler worker index out of range".into());
        }
        Ok(self
            .shared
            .lock_observations
            .get(worker)
            .map(locks::WorkerLockObservation::snapshot))
    }

    pub fn contention_statistics(&self) -> Option<SchedulerContentionStatistics> {
        self.shared
            .contention_observation
            .as_deref()
            .map(locks::LockContentionObservation::snapshot)
    }

    pub fn take_yield_sample(&self, worker: usize) -> Result<Option<SchedulerYieldSample>, String> {
        if worker >= self.shared.config.workers {
            return Err("scheduler worker index out of range".into());
        }
        Ok(self
            .shared
            .observations
            .get(worker)
            .and_then(yields::WorkerObservation::take))
    }

    pub fn yield_statistics(
        &self,
        worker: usize,
    ) -> Result<Option<SchedulerYieldStatistics>, String> {
        if worker >= self.shared.config.workers {
            return Err("scheduler worker index out of range".into());
        }
        Ok(self
            .shared
            .observations
            .get(worker)
            .map(yields::WorkerObservation::statistics))
    }

    pub fn spawn(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
    ) -> Result<SchedulerPid, String> {
        self.spawn_with_options(module, limits, mailbox, None, None, None)
    }

    pub fn spawn_with_heap(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
    ) -> Result<SchedulerPid, String> {
        self.spawn_with_options(module, limits, mailbox, Some(records), None, None)
    }

    pub fn spawn_with_heap_in_pool(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
        pool: &StackPool,
    ) -> Result<SchedulerPid, String> {
        self.spawn_with_options(module, limits, mailbox, Some(records), Some(pool), None)
    }

    fn spawn_with_options(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        heap_records: Option<usize>,
        pool: Option<&StackPool>,
        termination: Option<supervisor::ExitPublisher>,
    ) -> Result<SchedulerPid, String> {
        if !(1..=MAX_MAILBOX).contains(&mailbox) {
            return Err("scheduler mailbox requires 1..=256 slots".into());
        }
        module.validate_vm(&self.shared.tenant)?;
        let pid = {
            let mut state = self.shared.lock();
            state.open()?;
            let index = state.reserve_slot()?;
            state.pid(self.shared.identity, index)
        };
        let result = (|| {
            let account = self.shared.tenant.process_accounted(limits)?;
            let charge = account.reserve(Resources {
                processes: 1,
                bytes: (std::mem::size_of::<Actor>()
                    + std::mem::size_of::<SharedMailboxInner>()
                    + 2 * std::mem::size_of::<usize>()
                    + (mailbox + 1) * std::mem::size_of::<Option<Envelope>>())
                    as u64,
                ..Resources::default()
            })?;
            // One spare ring slot keeps the in-flight reservation physical, so
            // `restore` can never overwrite a concurrently admitted message.
            let slots = storage(mailbox + 1, || None)?;
            let temporary;
            let pool = if let Some(pool) = pool {
                pool
            } else {
                temporary = StackPool::new_accounted(&self.shared.tenant, 1, 0)?;
                &temporary
            };
            let bound = pool.for_process(&account)?;
            let process = if let Some(records) = heap_records {
                module.instantiate_with_heap(&bound, &account, records)?
            } else {
                module.instantiate_in_pool(&bound)?
            };
            let committed_state =
                (!module.manifest().state.is_reference()).then_some(process.state);
            let image = process.detach();
            let cancellation = CancellationToken::new_accounted(&account)?;
            let shared_mailbox = SharedMailbox::new(
                module.clone(),
                account.clone(),
                slots,
                self.shared.contention_observation.clone(),
            );
            Ok::<_, String>(Actor {
                module: module.clone(),
                account,
                image: Some(image),
                owner: None,
                last_worker: None,
                migrations: 0,
                execution: None,
                committed_state,
                stopping: false,
                cancellation,
                capability: wings_abi::encode_capability(
                    self.shared.identity,
                    pid.slot,
                    pid.generation,
                )
                .ok_or("scheduler actor capabilities exhausted")?,
                heap_records,
                mailbox: ActorMailbox(shared_mailbox),
                _charge: charge,
                termination,
            })
        })();
        let mut state = self.shared.lock();
        state.slots[pid.slot as usize].creating = false;
        match result {
            Ok(actor) if !state.closed => {
                self.shared.fast_slots[pid.slot as usize].set(
                    pid.generation,
                    &actor.mailbox,
                    self.shared.fast_slot_contention(),
                );
                state.slots[pid.slot as usize].actor = Some(actor);
                drop(state);
                Ok(pid)
            }
            Ok(actor) => {
                actor.mailbox.retire_clear();
                state.release_slot(pid.slot as usize);
                drop(state);
                drop(actor);
                Err("scheduler is closed".into())
            }
            Err(error) => {
                state.release_slot(pid.slot as usize);
                Err(error)
            }
        }
    }

    pub fn send(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
    ) -> Result<(), String> {
        let fast_mailbox = if !self.shared.closed.load(Ordering::Acquire)
            && self.shared.identity == pid.scheduler
        {
            self.shared
                .fast_slots
                .get(pid.slot as usize)
                .and_then(|slot| slot.get(pid.generation, self.shared.fast_slot_contention()))
        } else {
            None
        };
        if let Some(mailbox) = fast_mailbox {
            validate_scalar_module(&mailbox.inner.module, false, handler, arguments)?;
            let meta = handler.fast_meta();
            if meta.reply.is_some() {
                return Err("send requires a one-way handler".into());
            }
            if mailbox.occupancy_hint() < mailbox.capacity() {
                let charge = mailbox
                    .inner
                    .account
                    .reserve_envelope(
                        std::mem::size_of::<Envelope>() as u64
                            + MessageArguments::overflow_bytes(arguments.len()),
                    )
                    .map_err(String::from)?;
                let message_arguments =
                    MessageArguments::try_from_slice(arguments).map_err(String::from)?;
                let message = Envelope {
                    handler: handler.index,
                    arguments: message_arguments,
                    max_steps,
                    lists: None,
                    bytes: None,
                    reply: Reply::None,
                    fast_eligible: meta.fast_scalar(),
                    _charge: charge,
                };
                match mailbox.try_push_fast(&self.shared.closed, message) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {
                        let mut state = self.shared.lock();
                        let wake = state.enqueue_fast_pushed(pid, &mailbox);
                        if !state.closed {
                            self.shared.publish_signal(&mut state);
                            drop(state);
                            wake.fire(&self.shared.changed);
                        }
                        return Ok(());
                    }
                    Err(FastPushError::MailboxFull) => return Err("mailbox_full".into()),
                    Err(FastPushError::ClosedOrStopped) => {}
                }
            }
        }
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate_scalar(actor, handler, arguments)?;
        if handler.fast_meta().reply.is_some() {
            return Err("send requires a one-way handler".into());
        }
        let message = envelope_precopied(actor, handler, arguments, max_steps, Reply::None)?;
        actor.mailbox.push(message)?;
        let wake = state.enqueue(pid);
        self.shared.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.shared.changed);
        Ok(())
    }

    pub fn request(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        timeout: Duration,
    ) -> Result<IngressRequest, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("request deadline overflow")?;
        let fast_mailbox = if !self.shared.closed.load(Ordering::Acquire)
            && self.shared.identity == pid.scheduler
        {
            self.shared
                .fast_slots
                .get(pid.slot as usize)
                .and_then(|slot| slot.get(pid.generation, self.shared.fast_slot_contention()))
        } else {
            None
        };
        if let Some(mailbox) = fast_mailbox {
            validate_scalar_module(&mailbox.inner.module, false, handler, arguments)?;
            let meta = handler.fast_meta();
            let Some(reply) = meta.reply.filter(|_| meta.has_supported_reply()) else {
                return Err(
                    "request requires a scalar, Pid, IntList, Bytes, String or Record reply".into(),
                );
            };
            if mailbox.occupancy_hint() < mailbox.capacity() {
                let charge = mailbox
                    .inner
                    .account
                    .reserve_envelope(
                        std::mem::size_of::<Envelope>() as u64
                            + MessageArguments::overflow_bytes(arguments.len()),
                    )
                    .map_err(String::from)?;
                let message_arguments =
                    MessageArguments::try_from_slice(arguments).map_err(String::from)?;
                let (request, publisher) =
                    self.shared
                        .replies
                        .start(deadline, &mailbox.inner.account, reply)?;
                let message = Envelope {
                    handler: handler.index,
                    arguments: message_arguments,
                    max_steps,
                    lists: None,
                    bytes: None,
                    reply: Reply::External(publisher),
                    fast_eligible: meta.fast_scalar(),
                    _charge: charge,
                };
                match mailbox.try_push_fast(&self.shared.closed, message) {
                    Ok(true) => return Ok(request),
                    Ok(false) => {
                        let mut state = self.shared.lock();
                        let wake = state.enqueue_fast_pushed(pid, &mailbox);
                        if !state.closed {
                            self.shared.publish_signal(&mut state);
                            drop(state);
                            wake.fire(&self.shared.changed);
                        }
                        return Ok(request);
                    }
                    Err(FastPushError::MailboxFull) => return Err("mailbox_full".into()),
                    Err(FastPushError::ClosedOrStopped) => {}
                }
            }
        }
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate_scalar(actor, handler, arguments)?;
        let meta = handler.fast_meta();
        let Some(reply) = meta.reply.filter(|_| meta.has_supported_reply()) else {
            return Err(
                "request requires a scalar, Pid, IntList, Bytes, String or Record reply".into(),
            );
        };
        let mut message = envelope_precopied(actor, handler, arguments, max_steps, Reply::None)?;
        let (request, publisher) = self.shared.replies.start(deadline, &actor.account, reply)?;
        message.reply = Reply::External(publisher);
        actor.mailbox.push(message)?;
        let wake = state.enqueue(pid);
        self.shared.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.shared.changed);
        Ok(request)
    }

    pub fn stop(&self, pid: SchedulerPid) -> Result<(), String> {
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        actor.stopping = true;
        actor.mailbox.mark_stopping();
        actor.cancellation.cancel();
        self.shared.fast_slots[pid.slot as usize].clear(self.shared.fast_slot_contention());
        let retired = if actor.owner.is_none() {
            state.retire_with_reason(self.shared.identity, pid, 0)
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
            .maintain(self.shared.identity)
            .combine(state.wake_all_parked());
        self.shared.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.shared.changed);
        Ok(())
    }

    pub fn send_values(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        max_steps: u64,
    ) -> Result<(), String> {
        let message = self.host_message(pid, handler, values, false)?;
        self.publish_values(pid, handler, message, max_steps, None)?;
        Ok(())
    }

    pub fn request_values(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        max_steps: u64,
        timeout: Duration,
    ) -> Result<IngressRequest, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("request deadline overflow")?;
        let message = self.host_message(pid, handler, values, true)?;
        self.publish_values(pid, handler, message, max_steps, Some(deadline))?
            .ok_or_else(|| "host request result was not created".into())
    }

    fn host_message(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        request: bool,
    ) -> Result<HostMessage, String> {
        let account = {
            let state = self.shared.lock();
            state.open()?;
            let actor = state.actor(self.shared.identity, pid)?;
            if actor.stopping
                || !Arc::ptr_eq(&actor.module.inner, &handler.module.inner)
                || handler.schema().is_completion()
                || handler.schema().reply.is_some() != request
            {
                return Err("host message module, phase or handler mode mismatch".into());
            }
            HostMessage::validate(handler, values)?;
            if actor.mailbox.count() + actor.mailbox.in_flight() >= actor.mailbox.capacity() {
                return Err("mailbox_full".into());
            }
            actor.account.clone()
        };
        HostMessage::copy(handler, values, &account)
    }

    fn publish_values(
        &self,
        pid: SchedulerPid,
        handler: &HandlerId,
        message: HostMessage,
        max_steps: u64,
        deadline: Option<Instant>,
    ) -> Result<Option<IngressRequest>, String> {
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        let arguments = &message.arguments[..message.count];
        validate(
            actor,
            handler,
            arguments,
            false,
            &message.lists,
            &message.bytes,
        )?;
        let mut envelope = envelope(
            actor,
            handler,
            arguments,
            max_steps,
            message.lists,
            message.bytes,
            false,
            Reply::None,
        )?;
        let request = if let Some(deadline) = deadline {
            let reply_type = handler
                .schema()
                .reply
                .ok_or("request requires a reply handler")?;
            let (request, publisher) =
                self.shared
                    .replies
                    .start(deadline, &actor.account, reply_type)?;
            envelope.reply = Reply::External(publisher);
            Some(request)
        } else {
            None
        };
        actor.mailbox.push(envelope)?;
        let wake = state.enqueue(pid);
        self.shared.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.shared.changed);
        Ok(request)
    }

    pub fn snapshot(&self, pid: SchedulerPid) -> Result<SchedulerSnapshot, String> {
        let state = self.shared.lock();
        state.open()?;
        let actor = state.actor(self.shared.identity, pid)?;
        let execution = if actor.owner.is_some() {
            actor.mailbox.live_execution().or(actor.execution)
        } else {
            actor.execution
        };
        Ok(SchedulerSnapshot {
            owner: actor.owner,
            last_worker: actor.last_worker,
            migrations: actor.migrations,
            queued: actor.mailbox.count(),
            stopping: actor.stopping,
            execution,
            committed_state: actor.committed_state,
            resources: actor.account.used(),
        })
    }

    pub fn last_exit(&self) -> Option<SchedulerExitSnapshot> {
        self.shared.lock().last_exit
    }

    pub fn byte_state_snapshot(
        &self,
        pid: SchedulerPid,
    ) -> Result<Option<crate::ByteStateSnapshot>, String> {
        let state = self.shared.lock();
        state.open()?;
        let actor = state.actor(self.shared.identity, pid)?;
        if actor.owner.is_some() {
            return Err("byte state snapshot requires an idle actor".into());
        }
        Ok(actor
            .image
            .as_ref()
            .and_then(DetachedProcess::byte_state_snapshot))
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn fast_slot_contention(&self) -> Option<&locks::ContentionCounters> {
        self.contention_observation
            .as_deref()
            .map(|observation| &observation.fast_slot)
    }

    pub(super) fn publish_signal(&self, state: &mut State) {
        let bits = state.signal_bits();
        if bits != state.published_signals {
            state.published_signals = bits;
            self.ready_mask.store(bits, Ordering::Release);
        }
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        self.closed.store(true, Ordering::Release);
        for index in 0..self.fast_slots.len() {
            self.fast_slots[index].clear(self.fast_slot_contention());
        }
        for actor in state.slots.iter().filter_map(|slot| slot.actor.as_ref()) {
            actor.mailbox.mark_stopping();
            actor.cancellation.cancel();
        }
        let wake = state.wake_all_parked();
        self.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.changed);
    }
}

/// Lock-free effect context for a claimed actor: `EFFECT_SELF_PID`,
/// `EFFECT_SEND_SELF`, and `EFFECT_SEND` resolve through the fast slot table
/// and the target mailbox's atomics instead of suspending to the global
/// `Mutex<State>` effect path. Every returned status is identical to what
/// `Shared::effect` would produce; requests for other effects return `None`
/// and suspend as before.
#[derive(Clone)]
pub(crate) struct EffectSendTarget {
    shared: Arc<Shared>,
    mailbox: SharedMailbox,
    pid: SchedulerPid,
    capability: i64,
}

impl EffectSendTarget {
    pub(crate) fn new(
        shared: &Arc<Shared>,
        mailbox: &SharedMailbox,
        pid: SchedulerPid,
        capability: i64,
    ) -> Self {
        Self {
            shared: shared.clone(),
            mailbox: mailbox.clone(),
            pid,
            capability,
        }
    }

    /// Returns `Some(status)` when the effect completed inline, or `None` to
    /// fall back to the suspended scheduler path.
    pub(crate) unsafe fn try_effect(
        &self,
        context: &mut wings_abi::ScalarContext,
        request: &wings_abi::EffectRequest,
        output: *mut i64,
        budget: u64,
    ) -> Option<u32> {
        let reject = || {
            if self.shared.closed.load(Ordering::Acquire) || self.mailbox.is_retired() {
                wings_abi::STATUS_CANCELLED
            } else {
                wings_abi::STATUS_SEND_REJECTED
            }
        };
        match request.operation {
            wings_abi::EFFECT_SELF_PID => {
                if request.count != 0
                    || request.list_mask != 0
                    || request.bytes_mask != 0
                    || request.string_mask != 0
                {
                    // Mirrors the slow path: a `self_pid` carrying arguments is
                    // not a send, so it reports the generic control status.
                    return Some(wings_abi::STATUS_INVALID_CONTROL);
                }
                if self.mailbox.is_retired() || self.shared.closed.load(Ordering::Acquire) {
                    return Some(wings_abi::STATUS_CANCELLED);
                }
                if self.mailbox.is_stopping() {
                    return Some(wings_abi::STATUS_INVALID_CONTROL);
                }
                unsafe { output.write(self.capability) };
                Some(wings_abi::STATUS_OK)
            }
            wings_abi::EFFECT_SEND_SELF | wings_abi::EFFECT_SEND => {
                if self.mailbox.is_retired()
                    || self.mailbox.is_stopping()
                    || self.shared.closed.load(Ordering::Acquire)
                {
                    return Some(reject());
                }
                let (target_pid, target) = if request.operation == wings_abi::EFFECT_SEND_SELF {
                    (self.pid, self.mailbox.clone())
                } else {
                    let Some((slot, generation)) =
                        wings_abi::decode_capability(self.shared.identity, request.target)
                    else {
                        return Some(reject());
                    };
                    let pid = SchedulerPid {
                        scheduler: self.shared.identity,
                        slot: slot as u32,
                        generation,
                    };
                    let Some(mailbox) = self.shared.fast_slots.get(slot).and_then(|entry| {
                        entry.get(generation, self.shared.fast_slot_contention())
                    }) else {
                        return Some(reject());
                    };
                    (pid, mailbox)
                };
                let (arguments, lists, mut bytes) =
                    match unsafe { crate::execution::copy_effect_payload(context, request) } {
                        Ok(payload) => payload,
                        Err(status) => return Some(status),
                    };
                let handler = match target
                    .inner
                    .module
                    .handler_by_index(request.handler as usize)
                {
                    Ok(handler) => handler,
                    Err(_) => return Some(reject()),
                };
                let schema = handler.schema();
                if handler.fast_meta().is_completion()
                    || schema.reply.is_some()
                    || crate::message_values::validate_message_payload(
                        &schema.parameters,
                        arguments.as_slice(),
                        false,
                        true,
                        &lists,
                        &bytes,
                    )
                    .is_err()
                    || target.is_stopping()
                    || target.is_retired()
                    || !Arc::ptr_eq(&target.inner.module.inner, &self.mailbox.inner.module.inner)
                    || target.occupancy_hint() >= target.capacity()
                {
                    return Some(reject());
                }
                if bytes.transfer_checked(&target.inner.account).is_err() {
                    return Some(reject());
                }
                let charge = match target.inner.account.reserve_envelope(
                    std::mem::size_of::<Envelope>() as u64
                        + MessageArguments::overflow_bytes(arguments.as_slice().len())
                        + lists.overflow_bytes(),
                ) {
                    Ok(charge) => charge,
                    Err(_) => return Some(reject()),
                };
                let fast_eligible = envelope_fast_eligible(&handler, &lists, &bytes);
                let message = Envelope {
                    handler: handler.index,
                    arguments,
                    max_steps: budget,
                    lists: box_list_payload(lists),
                    bytes: box_byte_payload(bytes),
                    reply: Reply::None,
                    fast_eligible,
                    _charge: charge,
                };
                match target.try_push_fast(&self.shared.closed, message) {
                    Ok(needs_enqueue) => {
                        if !needs_enqueue {
                            let mut state = self.shared.lock();
                            let wake = state.enqueue_fast_pushed(target_pid, &target);
                            if !state.closed {
                                self.shared.publish_signal(&mut state);
                                drop(state);
                                wake.fire(&self.shared.changed);
                            }
                        }
                        unsafe { output.write(0) };
                        Some(wings_abi::STATUS_OK)
                    }
                    Err(FastPushError::MailboxFull) => Some(reject()),
                    Err(FastPushError::ClosedOrStopped) => {
                        if self.shared.closed.load(Ordering::Acquire) {
                            Some(wings_abi::STATUS_CANCELLED)
                        } else {
                            Some(reject())
                        }
                    }
                }
            }
            _ => None,
        }
    }
}

fn next_identity(counter: &AtomicU64) -> Result<u64, String> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current >= i64::MAX as u64 {
            return Err("scheduler identity exhausted".into());
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(current),
            Err(value) => current = value,
        }
    }
}

fn storage<Value>(
    capacity: usize,
    mut initialize: impl FnMut() -> Value,
) -> Result<Box<[Value]>, String> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|error| error.to_string())?;
    if values.capacity() != capacity {
        return Err("allocator changed scheduler fixed capacity".into());
    }
    values.resize_with(capacity, &mut initialize);
    Ok(values.into_boxed_slice())
}

enum FastPushError {
    MailboxFull,
    ClosedOrStopped,
}

const FRONT_HINT_FAST: u64 = 1 << 63;
const FRONT_HINT_HANDLER_MAX: usize = (1 << 31) - 1;
const FRONT_HINT_HANDLER_MASK: u64 = (1 << 31) - 1;

struct MailboxGuard<'mb>(&'mb SharedMailboxInner);

impl std::ops::Deref for MailboxGuard<'_> {
    type Target = Mailbox;
    fn deref(&self) -> &Mailbox {
        unsafe { &*self.0.state.get() }
    }
}

impl std::ops::DerefMut for MailboxGuard<'_> {
    fn deref_mut(&mut self) -> &mut Mailbox {
        unsafe { &mut *self.0.state.get() }
    }
}

impl Drop for MailboxGuard<'_> {
    fn drop(&mut self) {
        let state = unsafe { &*self.0.state.get() };
        if self.0.count_atomic.load(Ordering::Relaxed) != state.count {
            self.0.count_atomic.store(state.count, Ordering::Release);
        }
        self.0.locked.store(false, Ordering::Release);
    }
}

impl SharedMailbox {
    fn new(
        module: ScalarModule,
        account: ResourceAccount,
        slots: Box<[Option<Envelope>]>,
        contention_observation: Option<Arc<locks::LockContentionObservation>>,
    ) -> Self {
        // The ring has one spare slot over the advertised capacity; admission
        // checks compare `count + in_flight` against `slots.len()`.
        let capacity = slots.len() - 1;
        Self {
            inner: Arc::new(SharedMailboxInner {
                module,
                account,
                capacity,
                count_atomic: AtomicUsize::new(0),
                front_hint: AtomicU64::new(0),
                in_flight: AtomicUsize::new(0),
                stopping: AtomicBool::new(false),
                active_or_queued: AtomicBool::new(false),
                retired: AtomicBool::new(false),
                locked: AtomicBool::new(false),
                live_execution_locked: AtomicBool::new(false),
                live_execution: UnsafeCell::new(None),
                contention_observation,
                state: UnsafeCell::new(Mailbox {
                    slots,
                    head: 0,
                    count: 0,
                }),
            }),
        }
    }

    pub(super) fn set_live_execution(&self, snapshot: Option<ExecutionSnapshot>) {
        locks::spin_lock(&self.inner.live_execution_locked, None);
        unsafe {
            *self.inner.live_execution.get() = snapshot;
        }
        self.inner
            .live_execution_locked
            .store(false, Ordering::Release);
    }

    pub(super) fn live_execution(&self) -> Option<ExecutionSnapshot> {
        locks::spin_lock(&self.inner.live_execution_locked, None);
        let snapshot = unsafe { *self.inner.live_execution.get() };
        self.inner
            .live_execution_locked
            .store(false, Ordering::Release);
        snapshot
    }

    fn lock(&self) -> MailboxGuard<'_> {
        let observation = self
            .inner
            .contention_observation
            .as_deref()
            .map(|observation| &observation.mailbox);
        locks::spin_lock(&self.inner.locked, observation);
        MailboxGuard(&self.inner)
    }

    fn publish_front_hint(&self, mailbox: &Mailbox) {
        let message = if mailbox.count == 0 {
            None
        } else {
            mailbox.slots[mailbox.head].as_ref()
        };
        let hint = match message {
            // `try_pop_batch_fast` re-reads eligibility under the lock, so a
            // conservative `0` (not fast) for oversized handler indices is fine.
            Some(message) if message.fast_eligible && message.handler <= FRONT_HINT_HANDLER_MAX => {
                FRONT_HINT_FAST
                    | (message.max_steps.min(u32::MAX as u64) << 31)
                    | message.handler as u64
            }
            _ => 0,
        };
        // Homogeneous queues republish the same hint on every pop; skipping
        // the redundant store keeps this line from bouncing while senders and
        // batch pops read it each message.
        if self.inner.front_hint.load(Ordering::Relaxed) != hint {
            self.inner.front_hint.store(hint, Ordering::Release);
        }
    }

    pub(super) fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub(super) fn count(&self) -> usize {
        self.inner.count_atomic.load(Ordering::Acquire)
    }

    pub(super) fn occupancy_hint(&self) -> usize {
        self.count() + self.inner.in_flight.load(Ordering::Acquire)
    }

    pub(super) fn is_retired(&self) -> bool {
        self.inner.retired.load(Ordering::Acquire)
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
    }

    pub(super) fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::Acquire)
    }

    /// Releases one `in_flight` slot reservation. Saturates so a fused claim
    /// that already released at prepare time can call this unconditionally.
    pub(super) fn release_in_flight(&self) {
        let mut current = self.inner.in_flight.load(Ordering::Relaxed);
        loop {
            if current == 0 {
                break;
            }
            match self.inner.in_flight.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    pub(super) fn mark_started(&self) {
        self.release_in_flight();
    }

    pub(super) fn mark_stopping(&self) {
        self.inner.stopping.store(true, Ordering::Release);
    }

    pub(super) fn retire_clear(&self) {
        self.inner.retired.store(true, Ordering::Release);
        let mut drained = Vec::new();
        {
            let mut guard = self.lock();
            self.inner.stopping.store(true, Ordering::Relaxed);
            self.inner.in_flight.store(0, Ordering::Relaxed);
            self.inner.active_or_queued.store(false, Ordering::Relaxed);
            if guard.count > 0 {
                let _ = drained.try_reserve_exact(guard.count);
                while guard.count > 0 {
                    let head = guard.head;
                    let msg = guard.slots[head].take().expect("queued envelope");
                    guard.head = (head + 1) % guard.slots.len();
                    guard.count -= 1;
                    if drained.capacity() > drained.len() {
                        drained.push(msg);
                    }
                }
            }
            self.publish_front_hint(&guard);
        }
        drop(drained);
    }

    pub(super) fn set_active_or_queued(&self, active: bool) {
        self.inner.active_or_queued.store(active, Ordering::Release);
    }

    pub(super) fn mark_enqueued_if_nonempty(&self) -> bool {
        if !self.inner.stopping.load(Ordering::Acquire) && self.count() > 0 {
            self.inner.active_or_queued.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub(super) fn keep_active_if_nonempty(&self) -> bool {
        // Unlike `mark_enqueued_if_nonempty` (whose callers just pushed under
        // the same global lock, so their count is already published), this can
        // race a fast push that committed its enqueue but has not published
        // `count_atomic` yet — take the lock so the emptiness check is fresh.
        let guard = self.lock();
        let keep = !self.inner.stopping.load(Ordering::Acquire) && guard.count > 0;
        self.inner.active_or_queued.store(keep, Ordering::Release);
        keep
    }

    fn try_push_fast(&self, closed: &AtomicBool, message: Envelope) -> Result<bool, FastPushError> {
        let mut guard = self.lock();
        if closed.load(Ordering::Acquire)
            || self.inner.stopping.load(Ordering::Relaxed)
            || self.is_retired()
        {
            return Err(FastPushError::ClosedOrStopped);
        }
        if guard.count + self.inner.in_flight.load(Ordering::Relaxed) >= guard.slots.len() {
            return Err(FastPushError::MailboxFull);
        }
        let was_empty = guard.count == 0;
        let tail = (guard.head + guard.count) % guard.slots.len();
        guard.slots[tail] = Some(message);
        guard.count += 1;
        if was_empty {
            self.publish_front_hint(&guard);
        }
        // Under sustained load the flag stays set; a plain load avoids an
        // RMW on this contended line for every hot push. The swap below still
        // publishes for the (rare) clear-and-set transition.
        if self.inner.active_or_queued.load(Ordering::Acquire) {
            return Ok(true);
        }
        Ok(self.inner.active_or_queued.swap(true, Ordering::AcqRel))
    }

    pub(super) fn push(&self, message: Envelope) -> Result<(), String> {
        self.push_admitted(message)
            .map_or_else(|| Ok(()), |_| Err("mailbox_full".into()))
    }

    pub(super) fn push_checked(&self, message: Envelope) -> Result<(), crate::VmError> {
        self.push_admitted(message)
            .map_or_else(|| Ok(()), |_| Err(crate::VmError::MailboxFull))
    }

    /// Atomically admits and enqueues one envelope; returns it back when the
    /// mailbox has no free (unreserved) slot left.
    pub(super) fn push_admitted(&self, message: Envelope) -> Option<Envelope> {
        let mut guard = self.lock();
        if guard.count + self.inner.in_flight.load(Ordering::Relaxed) >= guard.slots.len() {
            return Some(message);
        }
        let was_empty = guard.count == 0;
        let tail = (guard.head + guard.count) % guard.slots.len();
        guard.slots[tail] = Some(message);
        guard.count += 1;
        if was_empty {
            self.publish_front_hint(&guard);
        }
        None
    }

    /// Pops the head envelope for a claim and keeps one slot reserved as
    /// `in_flight` until `mark_started` or `restore` releases it.
    pub(super) fn pop_for_claim(&self) -> Envelope {
        let mut guard = self.lock();
        let head = guard.head;
        let message = guard.slots[head].take().expect("ready mailbox");
        guard.head = (head + 1) % guard.slots.len();
        guard.count -= 1;
        self.inner.in_flight.fetch_add(1, Ordering::Relaxed);
        self.publish_front_hint(&guard);
        message
    }

    #[cfg(test)]
    pub(super) fn pop(&self) -> Envelope {
        let mut guard = self.lock();
        let head = guard.head;
        let message = guard.slots[head].take().expect("ready mailbox");
        guard.head = (head + 1) % guard.slots.len();
        guard.count -= 1;
        self.publish_front_hint(&guard);
        message
    }

    pub(super) fn front_is_fast(&self) -> bool {
        self.inner.front_hint.load(Ordering::Acquire) & FRONT_HINT_FAST != 0
    }

    pub(super) fn front_can_run_direct(
        &self,
        image: &DetachedProcess,
        fibers: &crate::execution::WorkerFibers,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        let hint = self.inner.front_hint.load(Ordering::Acquire);
        if hint & FRONT_HINT_FAST == 0 {
            return false;
        }
        fibers.can_run_direct_image_index(
            &self.inner.module,
            image,
            (hint & FRONT_HINT_HANDLER_MASK) as usize,
            (hint >> 31) & u32::MAX as u64,
            slice_steps,
            yield_observed,
        )
    }

    pub(super) fn try_pop_batch_fast(&self, threshold: usize) -> Option<Envelope> {
        if self.count() < threshold || !self.front_is_fast() {
            return None;
        }
        let mut guard = self.lock();
        if self.inner.stopping.load(Ordering::Relaxed) || guard.count < threshold {
            return None;
        }
        let is_fast = guard.slots[guard.head]
            .as_ref()
            .is_some_and(|message| message.fast_eligible);
        if !is_fast {
            return None;
        }
        let head = guard.head;
        let message = guard.slots[head].take().expect("batch mailbox");
        guard.head = (head + 1) % guard.slots.len();
        guard.count -= 1;
        self.inner.in_flight.fetch_add(1, Ordering::Relaxed);
        self.publish_front_hint(&guard);
        Some(message)
    }

    /// Reinserts a popped message at the head and releases its `in_flight`
    /// slot reservation when one is still held; the spare ring slot keeps the
    /// write position free regardless.
    pub(super) fn restore(&self, message: Envelope) {
        let mut guard = self.lock();
        let head = (guard.head + guard.slots.len() - 1) % guard.slots.len();
        guard.head = head;
        debug_assert!(guard.slots[head].is_none());
        guard.slots[head] = Some(message);
        guard.count += 1;
        let mut current = self.inner.in_flight.load(Ordering::Relaxed);
        while current != 0 {
            match self.inner.in_flight.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.publish_front_hint(&guard);
    }

    #[cfg(test)]
    pub(super) fn with_slot0_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let guard = self.lock();
        f(&guard.slots[0]
            .as_ref()
            .unwrap()
            .bytes
            .as_deref()
            .unwrap_or(BytePayload::empty_ref())
            .data)
    }
}

fn validate_scalar_module(
    module: &ScalarModule,
    stopping: bool,
    handler: &HandlerId,
    arguments: &[i64],
) -> Result<(), String> {
    if stopping || !Arc::ptr_eq(&module.inner, &handler.module.inner) {
        return Err("scheduler handler identity, phase or arguments mismatch".into());
    }
    let meta = handler.fast_meta();
    if meta.is_completion() {
        return Err("scheduler handler identity, phase or arguments mismatch".into());
    }
    if meta.all_plain_int() && arguments.len() == usize::from(meta.param_count) {
        return Ok(());
    }
    crate::message_values::validate_scalar_arguments(&handler.schema().parameters, arguments)
        .map_err(|err| format!("scheduler {err}"))
}

fn validate_scalar(actor: &Actor, handler: &HandlerId, arguments: &[i64]) -> Result<(), String> {
    validate_scalar_module(&actor.module, actor.stopping, handler, arguments)
}

fn validate(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    system: bool,
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), String> {
    validate_checked(actor, handler, arguments, system, lists, bytes).map_err(|err| match err {
        crate::VmError::InvalidArguments(msg) => format!("scheduler {msg}"),
        other => String::from(other),
    })
}

fn validate_checked(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    system: bool,
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), crate::VmError> {
    let schema = handler.schema();
    if actor.stopping
        || !Arc::ptr_eq(&actor.module.inner, &handler.module.inner)
        || schema.is_completion() != system
    {
        return Err(crate::VmError::Other(
            "scheduler handler identity, phase or arguments mismatch",
        ));
    }
    crate::message_values::validate_message_payload(
        &schema.parameters,
        arguments,
        system,
        true,
        lists,
        bytes,
    )
    .map_err(crate::VmError::InvalidArguments)
}

fn envelope_precopied(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    reply: Reply,
) -> Result<Envelope, String> {
    if actor.mailbox.count() + actor.mailbox.in_flight() >= actor.mailbox.capacity() {
        return Err(crate::VmError::MailboxFull.into());
    }
    let charge = actor.account.reserve_envelope(
        std::mem::size_of::<Envelope>() as u64 + MessageArguments::overflow_bytes(arguments.len()),
    )?;
    let arguments = MessageArguments::try_from_slice(arguments).map_err(String::from)?;
    let fast_eligible = handler.fast_meta().fast_scalar();
    Ok(Envelope {
        handler: handler.index,
        arguments,
        max_steps,
        lists: None,
        bytes: None,
        reply,
        fast_eligible,
        _charge: charge,
    })
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    lists: ListPayload,
    bytes: BytePayload,
    system: bool,
    reply: Reply,
) -> Result<Envelope, String> {
    envelope_checked(
        actor, handler, arguments, max_steps, lists, bytes, system, reply,
    )
    .map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
fn envelope_checked(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    lists: ListPayload,
    bytes: BytePayload,
    _system: bool,
    reply: Reply,
) -> Result<Envelope, crate::VmError> {
    if actor.mailbox.count() + actor.mailbox.in_flight() >= actor.mailbox.capacity() {
        return Err(crate::VmError::MailboxFull);
    }
    delayed_envelope_checked(actor, handler, arguments, max_steps, lists, bytes, reply)
}

fn delayed_envelope_checked(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    lists: ListPayload,
    mut bytes: BytePayload,
    reply: Reply,
) -> Result<Envelope, crate::VmError> {
    bytes.transfer_checked(&actor.account)?;
    let charge = actor.account.reserve_envelope(
        std::mem::size_of::<Envelope>() as u64
            + MessageArguments::overflow_bytes(arguments.len())
            + lists.overflow_bytes(),
    )?;
    let arguments = MessageArguments::try_from_slice(arguments).map_err(crate::VmError::Other)?;
    let fast_eligible = envelope_fast_eligible(handler, &lists, &bytes);
    Ok(Envelope {
        handler: handler.index,
        arguments,
        max_steps,
        lists: box_list_payload(lists),
        bytes: box_byte_payload(bytes),
        reply,
        fast_eligible,
        _charge: charge,
    })
}
