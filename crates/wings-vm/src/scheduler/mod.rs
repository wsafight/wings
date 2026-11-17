use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
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

mod locks;
mod ready;
mod run;
mod state;
mod supervisor;
mod yields;
pub use locks::{SchedulerLockMetrics, SchedulerLockStatistics};
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
    changed: Condvar,
    tenant: ResourceAccount,
    replies: Arc<ReplyTable>,
    config: SchedulerConfig,
    identity: u64,
    observations: Box<[yields::WorkerObservation]>,
    lock_observations: Box<[locks::WorkerLockObservation]>,
    _charge: Reservation,
}

struct State {
    closed: bool,
    last_exit: Option<SchedulerExitSnapshot>,
    slots: Box<[Slot]>,
    requests: Box<[RequestSlot]>,
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
    byte_state: Option<crate::ByteStateSnapshot>,
    stopping: bool,
    cancellation: CancellationToken,
    capability: i64,
    heap_records: Option<usize>,
    mailbox: Mailbox,
    _charge: Reservation,
    termination: Option<supervisor::ExitPublisher>,
}

struct Mailbox {
    slots: Box<[Option<Envelope>]>,
    head: usize,
    count: usize,
}

struct Envelope {
    handler: HandlerId,
    arguments: [i64; 64],
    count: usize,
    max_steps: u64,
    record_mask: u64,
    lists: ListPayload,
    bytes: BytePayload,
    system: bool,
    reply: Reply,
    _charge: Reservation,
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
        if tenant.level() != ResourceLevel::Tenant
            || !(1..=16).contains(&config.workers)
            || !(1..=MAX_ACTORS).contains(&config.capacity)
            || !(1..=MAX_REQUESTS).contains(&config.requests)
            || config.slice_steps == 0
            || config.quantum.is_zero()
            || config.quantum > Duration::from_secs(1)
            || config.yield_samples > 65536
        {
            return Err("scheduler requires a tenant, 1..=16 workers, 1..=16384 actor slots, 1..=256 request slots, nonzero bounded slices and 0..=65536 yield samples per worker".into());
        }
        let bytes = std::mem::size_of::<Shared>()
            + config.capacity * std::mem::size_of::<Slot>()
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
            + 256;
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
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                closed: false,
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
                requests: storage(config.requests, || RequestSlot {
                    generation: 1,
                    pending: None,
                })?,
                ready: storage(config.workers, ready::ReadyQueue::default)?,
                free: Some(0),
            }),
            changed: Condvar::new(),
            tenant: tenant.clone(),
            replies: ReplyTable::new(tenant, config.requests)?,
            config,
            identity,
            observations: observations.into_boxed_slice(),
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
                .spawn(move || run::worker(shared, index, ready))
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
            let mut actor = state.slots[index].actor.take();
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
        for request in &mut state.requests {
            request.pending.take();
        }
    }
}

impl SchedulerHandle {
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
            let byte_state = image.byte_state_snapshot();
            let cancellation = CancellationToken::new_accounted(&account)?;
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
                byte_state,
                stopping: false,
                cancellation,
                capability: next_identity(&NEXT_CAPABILITY)? as i64,
                heap_records,
                mailbox: Mailbox {
                    slots,
                    head: 0,
                    count: 0,
                },
                _charge: charge,
                termination,
            })
        })();
        let mut state = self.shared.lock();
        state.slots[pid.slot as usize].creating = false;
        match result {
            Ok(actor) if !state.closed => {
                state.slots[pid.slot as usize].actor = Some(actor);
                drop(state);
                self.shared.changed.notify_all();
                Ok(pid)
            }
            Ok(actor) => {
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
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate(
            actor,
            handler,
            arguments,
            false,
            0,
            &ListPayload::default(),
            &BytePayload::default(),
        )?;
        if handler.schema().reply.is_some() {
            return Err("send requires a one-way handler".into());
        }
        let message = envelope(
            actor,
            handler,
            arguments,
            max_steps,
            0,
            ListPayload::default(),
            BytePayload::default(),
            false,
            Reply::None,
        )?;
        actor.mailbox.push(message)?;
        state.enqueue(pid);
        drop(state);
        self.shared.changed.notify_all();
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
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        validate(
            actor,
            handler,
            arguments,
            false,
            0,
            &ListPayload::default(),
            &BytePayload::default(),
        )?;
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
        let mut message = envelope(
            actor,
            handler,
            arguments,
            max_steps,
            0,
            ListPayload::default(),
            BytePayload::default(),
            false,
            Reply::None,
        )?;
        let (request, publisher) = self.shared.replies.start(deadline, &actor.account, reply)?;
        message.reply = Reply::External(publisher);
        actor.mailbox.push(message)?;
        state.enqueue(pid);
        drop(state);
        self.shared.changed.notify_all();
        Ok(request)
    }

    pub fn stop(&self, pid: SchedulerPid) -> Result<(), String> {
        let mut state = self.shared.lock();
        state.open()?;
        let actor = state.actor_mut(self.shared.identity, pid)?;
        actor.stopping = true;
        actor.cancellation.cancel();
        let retired = if actor.owner.is_none() {
            state.retire(self.shared.identity, pid)
        } else {
            None
        };
        drop(state);
        drop(retired);
        self.shared.changed.notify_all();
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
            if actor.mailbox.count + usize::from(actor.starting) == actor.mailbox.slots.len() {
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
        state.enqueue(pid);
        drop(state);
        self.shared.changed.notify_all();
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
            queued: actor.mailbox.count,
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
        Ok(actor.byte_state)
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        for actor in state.slots.iter().filter_map(|slot| slot.actor.as_ref()) {
            actor.cancellation.cancel();
        }
        drop(state);
        self.changed.notify_all();
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

impl Mailbox {
    fn push(&mut self, message: Envelope) -> Result<(), String> {
        if self.count == self.slots.len() {
            return Err("mailbox_full".into());
        }
        let tail = (self.head + self.count) % self.slots.len();
        self.slots[tail] = Some(message);
        self.count += 1;
        Ok(())
    }
    fn pop(&mut self) -> Envelope {
        let message = self.slots[self.head].take().expect("ready mailbox");
        self.head = (self.head + 1) % self.slots.len();
        self.count -= 1;
        message
    }
    fn restore(&mut self, message: Envelope) {
        self.head = (self.head + self.slots.len() - 1) % self.slots.len();
        self.slots[self.head] = Some(message);
        self.count += 1;
    }
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

#[allow(clippy::too_many_arguments)]
fn envelope(
    actor: &Actor,
    handler: &HandlerId,
    arguments: &[i64],
    max_steps: u64,
    record_mask: u64,
    lists: ListPayload,
    mut bytes: BytePayload,
    system: bool,
    reply: Reply,
) -> Result<Envelope, String> {
    if actor.mailbox.count + usize::from(actor.starting) == actor.mailbox.slots.len() {
        return Err("mailbox_full".into());
    }
    bytes.transfer(&actor.account)?;
    let charge = actor.account.reserve(Resources {
        messages: 1,
        bytes: std::mem::size_of::<Envelope>() as u64,
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
        system,
        reply,
        _charge: charge,
    })
}
