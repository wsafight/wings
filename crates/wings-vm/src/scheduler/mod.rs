use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ingress_requests::{ReplyPublisher, ReplyTable};
use crate::message_values::ListPayload;
use crate::{
    CancellationToken, DetachedProcess, ExecutionFault, ExecutionSnapshot, HandlerId,
    IngressRequest, Reservation, ResourceAccount, ResourceLevel, Resources, ScalarModule,
    StackPool,
};

mod run;
mod state;
#[cfg(test)]
mod tests;

const MAX_ACTORS: usize = 256;
const THREAD_STACK: usize = 2 << 20;
const INVOCATION_BYTES: u64 = 4096;
static NEXT_SCHEDULER: AtomicU64 = AtomicU64::new(1);
static NEXT_CAPABILITY: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub struct SchedulerConfig {
    pub workers: usize,
    pub capacity: usize,
    pub requests: usize,
    pub slice_steps: u64,
    pub quantum: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            capacity: 64,
            requests: 64,
            slice_steps: 64,
            quantum: Duration::from_millis(1),
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
    config: SchedulerConfig,
    identity: u64,
    _charge: Reservation,
}

struct State {
    closed: bool,
    last_exit: Option<SchedulerExitSnapshot>,
    slots: Box<[Slot]>,
    requests: Box<[RequestSlot]>,
}

struct Slot {
    generation: u32,
    creating: bool,
    actor: Option<Actor>,
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
    mailbox: Mailbox,
    replies: [Arc<ReplyTable>; 3],
    _charge: Reservation,
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
    handler: HandlerId,
    deadline: Instant,
    max_steps: u64,
    outcome: Option<crate::RequestOutcome>,
    completion_charge: Reservation,
    _charge: Reservation,
}

impl Scheduler {
    pub fn new(tenant: &ResourceAccount, config: SchedulerConfig) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant
            || !(1..=16).contains(&config.workers)
            || !(1..=MAX_ACTORS).contains(&config.capacity)
            || !(1..=MAX_ACTORS).contains(&config.requests)
            || config.slice_steps == 0
            || config.quantum.is_zero()
            || config.quantum > Duration::from_secs(1)
        {
            return Err("scheduler requires a tenant, 1..=16 workers, 1..=256 actor/request slots and nonzero bounded slices".into());
        }
        let bytes = std::mem::size_of::<Shared>()
            + config.capacity * std::mem::size_of::<Slot>()
            + config.requests * std::mem::size_of::<RequestSlot>()
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
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                closed: false,
                last_exit: None,
                slots: storage(config.capacity, || Slot {
                    generation: 1,
                    creating: false,
                    actor: None,
                })?,
                requests: storage(config.requests, || RequestSlot {
                    generation: 1,
                    pending: None,
                })?,
            }),
            changed: Condvar::new(),
            tenant: tenant.clone(),
            config,
            identity,
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
            let actor = state.slots[index].actor.take();
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
    pub fn spawn(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
    ) -> Result<SchedulerPid, String> {
        self.spawn_with_options(module, limits, mailbox, None, None)
    }

    pub fn spawn_with_heap(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
    ) -> Result<SchedulerPid, String> {
        self.spawn_with_options(module, limits, mailbox, Some(records), None)
    }

    fn spawn_with_options(
        &self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        heap_records: Option<usize>,
        pool: Option<&StackPool>,
    ) -> Result<SchedulerPid, String> {
        if !(1..=64).contains(&mailbox) {
            return Err("mailbox requires 1..=64 slots".into());
        }
        module.validate_vm(&self.shared.tenant)?;
        let pid = {
            let mut state = self.shared.lock();
            state.open()?;
            let index = state
                .slots
                .iter()
                .position(|slot| slot.generation != 0 && !slot.creating && slot.actor.is_none())
                .ok_or("scheduler process table full")?;
            state.slots[index].creating = true;
            state.pid(self.shared.identity, index)
        };
        let result = (|| {
            let account = self.shared.tenant.process_accounted(limits)?;
            let charge = account.reserve(Resources {
                processes: 1,
                bytes: (std::mem::size_of::<Actor>()
                    + 64
                    + mailbox * std::mem::size_of::<Option<Envelope>>())
                    as u64,
                ..Resources::default()
            })?;
            let slots = storage(mailbox, || None)?;
            let replies = [
                ReplyTable::new(
                    &account,
                    self.shared.config.requests,
                    wings_abi::ValueType::Int,
                )?,
                ReplyTable::new(
                    &account,
                    self.shared.config.requests,
                    wings_abi::ValueType::Bool,
                )?,
                ReplyTable::new(
                    &account,
                    self.shared.config.requests,
                    wings_abi::ValueType::Unit,
                )?,
            ];
            let temporary;
            let pool = if let Some(pool) = pool {
                pool
            } else {
                temporary = StackPool::new_accounted(&self.shared.tenant, 1, 0)?;
                &temporary
            };
            let bound = pool.for_process(&account)?;
            let initialization_charge = account.reserve(Resources {
                bytes: INVOCATION_BYTES,
                ..Resources::default()
            })?;
            let process = if let Some(records) = heap_records {
                module.instantiate_with_heap(&bound, &account, records)?
            } else {
                module.instantiate_in_pool(&bound)?
            };
            drop(initialization_charge);
            let committed_state =
                (!module.manifest().state.is_reference()).then_some(process.state);
            Ok::<_, String>(Actor {
                module: module.clone(),
                account,
                image: Some(process.detach()),
                owner: None,
                starting: false,
                last_worker: None,
                migrations: 0,
                execution: None,
                committed_state,
                stopping: false,
                cancellation: CancellationToken::default(),
                capability: next_identity(&NEXT_CAPABILITY)? as i64,
                heap_records,
                mailbox: Mailbox {
                    slots,
                    head: 0,
                    count: 0,
                },
                replies,
                _charge: charge,
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
                drop(state);
                drop(actor);
                Err("scheduler is closed".into())
            }
            Err(error) => Err(error),
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
        validate(actor, handler, arguments, false, 0, &ListPayload::default())?;
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
            false,
            Reply::None,
        )?;
        actor.mailbox.push(message)?;
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
        validate(actor, handler, arguments, false, 0, &ListPayload::default())?;
        let reply = match handler.schema().reply {
            Some(wings_abi::ValueType::Int) => 0,
            Some(wings_abi::ValueType::Bool) => 1,
            Some(wings_abi::ValueType::Unit) => 2,
            _ => return Err("request requires a scalar reply".into()),
        };
        let mut message = envelope(
            actor,
            handler,
            arguments,
            max_steps,
            0,
            ListPayload::default(),
            false,
            Reply::None,
        )?;
        let (request, publisher) = actor.replies[reply].start(deadline)?;
        message.reply = Reply::External(publisher);
        actor.mailbox.push(message)?;
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
    for (index, (value, (_, ty))) in arguments.iter().zip(&schema.parameters).enumerate() {
        match ty {
            wings_abi::ValueType::Ref => expected_record |= 1 << index,
            wings_abi::ValueType::ListRef => expected_list |= 1 << index,
            wings_abi::ValueType::RequestId if system && *value > 0 => {}
            _ if ty.accepts(*value) => {}
            _ => return Err("scheduler scalar argument mismatch".into()),
        }
    }
    if record_mask != expected_record || !lists.valid(arguments, expected_list) {
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
    system: bool,
    reply: Reply,
) -> Result<Envelope, String> {
    if actor.mailbox.count + usize::from(actor.starting) == actor.mailbox.slots.len() {
        return Err("mailbox_full".into());
    }
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
        system,
        reply,
        _charge: charge,
    })
}
