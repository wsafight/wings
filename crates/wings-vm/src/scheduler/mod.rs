use std::cell::UnsafeCell;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::host_values::HostMessage;
use crate::ingress_requests::{ReplyPublisher, ReplyTable};
use crate::message_values::{BytePayload, ListPayload};
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
pub use locks::{SchedulerLockMetrics, SchedulerLockStatistics};
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
const THREAD_STACK: usize = 2 << 20;
static NEXT_SCHEDULER: AtomicU64 = AtomicU64::new(1);
static NEXT_CAPABILITY: AtomicU64 = AtomicU64::new(1);

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

struct Shared {
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
    _charge: Reservation,
}

struct State {
    closed: bool,
    ready_bits: u32,
    last_exit: Option<SchedulerExitSnapshot>,
    slots: Box<[Slot]>,
    requests: state::RequestTable,
    ready: Box<[ready::ReadyQueue]>,
    free: Option<usize>,
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
    starting: bool,
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

struct FastSlot {
    locked: AtomicBool,
    entry: UnsafeCell<Option<(u32, Weak<SharedMailboxInner>)>>,
}

unsafe impl Send for FastSlot {}
unsafe impl Sync for FastSlot {}

impl Default for FastSlot {
    fn default() -> Self {
        Self {
            locked: AtomicBool::new(false),
            entry: UnsafeCell::new(None),
        }
    }
}

impl FastSlot {
    fn lock(&self) -> FastSlotGuard<'_> {
        let mut spins = 0u32;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if spins < 16 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                std::thread::yield_now();
            }
        }
        FastSlotGuard(self)
    }

    fn set(&self, generation: u32, mailbox: &SharedMailbox) {
        let mut guard = self.lock();
        *guard = Some((generation, Arc::downgrade(&mailbox.inner)));
    }

    fn clear(&self) {
        let current = {
            let mut guard = self.lock();
            guard.take()
        };
        drop(current);
    }

    fn get(&self, generation: u32) -> Option<SharedMailbox> {
        let guard = self.lock();
        match guard.as_ref() {
            Some((slot_gen, weak)) if *slot_gen == generation => weak
                .upgrade()
                .map(|inner| SharedMailbox { inner })
                .filter(|mailbox| !mailbox.is_retired()),
            _ => None,
        }
    }
}

struct FastSlotGuard<'slot>(&'slot FastSlot);

impl std::ops::Deref for FastSlotGuard<'_> {
    type Target = Option<(u32, Weak<SharedMailboxInner>)>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.entry.get() }
    }
}

impl std::ops::DerefMut for FastSlotGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.0.entry.get() }
    }
}

impl Drop for FastSlotGuard<'_> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
struct SharedMailbox {
    inner: Arc<SharedMailboxInner>,
}

struct SharedMailboxInner {
    module: ScalarModule,
    account: ResourceAccount,
    capacity: usize,
    count_atomic: AtomicUsize,
    occupancy_atomic: AtomicUsize,
    retired: AtomicBool,
    locked: AtomicBool,
    state: UnsafeCell<Mailbox>,
}

unsafe impl Send for SharedMailboxInner {}
unsafe impl Sync for SharedMailboxInner {}

struct Mailbox {
    slots: Box<[Option<Envelope>]>,
    head: usize,
    count: usize,
    starting: bool,
    stopping: bool,
    active_or_queued: bool,
}

struct Envelope {
    handler: HandlerId,
    arguments: [i64; 64],
    count: usize,
    max_steps: u64,
    record_mask: u64,
    lists: ListPayload,
    bytes: BytePayload,
    reply: Reply,
    _charge: Reservation,
    _message_charge: Option<Reservation>,
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
                last_exit: None,
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
            self.handle.shared.fast_slots[index].clear();
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
        state.requests.clear_all();
    }
}

impl SchedulerHandle {
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
                    + mailbox * std::mem::size_of::<Option<Envelope>>())
                    as u64,
                ..Resources::default()
            })?;
            let slots = storage(mailbox, || None)?;
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
            let shared_mailbox = SharedMailbox::new(module.clone(), account.clone(), slots);
            Ok::<_, String>(Actor {
                module: module.clone(),
                account,
                image: Some(image),
                owner: None,
                starting: false,
                last_worker: None,
                migrations: 0,
                execution: None,
                committed_state,
                stopping: false,
                cancellation,
                capability: next_identity(&NEXT_CAPABILITY)? as i64,
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
                self.shared.fast_slots[pid.slot as usize].set(pid.generation, &actor.mailbox);
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
        if !self.shared.closed.load(Ordering::Acquire)
            && self.shared.identity == pid.scheduler
            && let Some(fast_slot) = self.shared.fast_slots.get(pid.slot as usize)
            && let Some(mailbox) = fast_slot.get(pid.generation)
        {
            validate_scalar_module(&mailbox.inner.module, false, handler, arguments)?;
            if handler.schema().reply.is_some() {
                return Err("send requires a one-way handler".into());
            }
            if mailbox.occupancy_hint() < mailbox.capacity() {
                let mut owned = [0; 64];
                owned[..arguments.len()].copy_from_slice(arguments);
                let charge = mailbox.inner.account.reserve(Resources {
                    bytes: std::mem::size_of::<Envelope>() as u64,
                    ..Resources::default()
                })?;
                let message_charge = mailbox.inner.account.reserve(Resources {
                    messages: 1,
                    ..Resources::default()
                })?;
                let message = Envelope {
                    handler: handler.clone(),
                    arguments: owned,
                    count: arguments.len(),
                    max_steps,
                    record_mask: 0,
                    lists: ListPayload::default(),
                    bytes: BytePayload::default(),
                    reply: Reply::None,
                    _charge: charge,
                    _message_charge: Some(message_charge),
                };
                match mailbox.try_push_fast(&self.shared.closed, message) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {
                        let mut state = self.shared.lock();
                        if !state.closed {
                            let wake = state.enqueue(pid);
                            self.shared.publish_signal(&state);
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
        let mut owned = [0; 64];
        if arguments.len() <= 64 {
            owned[..arguments.len()].copy_from_slice(arguments);
        }
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate_scalar(actor, handler, arguments)?;
        if handler.schema().reply.is_some() {
            return Err("send requires a one-way handler".into());
        }
        let message = envelope_precopied(
            actor,
            handler,
            owned,
            arguments.len(),
            max_steps,
            Reply::None,
        )?;
        actor.mailbox.push(message)?;
        let wake = state.enqueue(pid);
        self.shared.publish_signal(&state);
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
        if !self.shared.closed.load(Ordering::Acquire)
            && self.shared.identity == pid.scheduler
            && let Some(fast_slot) = self.shared.fast_slots.get(pid.slot as usize)
            && let Some(mailbox) = fast_slot.get(pid.generation)
        {
            validate_scalar_module(&mailbox.inner.module, false, handler, arguments)?;
            let reply = match handler.schema().reply {
                Some(
                    ty @ (wings_abi::ValueType::Int
                    | wings_abi::ValueType::Bool
                    | wings_abi::ValueType::Unit
                    | wings_abi::ValueType::Ref
                    | wings_abi::ValueType::ListRef
                    | wings_abi::ValueType::BytesRef
                    | wings_abi::ValueType::StringRef),
                ) => ty,
                _ => {
                    return Err(
                        "request requires a scalar, RecordInt, IntList, Bytes or String reply"
                            .into(),
                    );
                }
            };
            if mailbox.occupancy_hint() < mailbox.capacity() {
                let mut owned = [0; 64];
                owned[..arguments.len()].copy_from_slice(arguments);
                let charge = mailbox.inner.account.reserve(Resources {
                    bytes: std::mem::size_of::<Envelope>() as u64,
                    ..Resources::default()
                })?;
                let message_charge = mailbox.inner.account.reserve(Resources {
                    messages: 1,
                    ..Resources::default()
                })?;
                let (request, publisher) =
                    self.shared
                        .replies
                        .start(deadline, &mailbox.inner.account, reply)?;
                let message = Envelope {
                    handler: handler.clone(),
                    arguments: owned,
                    count: arguments.len(),
                    max_steps,
                    record_mask: 0,
                    lists: ListPayload::default(),
                    bytes: BytePayload::default(),
                    reply: Reply::External(publisher),
                    _charge: charge,
                    _message_charge: Some(message_charge),
                };
                match mailbox.try_push_fast(&self.shared.closed, message) {
                    Ok(true) => return Ok(request),
                    Ok(false) => {
                        let mut state = self.shared.lock();
                        if !state.closed {
                            let wake = state.enqueue(pid);
                            self.shared.publish_signal(&state);
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
        let mut owned = [0; 64];
        if arguments.len() <= 64 {
            owned[..arguments.len()].copy_from_slice(arguments);
        }
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate_scalar(actor, handler, arguments)?;
        let reply = match handler.schema().reply {
            Some(
                ty @ (wings_abi::ValueType::Int
                | wings_abi::ValueType::Bool
                | wings_abi::ValueType::Unit
                | wings_abi::ValueType::Ref
                | wings_abi::ValueType::ListRef
                | wings_abi::ValueType::BytesRef
                | wings_abi::ValueType::StringRef),
            ) => ty,
            _ => {
                return Err(
                    "request requires a scalar, RecordInt, IntList, Bytes or String reply".into(),
                );
            }
        };
        let mut message = envelope_precopied(
            actor,
            handler,
            owned,
            arguments.len(),
            max_steps,
            Reply::None,
        )?;
        let (request, publisher) = self.shared.replies.start(deadline, &actor.account, reply)?;
        message.reply = Reply::External(publisher);
        actor.mailbox.push(message)?;
        let wake = state.enqueue(pid);
        self.shared.publish_signal(&state);
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
        self.shared.fast_slots[pid.slot as usize].clear();
        let retired = if actor.owner.is_none() {
            state.retire(self.shared.identity, pid)
        } else {
            None
        };
        let wake = state.wake_all_parked();
        self.shared.publish_signal(&state);
        drop(state);
        if let Some(actor_ref) = retired.as_ref() {
            actor_ref.mailbox.retire_clear();
        }
        drop(retired);
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
            if actor.mailbox.count() + usize::from(actor.starting || actor.mailbox.is_starting())
                == actor.mailbox.capacity()
            {
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
            message.record_mask,
            &message.lists,
            &message.bytes,
        )?;
        let mut envelope = envelope(
            actor,
            handler,
            arguments,
            max_steps,
            message.record_mask,
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
        self.shared.publish_signal(&state);
        drop(state);
        wake.fire(&self.shared.changed);
        Ok(request)
    }

    pub fn snapshot(&self, pid: SchedulerPid) -> Result<SchedulerSnapshot, String> {
        let state = self.shared.lock();
        state.open()?;
        let actor = state.actor(self.shared.identity, pid)?;
        Ok(SchedulerSnapshot {
            owner: actor.owner,
            last_worker: actor.last_worker,
            migrations: actor.migrations,
            queued: actor.mailbox.count(),
            stopping: actor.stopping,
            execution: actor.execution,
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

    pub(super) fn publish_signal(&self, state: &State) {
        self.ready_mask
            .store(state.signal_bits(), Ordering::Release);
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        self.closed.store(true, Ordering::Release);
        for index in 0..self.fast_slots.len() {
            self.fast_slots[index].clear();
        }
        for actor in state.slots.iter().filter_map(|slot| slot.actor.as_ref()) {
            actor.mailbox.mark_stopping();
            actor.cancellation.cancel();
        }
        let wake = state.wake_all_parked();
        self.publish_signal(&state);
        drop(state);
        wake.fire(&self.changed);
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
        self.0.count_atomic.store(state.count, Ordering::Release);
        self.0
            .occupancy_atomic
            .store(state.count + usize::from(state.starting), Ordering::Release);
        self.0.locked.store(false, Ordering::Release);
    }
}

impl SharedMailbox {
    fn new(module: ScalarModule, account: ResourceAccount, slots: Box<[Option<Envelope>]>) -> Self {
        let capacity = slots.len();
        Self {
            inner: Arc::new(SharedMailboxInner {
                module,
                account,
                capacity,
                count_atomic: AtomicUsize::new(0),
                occupancy_atomic: AtomicUsize::new(0),
                retired: AtomicBool::new(false),
                locked: AtomicBool::new(false),
                state: UnsafeCell::new(Mailbox {
                    slots,
                    head: 0,
                    count: 0,
                    starting: false,
                    stopping: false,
                    active_or_queued: false,
                }),
            }),
        }
    }

    fn lock(&self) -> MailboxGuard<'_> {
        let mut spins = 0u32;
        while self
            .inner
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if spins < 16 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                std::thread::yield_now();
            }
        }
        MailboxGuard(&self.inner)
    }

    pub(super) fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub(super) fn count(&self) -> usize {
        self.inner.count_atomic.load(Ordering::Acquire)
    }

    pub(super) fn occupancy_hint(&self) -> usize {
        self.inner.occupancy_atomic.load(Ordering::Acquire)
    }

    pub(super) fn is_retired(&self) -> bool {
        self.inner.retired.load(Ordering::Acquire)
    }

    pub(super) fn is_starting(&self) -> bool {
        self.lock().starting
    }

    pub(super) fn set_starting(&self, starting: bool) {
        self.lock().starting = starting;
    }

    pub(super) fn mark_stopping(&self) {
        self.lock().stopping = true;
    }

    pub(super) fn retire_clear(&self) {
        self.inner.retired.store(true, Ordering::Release);
        let mut drained = Vec::new();
        {
            let mut guard = self.lock();
            guard.stopping = true;
            guard.starting = false;
            guard.active_or_queued = false;
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
        }
        drop(drained);
    }

    pub(super) fn set_active_or_queued(&self, active: bool) {
        self.lock().active_or_queued = active;
    }

    pub(super) fn mark_enqueued_if_nonempty(&self) -> bool {
        let mut guard = self.lock();
        if !guard.stopping && guard.count > 0 {
            guard.active_or_queued = true;
            true
        } else {
            false
        }
    }

    pub(super) fn keep_active_if_nonempty(&self) -> bool {
        let mut guard = self.lock();
        if !guard.stopping && guard.count > 0 {
            guard.active_or_queued = true;
            true
        } else {
            guard.active_or_queued = false;
            false
        }
    }

    fn try_push_fast(&self, closed: &AtomicBool, message: Envelope) -> Result<bool, FastPushError> {
        let mut guard = self.lock();
        if closed.load(Ordering::Acquire) || guard.stopping || self.is_retired() {
            return Err(FastPushError::ClosedOrStopped);
        }
        if guard.count + usize::from(guard.starting) == guard.slots.len() {
            return Err(FastPushError::MailboxFull);
        }
        let tail = (guard.head + guard.count) % guard.slots.len();
        guard.slots[tail] = Some(message);
        guard.count += 1;
        Ok(guard.active_or_queued)
    }

    pub(super) fn push(&self, message: Envelope) -> Result<(), String> {
        let mut guard = self.lock();
        if guard.count == guard.slots.len() {
            return Err("mailbox_full".into());
        }
        let tail = (guard.head + guard.count) % guard.slots.len();
        guard.slots[tail] = Some(message);
        guard.count += 1;
        Ok(())
    }

    pub(super) fn pop(&self) -> Envelope {
        let mut guard = self.lock();
        let head = guard.head;
        let message = guard.slots[head].take().expect("ready mailbox");
        guard.head = (head + 1) % guard.slots.len();
        guard.count -= 1;
        message
    }

    pub(super) fn front_is_fast(&self) -> bool {
        let guard = self.lock();
        guard.count > 0
            && guard.slots[guard.head].as_ref().is_some_and(|message| {
                crate::execution::WorkerFibers::can_run_fast(
                    &message.handler,
                    message.record_mask,
                    &message.lists,
                    &message.bytes,
                )
            })
    }

    pub(super) fn front_can_run_direct(
        &self,
        image: &DetachedProcess,
        fibers: &crate::execution::WorkerFibers,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        let guard = self.lock();
        guard.count > 0
            && guard.slots[guard.head].as_ref().is_some_and(|message| {
                crate::execution::WorkerFibers::can_run_fast(
                    &message.handler,
                    message.record_mask,
                    &message.lists,
                    &message.bytes,
                ) && fibers.can_run_direct_image(
                    image,
                    &message.handler,
                    message.max_steps,
                    slice_steps,
                    yield_observed,
                )
            })
    }

    pub(super) fn try_pop_batch_fast(&self, threshold: usize) -> Option<Envelope> {
        let mut guard = self.lock();
        if guard.stopping || guard.count < threshold {
            return None;
        }
        let is_fast = guard.slots[guard.head].as_ref().is_some_and(|message| {
            crate::execution::WorkerFibers::can_run_fast(
                &message.handler,
                message.record_mask,
                &message.lists,
                &message.bytes,
            )
        });
        if !is_fast {
            return None;
        }
        let head = guard.head;
        let message = guard.slots[head].take().expect("batch mailbox");
        guard.head = (head + 1) % guard.slots.len();
        guard.count -= 1;
        Some(message)
    }

    pub(super) fn restore(&self, message: Envelope) {
        let mut guard = self.lock();
        let head = (guard.head + guard.slots.len() - 1) % guard.slots.len();
        guard.head = head;
        guard.slots[head] = Some(message);
        guard.count += 1;
    }

    #[cfg(test)]
    pub(super) fn with_slot0_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let guard = self.lock();
        f(&guard.slots[0].as_ref().unwrap().bytes.data)
    }
}

fn validate_scalar_module(
    module: &ScalarModule,
    stopping: bool,
    handler: &HandlerId,
    arguments: &[i64],
) -> Result<(), String> {
    let schema = handler.schema();
    if stopping
        || !Arc::ptr_eq(&module.inner, &handler.module.inner)
        || schema.is_completion()
        || arguments.len() != schema.parameters.len()
        || arguments.len() > 64
    {
        return Err("scheduler handler identity, phase or arguments mismatch".into());
    }
    for (value, (_, ty)) in arguments.iter().zip(&schema.parameters) {
        if ty.is_reference() {
            return Err("scheduler reference payload mismatch".into());
        }
        if !ty.accepts(*value) {
            return Err("scheduler scalar argument mismatch".into());
        }
    }
    Ok(())
}

fn validate_scalar(actor: &Actor, handler: &HandlerId, arguments: &[i64]) -> Result<(), String> {
    validate_scalar_module(&actor.module, actor.stopping, handler, arguments)
}

fn validate(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    system: bool,
    record_mask: u64,
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), String> {
    let schema = handler.schema();
    if actor.stopping
        || !Arc::ptr_eq(&actor.module.inner, &handler.module.inner)
        || schema.is_completion() != system
        || arguments.len() != schema.parameters.len()
        || arguments.len() > 64
    {
        return Err("scheduler handler identity, phase or arguments mismatch".into());
    }
    let mut expected_record = 0;
    let mut expected_list = 0;
    let mut expected_bytes = 0;
    let mut expected_strings = 0;
    for (index, (value, (_, ty))) in arguments.iter().zip(&schema.parameters).enumerate() {
        match ty {
            wings_abi::ValueType::Ref => expected_record |= 1 << index,
            wings_abi::ValueType::ListRef => expected_list |= 1 << index,
            wings_abi::ValueType::BytesRef => expected_bytes |= 1 << index,
            wings_abi::ValueType::StringRef => {
                expected_bytes |= 1 << index;
                expected_strings |= 1 << index;
            }
            wings_abi::ValueType::RequestId if system && *value > 0 => {}
            _ if ty.accepts(*value) => {}
            _ => return Err("scheduler scalar argument mismatch".into()),
        }
    }
    if record_mask != expected_record
        || !lists.valid(arguments, expected_list)
        || !bytes.valid(arguments, expected_bytes)
        || bytes.string_mask != expected_strings
    {
        return Err("scheduler reference payload mismatch".into());
    }
    Ok(())
}

fn envelope_precopied(
    actor: &Actor,
    handler: &HandlerId,
    arguments: [i64; 64],
    count: usize,
    max_steps: u64,
    reply: Reply,
) -> Result<Envelope, String> {
    if actor.mailbox.count() + usize::from(actor.starting || actor.mailbox.is_starting())
        == actor.mailbox.capacity()
    {
        return Err("mailbox_full".into());
    }
    let charge = actor.account.reserve(Resources {
        bytes: std::mem::size_of::<Envelope>() as u64,
        ..Resources::default()
    })?;
    let message_charge = actor.account.reserve(Resources {
        messages: 1,
        ..Resources::default()
    })?;
    Ok(Envelope {
        handler: handler.clone(),
        arguments,
        count,
        max_steps,
        record_mask: 0,
        lists: ListPayload::default(),
        bytes: BytePayload::default(),
        reply,
        _charge: charge,
        _message_charge: Some(message_charge),
    })
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    record_mask: u64,
    lists: ListPayload,
    mut bytes: BytePayload,
    _system: bool,
    reply: Reply,
) -> Result<Envelope, String> {
    if actor.mailbox.count() + usize::from(actor.starting || actor.mailbox.is_starting())
        == actor.mailbox.capacity()
    {
        return Err("mailbox_full".into());
    }
    bytes.transfer(&actor.account)?;
    let charge = actor.account.reserve(Resources {
        bytes: std::mem::size_of::<Envelope>() as u64,
        ..Resources::default()
    })?;
    let message_charge = actor.account.reserve(Resources {
        messages: 1,
        ..Resources::default()
    })?;
    let mut owned = [0; 64];
    owned[..arguments.len()].copy_from_slice(arguments);
    Ok(Envelope {
        handler: handler.clone(),
        arguments: owned,
        count: arguments.len(),
        max_steps,
        record_mask,
        lists,
        bytes,
        reply,
        _charge: charge,
        _message_charge: Some(message_charge),
    })
}
