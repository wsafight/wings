use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::{
    CancellationToken, ExecutionStatus, HandlerId, PreemptionToken, Reservation, ResourceAccount,
    ResourceLevel, Resources, ScalarModule, ScalarTask, StackPool,
};

static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
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

pub struct ScalarWorker {
    identity: u64,
    slots: Box<[ProcessSlot]>,
    requests: Box<[RequestSlot]>,
    pool: StackPool,
    tenant: ResourceAccount,
    cursor: usize,
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
    _charge: Reservation,
}

struct Actor {
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
            identity,
            slots,
            requests,
            pool,
            tenant: tenant.clone(),
            cursor: 0,
            _charge: charge,
        })
    }

    pub fn spawn(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox_capacity: usize,
    ) -> Result<Pid, String> {
        if !(1..=MAX_MAILBOX).contains(&mailbox_capacity) {
            return Err("mailbox requires 1..=64 messages".into());
        }
        let index = self
            .slots
            .iter()
            .position(|slot| slot.actor.is_none() && slot.generation != 0)
            .ok_or("worker process table exhausted")?;
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
        let task = ScalarTask::new(module.instantiate()?);
        self.slots[index].actor = Some(Actor {
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
            _charge: charge,
        });
        Ok(request)
    }

    pub fn take_reply(&mut self, request: RequestId) -> Result<Option<RequestOutcome>, String> {
        self.expire_requests();
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
        let actor = self.actor_mut(pid)?;
        if !std::sync::Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let schema = handler.schema();
        if schema.reply.is_some() != request.is_some() {
            return Err("send requires a one-way handler; request requires a reply handler".into());
        }
        if arguments.len() != schema.parameters.len()
            || arguments.len() > MAX_ARGUMENTS
            || arguments
                .iter()
                .zip(&schema.parameters)
                .any(|(value, (_, ty))| !ty.accepts(*value))
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
        for _ in 0..self.slots.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.slots.len();
            let pid = self.pid(index);
            let Some(actor) = self.slots[index].actor.as_mut() else {
                continue;
            };
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
                actor.task.start(
                    &message.handler,
                    &message.arguments[..message.count],
                    message.max_steps,
                    &CancellationToken::default(),
                    &PreemptionToken::default(),
                    &actor.pool,
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
                Ok(ExecutionStatus::Suspended) => Ok(WorkerEvent::Suspended(pid)),
                Ok(ExecutionStatus::Complete(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, reply) {
                        (Some(request), Some(value)) => {
                            self.finish_request(request, RequestOutcome::Reply(value));
                        }
                        (None, None) => {}
                        _ => {
                            self.remove(index);
                            return Err("handler reply does not match message kind".into());
                        }
                    }
                    Ok(WorkerEvent::Delivered(pid))
                }
                Err(reason) => {
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

    pub fn committed_state(&self, pid: Pid) -> Result<i64, String> {
        self.actor(pid).map(|actor| actor.task.committed_state())
    }

    pub fn process_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.actor.is_some())
            .count()
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

    fn expire_requests(&mut self) {
        let now = Instant::now();
        for slot in &mut self.requests {
            if let Some(pending) = &mut slot.pending {
                if pending.outcome.is_none() && pending.deadline <= now {
                    pending.outcome = Some(RequestOutcome::TimedOut);
                }
            }
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
