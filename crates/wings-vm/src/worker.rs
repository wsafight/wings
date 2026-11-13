use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::ingress::IngressReceiver;
use crate::ingress_requests::ReplyPublisher;
use crate::timer::DeadlineTimer;
use crate::{
    CancellationToken, ExecutionSnapshot, ExecutionStatus, HandlerId, HeapStats, IngressSender,
    PreemptionToken, Reservation, ResourceAccount, ResourceLevel, Resources, ScalarModule,
    ScalarTask, StackPool,
};

static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
static NEXT_CAPABILITY: AtomicU64 = AtomicU64::new(1);
static NEXT_REQUEST_CAPABILITY: AtomicU64 = AtomicU64::new(1);
const MAX_PROCESSES: usize = 256;
const MAX_MAILBOX: usize = 64;
const MAX_ARGUMENTS: usize = 64;
const INVOCATION_OVERHEAD: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pid {
    worker: u64,
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId {
    worker: u64,
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Reply(i64),
    TimedOut,
    Cancelled,
    Failed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerEvent {
    Idle,
    Suspended(Pid),
    Delivered(Pid),
    Exited { pid: Pid, reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerWait {
    Ready,
    Deadline,
    TimedOut,
}

struct IdleWait {
    parker: crate::WorkerParker,
    charge: Arc<Reservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessPhase {
    Idle,
    Runnable,
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSnapshot<'module> {
    pub phase: ProcessPhase,
    pub queued: usize,
    pub mailbox_capacity: usize,
    pub active_handler: Option<&'module str>,
    pub state_type: wings_abi::ValueType,
    pub committed_state: Option<i64>,
    pub execution: Option<ExecutionSnapshot>,
    pub heap: Option<HeapStats>,
    pub resources: Resources,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitSnapshot {
    pub pid: Pid,
    pub execution: ExecutionSnapshot,
    pub state_type: wings_abi::ValueType,
    pub committed_state: Option<i64>,
    pub queued: usize,
    pub resources_before_cleanup: Resources,
    handler: [u8; 256],
    handler_len: usize,
}

impl ExitSnapshot {
    pub fn handler(&self) -> &str {
        std::str::from_utf8(&self.handler[..self.handler_len]).expect("validated handler name")
    }
}

pub struct ScalarWorker {
    idle_wait: Option<IdleWait>,
    timer: Option<DeadlineTimer>,
    preemption: PreemptionToken,
    identity: u64,
    slots: Box<[ProcessSlot]>,
    requests: Box<[RequestSlot]>,
    pool: StackPool,
    tenant: ResourceAccount,
    cursor: usize,
    last_exit: Option<ExitSnapshot>,
    _charge: Reservation,
}

struct ProcessSlot {
    generation: u32,
    actor: Option<Actor>,
}

struct RequestSlot {
    generation: u32,
    pending: Option<PendingRequest>,
}

struct PendingRequest {
    target: Pid,
    deadline: Instant,
    outcome: Option<RequestOutcome>,
    completion: Option<Completion>,
    _charge: Reservation,
}

struct Completion {
    owner: Pid,
    capability: i64,
    handler: HandlerId,
    budget: u64,
    charge: Reservation,
}

struct Actor {
    ingress: Option<IngressReceiver>,
    capability: i64,
    heap_records: Option<usize>,
    task: ScalarTask,
    active: Option<ActiveMessage>,
    mailbox: Mailbox,
    pool: StackPool,
    account: ResourceAccount,
    _charge: Reservation,
}

struct ActiveMessage {
    message: Message,
    _execution_charge: Reservation,
}

struct Message {
    handler: HandlerId,
    arguments: [i64; MAX_ARGUMENTS],
    count: usize,
    max_steps: u64,
    request: Option<RequestId>,
    system: bool,
    external: Option<ReplyPublisher>,
    record_mask: u64,
    _charge: Reservation,
}

struct Mailbox {
    slots: Box<[Option<Message>]>,
    head: usize,
    count: usize,
}

impl ScalarWorker {
    pub fn new(tenant: &ResourceAccount, capacity: usize) -> Result<Self, String> {
        Self::with_request_capacity(tenant, capacity, capacity)
    }

    pub fn with_request_capacity(
        tenant: &ResourceAccount,
        capacity: usize,
        request_capacity: usize,
    ) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant || !(1..=MAX_PROCESSES).contains(&capacity) {
            return Err("scalar worker requires a tenant and 1..=256 process slots".into());
        }
        if !(1..=MAX_PROCESSES).contains(&request_capacity) {
            return Err("scalar worker requires 1..=256 request slots".into());
        }
        let bytes = std::mem::size_of::<Self>()
            + capacity * std::mem::size_of::<ProcessSlot>()
            + request_capacity * std::mem::size_of::<RequestSlot>();
        let charge = tenant.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let mut identity = NEXT_WORKER.load(Ordering::Relaxed);
        let identity = loop {
            let next = identity
                .checked_add(1)
                .ok_or("worker identities exhausted")?;
            match NEXT_WORKER.compare_exchange_weak(
                identity,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break identity,
                Err(current) => identity = current,
            }
        };
        let slots = storage(capacity, || ProcessSlot {
            generation: 1,
            actor: None,
        })?;
        let requests = storage(request_capacity, || RequestSlot {
            generation: 1,
            pending: None,
        })?;
        let pool = StackPool::new_accounted(tenant, capacity, 2.min(capacity))?;
        Ok(Self {
            idle_wait: None,
            timer: None,
            preemption: PreemptionToken::default(),
            identity,
            slots,
            requests,
            pool,
            tenant: tenant.clone(),
            cursor: 0,
            last_exit: None,
            _charge: charge,
        })
    }

    pub fn enable_timer(&mut self) -> Result<(), String> {
        if self.timer.is_some() {
            return Err("worker timer is already enabled".into());
        }
        self.timer = Some(DeadlineTimer::new(&self.tenant, &self.preemption)?);
        Ok(())
    }

    pub fn enable_wait(&mut self) -> Result<(), String> {
        if self.idle_wait.is_some() {
            return Err("worker wait is already enabled".into());
        }
        let charge = self.tenant.reserve(Resources {
            bytes: 4096,
            ..Resources::default()
        })?;
        let parker =
            crate::WorkerParker::new().map_err(|error| format!("create worker wait: {error}"))?;
        self.idle_wait = Some(IdleWait {
            parker,
            charge: Arc::new(charge),
        });
        Ok(())
    }

    pub fn wait(&mut self, max_wait: Duration) -> Result<WorkerWait, String> {
        let end = Instant::now()
            .checked_add(max_wait)
            .ok_or("worker wait deadline overflow")?;
        if self.idle_wait.is_none() {
            return Err("worker wait is not enabled".into());
        }
        let expired = self.expire_requests();
        self.flush_completions();
        if self.has_ready() {
            return Ok(WorkerWait::Ready);
        }
        if expired {
            return Ok(WorkerWait::Deadline);
        }
        let deadline = self
            .requests
            .iter()
            .filter_map(|slot| slot.pending.as_ref())
            .filter(|pending| pending.outcome.is_none())
            .map(|pending| pending.deadline)
            .min()
            .map_or(end, |deadline| deadline.min(end));
        self.idle_wait
            .as_mut()
            .expect("enabled worker wait")
            .parker
            .wait(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| format!("worker wait failed: {error}"))?;
        let expired = self.expire_requests();
        self.flush_completions();
        Ok(if self.has_ready() {
            WorkerWait::Ready
        } else if expired {
            WorkerWait::Deadline
        } else {
            WorkerWait::TimedOut
        })
    }

    fn has_ready(&self) -> bool {
        self.slots
            .iter()
            .filter_map(|slot| slot.actor.as_ref())
            .any(|actor| {
                actor.active.is_some()
                    || actor.mailbox.count != 0
                    || actor
                        .ingress
                        .as_ref()
                        .is_some_and(IngressReceiver::has_pending)
            })
    }

    pub fn open_ingress(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        capacity: usize,
        max_steps: u64,
    ) -> Result<IngressSender, String> {
        if handler.schema().reply.is_some() {
            return Err("one-way ingress requires a one-way handler".into());
        }
        self.open_ingress_kind(pid, handler, capacity, max_steps)
    }

    pub fn open_request_ingress(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        capacity: usize,
        max_steps: u64,
    ) -> Result<IngressSender, String> {
        if handler.schema().reply.is_none() {
            return Err("request ingress requires a reply handler".into());
        }
        self.open_ingress_kind(pid, handler, capacity, max_steps)
    }

    fn open_ingress_kind(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        capacity: usize,
        max_steps: u64,
    ) -> Result<IngressSender, String> {
        let wait = self
            .idle_wait
            .as_ref()
            .ok_or("worker wait is not enabled")?;
        let actor = self.actor(pid)?;
        if actor.ingress.is_some() {
            return Err("actor ingress is already registered".into());
        }
        if !Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let (receiver, sender) = IngressReceiver::new(
            &actor.account,
            handler.clone(),
            capacity,
            max_steps,
            std::mem::size_of::<Message>() as u64,
            wait.parker.notifier(),
            wait.charge.clone(),
        )?;
        self.actor_mut(pid)?.ingress = Some(receiver);
        Ok(sender)
    }

    pub fn tick_timed(
        &mut self,
        slice_steps: u64,
        duration: Duration,
    ) -> Result<WorkerEvent, String> {
        if slice_steps == 0 || duration.is_zero() {
            return Err("timed worker slice requires nonzero steps and duration".into());
        }
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("worker deadline is out of range")?;
        self.timer
            .as_ref()
            .ok_or("worker timer is not enabled")?
            .arm(deadline)?;
        let result = self.tick(slice_steps);
        self.timer.as_ref().expect("enabled worker timer").disarm();
        result
    }

    pub fn spawn(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox_capacity: usize,
    ) -> Result<Pid, String> {
        self.spawn_configured(module, limits, mailbox_capacity, None)
    }

    pub fn spawn_with_heap(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox_capacity: usize,
        heap_records: usize,
    ) -> Result<Pid, String> {
        self.spawn_configured(module, limits, mailbox_capacity, Some(heap_records))
    }

    pub(crate) fn spawn_configured(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox_capacity: usize,
        heap_records: Option<usize>,
    ) -> Result<Pid, String> {
        module.validate_vm(&self.tenant)?;
        if !(1..=MAX_MAILBOX).contains(&mailbox_capacity) {
            return Err("mailbox requires 1..=64 messages".into());
        }
        let index = self
            .slots
            .iter()
            .position(|slot| slot.actor.is_none() && slot.generation != 0)
            .ok_or("worker process table exhausted")?;
        let mut capability = NEXT_CAPABILITY.load(Ordering::Relaxed);
        let capability = loop {
            if capability > i64::MAX as u64 {
                return Err("process capabilities exhausted".into());
            }
            match NEXT_CAPABILITY.compare_exchange_weak(
                capability,
                capability + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break capability as i64,
                Err(current) => capability = current,
            }
        };
        let bytes = std::mem::size_of::<Actor>()
            + mailbox_capacity * std::mem::size_of::<Option<Message>>();
        let charge = self.tenant.reserve(Resources {
            bytes: bytes as u64,
            processes: 1,
            ..Resources::default()
        })?;
        let account = self.tenant.process(limits)?;
        let mut charge = charge;
        charge.transfer(&account)?;
        let mailbox = Mailbox {
            slots: storage(mailbox_capacity, || None)?,
            head: 0,
            count: 0,
        };
        let pool = self.pool.for_process(&account)?;
        let process = match heap_records {
            Some(capacity) => module.instantiate_with_heap(&pool, &account, capacity)?,
            None => module.instantiate_in_pool(&pool)?,
        };
        let mut task = ScalarTask::new(process);
        task.enable_effects();
        self.slots[index].actor = Some(Actor {
            ingress: None,
            capability,
            heap_records,
            task,
            active: None,
            mailbox,
            pool,
            account,
            _charge: charge,
        });
        Ok(self.pid(index))
    }

    pub fn send(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
    ) -> Result<(), String> {
        self.enqueue(pid, handler, arguments, max_steps, None)
    }

    pub fn request(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        timeout: Duration,
    ) -> Result<RequestId, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("request deadline is out of range")?;
        let actor = self.actor(pid)?;
        let index = self
            .requests
            .iter()
            .position(|slot| slot.pending.is_none() && slot.generation != 0)
            .ok_or("request_table_full")?;
        let request = RequestId {
            worker: self.identity,
            slot: index as u32,
            generation: self.requests[index].generation,
        };
        let charge = actor.account.reserve(Resources {
            requests: 1,
            ..Resources::default()
        })?;
        self.enqueue(pid, handler, arguments, max_steps, Some(request))?;
        self.requests[index].pending = Some(PendingRequest {
            target: pid,
            deadline,
            outcome: None,
            completion: None,
            _charge: charge,
        });
        Ok(request)
    }

    pub fn take_reply(&mut self, request: RequestId) -> Result<Option<RequestOutcome>, String> {
        self.expire_requests();
        if self.pending(request)?.completion.is_some() {
            return Err("language request results require completion events".into());
        }
        let outcome = self.pending(request)?.outcome;
        if outcome.is_some() {
            let slot = &mut self.requests[request.slot as usize];
            slot.pending.take();
            slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        }
        Ok(outcome)
    }

    pub fn cancel_request(&mut self, request: RequestId) -> Result<(), String> {
        self.expire_requests();
        if self.pending(request)?.completion.is_some() {
            return Err("language request cancellation requires its owner".into());
        }
        self.pending(request)?;
        self.finish_request(request, RequestOutcome::Cancelled);
        Ok(())
    }

    fn enqueue(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
    ) -> Result<(), String> {
        self.enqueue_payload(pid, handler, arguments, max_steps, request, 0)
    }

    fn enqueue_payload(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        record_mask: u64,
    ) -> Result<(), String> {
        let actor = self.actor_mut(pid)?;
        if !std::sync::Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let schema = handler.schema();
        if schema.is_completion() {
            return Err("completion events cannot be sent directly".into());
        }
        if schema.reply.is_some() != request.is_some() {
            return Err("send requires a one-way handler; request requires a reply handler".into());
        }
        let expected_mask = schema
            .parameters
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, (_, ty))| {
                mask | (u64::from(*ty == wings_abi::ValueType::Ref) << index)
            });
        if record_mask != expected_mask
            || arguments.len() != schema.parameters.len()
            || arguments.len() > MAX_ARGUMENTS
            || arguments
                .iter()
                .zip(&schema.parameters)
                .any(|(value, (_, ty))| *ty != wings_abi::ValueType::Ref && !ty.accepts(*value))
        {
            return Err("handler argument count or scalar type mismatch".into());
        }
        if actor.mailbox.count == actor.mailbox.slots.len() {
            return Err("mailbox_full".into());
        }
        let charge = actor.account.reserve(Resources {
            messages: 1,
            bytes: std::mem::size_of::<Message>() as u64,
            ..Resources::default()
        })?;
        let mut owned_arguments = [0; MAX_ARGUMENTS];
        owned_arguments[..arguments.len()].copy_from_slice(arguments);
        let message = Message {
            handler: handler.clone(),
            arguments: owned_arguments,
            count: arguments.len(),
            max_steps,
            request,
            system: false,
            external: None,
            record_mask,
            _charge: charge,
        };
        let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
        actor.mailbox.slots[tail] = Some(message);
        actor.mailbox.count += 1;
        Ok(())
    }

    pub fn tick(&mut self, slice_steps: u64) -> Result<WorkerEvent, String> {
        if slice_steps == 0 {
            return Err("worker slice must contain at least one step".into());
        }
        self.expire_requests();
        self.flush_completions();
        for _ in 0..self.slots.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.slots.len();
            let pid = self.pid(index);
            let Some(actor) = self.slots[index].actor.as_mut() else {
                continue;
            };
            if actor.mailbox.count < actor.mailbox.slots.len() {
                if let Some(ingress) = &actor.ingress {
                    if let Some(envelope) = ingress.take() {
                        let tail =
                            (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
                        actor.mailbox.slots[tail] = Some(Message {
                            handler: ingress.handler.clone(),
                            arguments: envelope.arguments,
                            count: envelope.count,
                            max_steps: ingress.max_steps,
                            request: None,
                            system: false,
                            external: envelope.reply,
                            record_mask: 0,
                            _charge: envelope.charge,
                        });
                        actor.mailbox.count += 1;
                    }
                }
            }
            if actor.active.is_none() {
                if actor.mailbox.count == 0 {
                    continue;
                }
                let execution_charge = actor.account.reserve(Resources {
                    bytes: INVOCATION_OVERHEAD,
                    ..Resources::default()
                })?;
                let message = actor.mailbox.slots[actor.mailbox.head]
                    .as_ref()
                    .expect("mailbox head");
                actor.task.start_kind(
                    &message.handler,
                    &message.arguments[..message.count],
                    message.max_steps,
                    &CancellationToken::default(),
                    &self.preemption,
                    &actor.pool,
                    message.system,
                    message.record_mask,
                )?;
                let message = actor.mailbox.slots[actor.mailbox.head]
                    .take()
                    .expect("started message");
                actor.mailbox.head = (actor.mailbox.head + 1) % actor.mailbox.slots.len();
                actor.mailbox.count -= 1;
                actor.active = Some(ActiveMessage {
                    message,
                    _execution_charge: execution_charge,
                });
            }
            let status = actor.task.resume(slice_steps);
            self.expire_requests();
            return match status {
                Ok(ExecutionStatus::Suspended) => {
                    let actor = self.slots[index].actor.as_mut().expect("suspended actor");
                    if let Some(effect) = actor.task.take_effect() {
                        let (status, value) = self.apply_effect(pid, effect);
                        self.slots[index]
                            .actor
                            .as_mut()
                            .expect("effect actor")
                            .task
                            .acknowledge_effect(status, value);
                    }
                    Ok(WorkerEvent::Suspended(pid))
                }
                Ok(ExecutionStatus::Complete(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, active.message.external, reply) {
                        (Some(request), None, Some(value)) => {
                            self.finish_request(request, RequestOutcome::Reply(value));
                        }
                        (None, Some(publisher), Some(value)) => publisher.complete(value),
                        (None, None, None) => {}
                        _ => {
                            self.remove(index);
                            return Err("handler reply does not match message kind".into());
                        }
                    }
                    Ok(WorkerEvent::Delivered(pid))
                }
                Err(reason) => {
                    let actor = self.slots[index].actor.as_ref().expect("failed actor");
                    let name = &actor
                        .active
                        .as_ref()
                        .expect("failed message")
                        .message
                        .handler
                        .schema()
                        .name;
                    let mut handler = [0; 256];
                    handler[..name.len()].copy_from_slice(name.as_bytes());
                    let state_type = actor.task.module().manifest().state;
                    self.last_exit = Some(ExitSnapshot {
                        pid,
                        execution: actor.task.snapshot().expect("failed invocation snapshot"),
                        state_type,
                        committed_state: (!state_type.is_reference())
                            .then(|| actor.task.committed_state()),
                        queued: actor.mailbox.count,
                        resources_before_cleanup: actor.account.used(),
                        handler,
                        handler_len: name.len(),
                    });
                    self.remove(index);
                    Ok(WorkerEvent::Exited { pid, reason })
                }
            };
        }
        Ok(WorkerEvent::Idle)
    }

    pub fn stop(&mut self, pid: Pid) -> Result<(), String> {
        self.actor_mut(pid)?;
        self.expire_requests();
        self.remove(pid.slot as usize);
        Ok(())
    }

    pub fn queued(&self, pid: Pid) -> Result<usize, String> {
        self.actor(pid).map(|actor| actor.mailbox.count)
    }

    pub fn resources(&self, pid: Pid) -> Result<Resources, String> {
        self.actor(pid).map(|actor| actor.account.used())
    }

    pub fn last_exit(&self) -> Option<&ExitSnapshot> {
        self.last_exit.as_ref()
    }

    pub fn snapshot(&self, pid: Pid) -> Result<ProcessSnapshot<'_>, String> {
        let actor = self.actor(pid)?;
        let state_type = actor.task.module().manifest().state;
        Ok(ProcessSnapshot {
            phase: if actor.active.is_some() {
                ProcessPhase::Suspended
            } else if actor.mailbox.count != 0 {
                ProcessPhase::Runnable
            } else {
                ProcessPhase::Idle
            },
            queued: actor.mailbox.count,
            mailbox_capacity: actor.mailbox.slots.len(),
            active_handler: actor
                .active
                .as_ref()
                .map(|active| active.message.handler.schema().name.as_str()),
            state_type,
            committed_state: (!state_type.is_reference()).then(|| actor.task.committed_state()),
            execution: actor.task.snapshot(),
            heap: actor.task.heap_stats(),
            resources: actor.account.used(),
        })
    }

    pub fn committed_state(&self, pid: Pid) -> Result<i64, String> {
        let actor = self.actor(pid)?;
        if actor.task.module().manifest().state.is_reference() {
            return Err("record state is not exposed as a scalar diagnostic".into());
        }
        Ok(actor.task.committed_state())
    }

    pub fn process_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.actor.is_some())
            .count()
    }

    pub fn pids(&self) -> impl Iterator<Item = Pid> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.actor.as_ref().map(|_| self.pid(index)))
    }

    fn apply_effect(&mut self, pid: Pid, effect: crate::execution::ProcessEffect) -> (u32, i64) {
        let actor = self.actor(pid).expect("effect actor");
        let module = actor.task.module().clone();
        let budget = actor
            .active
            .as_ref()
            .expect("active effect message")
            .message
            .max_steps;
        match effect.operation {
            wings_abi::EFFECT_REQUEST => {
                let result = self.language_request(pid, effect);
                match result {
                    Ok(value) => (wings_abi::STATUS_OK, value),
                    Err(_) => (wings_abi::STATUS_REQUEST_REJECTED, 0),
                }
            }
            wings_abi::EFFECT_CANCEL_REQUEST => {
                let request = self.requests.iter().enumerate().find_map(|(index, slot)| {
                    slot.pending
                        .as_ref()
                        .and_then(|pending| pending.completion.as_ref())
                        .filter(|completion| {
                            completion.owner == pid && completion.capability == effect.target
                        })
                        .map(|_| RequestId {
                            worker: self.identity,
                            slot: index as u32,
                            generation: slot.generation,
                        })
                });
                if let Some(request) = request {
                    self.finish_request(request, RequestOutcome::Cancelled);
                    (wings_abi::STATUS_OK, 0)
                } else {
                    (wings_abi::STATUS_REQUEST_REJECTED, 0)
                }
            }
            wings_abi::EFFECT_SELF_PID if effect.count == 0 => {
                (wings_abi::STATUS_OK, actor.capability)
            }
            wings_abi::EFFECT_SPAWN_SELF if effect.count == 0 => {
                let limits = actor.account.limit();
                let capacity = actor.mailbox.slots.len();
                let heap_records = actor.heap_records;
                match self.spawn_configured(&module, limits, capacity, heap_records) {
                    Ok(child) => (
                        wings_abi::STATUS_OK,
                        self.actor(child).expect("spawned actor").capability,
                    ),
                    Err(_) => (wings_abi::STATUS_SPAWN_REJECTED, 0),
                }
            }
            wings_abi::EFFECT_SEND_SELF | wings_abi::EFFECT_SEND => {
                let target = if effect.operation == wings_abi::EFFECT_SEND_SELF {
                    Some(pid)
                } else {
                    self.slots.iter().enumerate().find_map(|(index, slot)| {
                        slot.actor
                            .as_ref()
                            .filter(|target| target.capability == effect.target)
                            .map(|_| self.pid(index))
                    })
                };
                let accepted = target
                    .zip(module.manifest().handlers.get(effect.handler))
                    .and_then(|(target, schema)| {
                        module
                            .handler(&schema.name)
                            .ok()
                            .map(|handler| (target, handler))
                    })
                    .is_some_and(|(target, handler)| {
                        self.enqueue_payload(
                            target,
                            &handler,
                            &effect.arguments[..effect.count],
                            budget,
                            None,
                            effect.record_mask,
                        )
                        .is_ok()
                    });
                (
                    if accepted {
                        wings_abi::STATUS_OK
                    } else {
                        wings_abi::STATUS_SEND_REJECTED
                    },
                    0,
                )
            }
            _ => (wings_abi::STATUS_INVALID_CONTROL, 0),
        }
    }

    fn pid(&self, index: usize) -> Pid {
        Pid {
            worker: self.identity,
            slot: index as u32,
            generation: self.slots[index].generation,
        }
    }

    fn actor(&self, pid: Pid) -> Result<&Actor, String> {
        if pid.worker != self.identity {
            return Err("Pid belongs to a different worker".into());
        }
        self.slots
            .get(pid.slot as usize)
            .filter(|slot| slot.generation == pid.generation)
            .and_then(|slot| slot.actor.as_ref())
            .ok_or_else(|| "stale or exited Pid".into())
    }

    fn actor_mut(&mut self, pid: Pid) -> Result<&mut Actor, String> {
        self.actor(pid)?;
        Ok(self.slots[pid.slot as usize]
            .actor
            .as_mut()
            .expect("validated actor"))
    }

    fn remove(&mut self, index: usize) {
        let pid = self.pid(index);
        for slot in &mut self.requests {
            if slot
                .pending
                .as_ref()
                .and_then(|pending| pending.completion.as_ref())
                .is_some_and(|completion| completion.owner == pid)
            {
                slot.pending.take();
                slot.generation = slot.generation.checked_add(1).unwrap_or(0);
                continue;
            }
            if let Some(pending) = &mut slot.pending {
                if pending.target == pid && pending.outcome.is_none() {
                    pending.outcome = Some(RequestOutcome::Failed);
                }
            }
        }
        self.slots[index].actor.take();
        self.slots[index].generation = self.slots[index].generation.checked_add(1).unwrap_or(0);
    }

    fn pending(&self, request: RequestId) -> Result<&PendingRequest, String> {
        if request.worker != self.identity {
            return Err("request ID belongs to a different worker".into());
        }
        self.requests
            .get(request.slot as usize)
            .filter(|slot| slot.generation == request.generation)
            .and_then(|slot| slot.pending.as_ref())
            .ok_or_else(|| "stale or consumed request ID".into())
    }

    fn finish_request(&mut self, request: RequestId, outcome: RequestOutcome) {
        if self.pending(request).is_ok() {
            let pending = self.requests[request.slot as usize]
                .pending
                .as_mut()
                .expect("validated request");
            if pending.outcome.is_none() {
                pending.outcome = Some(outcome);
            }
        }
    }

    fn expire_requests(&mut self) -> bool {
        let now = Instant::now();
        let mut expired = false;
        for slot in &mut self.requests {
            if let Some(pending) = &mut slot.pending {
                if pending.outcome.is_none() && pending.deadline <= now {
                    pending.outcome = Some(RequestOutcome::TimedOut);
                    expired = true;
                }
            }
        }
        expired
    }

    fn language_request(
        &mut self,
        owner: Pid,
        effect: crate::execution::ProcessEffect,
    ) -> Result<i64, String> {
        if !(0..=86_400_000).contains(&effect.timeout_ms) {
            return Err("invalid request timeout".into());
        }
        let actor = self.actor(owner)?;
        let module = actor.task.module().clone();
        let target = self
            .slots
            .iter()
            .enumerate()
            .find_map(|(index, slot)| {
                slot.actor
                    .as_ref()
                    .filter(|actor| actor.capability == effect.target)
                    .map(|_| self.pid(index))
            })
            .ok_or("invalid request target")?;
        let schema = module
            .manifest()
            .handlers
            .get(effect.handler)
            .ok_or("invalid request handler")?;
        if schema.reply != Some(wings_abi::ValueType::Int) {
            return Err("request requires Int reply".into());
        }
        let callback = module
            .manifest()
            .handlers
            .get(effect.callback)
            .filter(|schema| schema.is_completion())
            .ok_or("invalid request completion")?;
        let handler = module.handler(&schema.name)?;
        let callback = module.handler(&callback.name)?;
        let index = self
            .requests
            .iter()
            .position(|slot| slot.pending.is_none() && slot.generation != 0)
            .ok_or("request_table_full")?;
        let mut capability = NEXT_REQUEST_CAPABILITY.load(Ordering::Relaxed);
        let capability = loop {
            if capability > i64::MAX as u64 {
                return Err("request capabilities exhausted".into());
            }
            match NEXT_REQUEST_CAPABILITY.compare_exchange_weak(
                capability,
                capability + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break capability as i64,
                Err(current) => capability = current,
            }
        };
        let request = RequestId {
            worker: self.identity,
            slot: index as u32,
            generation: self.requests[index].generation,
        };
        let budget = actor
            .active
            .as_ref()
            .expect("request active message")
            .message
            .max_steps;
        let charge = actor.account.reserve(Resources {
            requests: 1,
            ..Resources::default()
        })?;
        let event_charge = actor.account.reserve(Resources {
            messages: 1,
            bytes: std::mem::size_of::<Message>() as u64,
            ..Resources::default()
        })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(effect.timeout_ms as u64))
            .ok_or("request deadline overflow")?;
        self.enqueue_payload(
            target,
            &handler,
            &effect.arguments[..effect.count],
            budget,
            Some(request),
            effect.record_mask,
        )?;
        self.requests[index].pending = Some(PendingRequest {
            target,
            deadline,
            outcome: None,
            completion: Some(Completion {
                owner,
                capability,
                handler: callback,
                budget,
                charge: event_charge,
            }),
            _charge: charge,
        });
        Ok(capability)
    }

    fn flush_completions(&mut self) {
        for index in 0..self.requests.len() {
            let Some(pending) = self.requests[index].pending.as_ref() else {
                continue;
            };
            let Some(outcome) = pending.outcome else {
                continue;
            };
            let Some(completion) = pending.completion.as_ref() else {
                continue;
            };
            let owner = completion.owner;
            let Ok(actor) = self.actor(owner) else {
                continue;
            };
            if actor.mailbox.count == actor.mailbox.slots.len() {
                continue;
            }
            let mut pending = self.requests[index]
                .pending
                .take()
                .expect("completed request");
            let completion = pending.completion.take().expect("completion route");
            self.requests[index].generation =
                self.requests[index].generation.checked_add(1).unwrap_or(0);
            let mut arguments = [0; MAX_ARGUMENTS];
            arguments[0] = completion.capability;
            (arguments[1], arguments[2]) = match outcome {
                RequestOutcome::Reply(value) => (0, value),
                RequestOutcome::TimedOut => (1, 0),
                RequestOutcome::Cancelled => (2, 0),
                RequestOutcome::Failed => (3, 0),
            };
            let message = Message {
                handler: completion.handler,
                arguments,
                count: 3,
                max_steps: completion.budget,
                request: None,
                system: true,
                external: None,
                record_mask: 0,
                _charge: completion.charge,
            };
            let actor = self.actor_mut(owner).expect("completion recipient");
            let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
            actor.mailbox.slots[tail] = Some(message);
            actor.mailbox.count += 1;
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
        return Err("allocator changed requested fixed capacity".into());
    }
    values.resize_with(capacity, &mut initialize);
    Ok(values.into_boxed_slice())
}

#[cfg(test)]
mod wait_tests {
    use super::*;

    #[test]
    fn pending_without_ready_work_fixture_uses_earliest_deadline_once() {
        let limit = Resources {
            bytes: 16 << 20,
            processes: 4,
            messages: 4,
            requests: 4,
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 2).unwrap();
        worker.enable_wait().unwrap();
        let early = Instant::now() + Duration::from_millis(2);
        for (index, deadline) in [early, early + Duration::from_secs(60)]
            .into_iter()
            .enumerate()
        {
            worker.requests[index].pending = Some(PendingRequest {
                target: worker.pid(0),
                deadline,
                outcome: None,
                completion: None,
                _charge: tenant
                    .reserve(Resources {
                        requests: 1,
                        ..Resources::default()
                    })
                    .unwrap(),
            });
        }
        assert_eq!(
            worker.wait(Duration::from_secs(1)).unwrap(),
            WorkerWait::Deadline
        );
        assert!(Instant::now() >= early);
        assert_eq!(worker.wait(Duration::ZERO).unwrap(), WorkerWait::TimedOut);
        let request = RequestId {
            worker: worker.identity,
            slot: 0,
            generation: worker.requests[0].generation,
        };
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::TimedOut)
        );
        assert!(
            worker.requests[1]
                .pending
                .as_ref()
                .unwrap()
                .outcome
                .is_none()
        );
        let start = Instant::now();
        assert_eq!(
            worker.wait(Duration::from_millis(2)).unwrap(),
            WorkerWait::TimedOut
        );
        assert!(start.elapsed() >= Duration::from_millis(2));
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }
}
