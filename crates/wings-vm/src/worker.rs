use std::sync::atomic::{AtomicU64, Ordering};

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
    pool: StackPool,
    tenant: ResourceAccount,
    cursor: usize,
    _charge: Reservation,
}

struct ProcessSlot {
    generation: u32,
    actor: Option<Actor>,
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
    _message: Message,
    _execution_charge: Reservation,
}

struct Message {
    handler: HandlerId,
    arguments: [i64; MAX_ARGUMENTS],
    count: usize,
    max_steps: u64,
    _charge: Reservation,
}

struct Mailbox {
    slots: Box<[Option<Message>]>,
    head: usize,
    count: usize,
}

impl ScalarWorker {
    pub fn new(tenant: &ResourceAccount, capacity: usize) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant || !(1..=MAX_PROCESSES).contains(&capacity) {
            return Err("scalar worker requires a tenant and 1..=256 process slots".into());
        }
        let bytes = std::mem::size_of::<Self>() + capacity * std::mem::size_of::<ProcessSlot>();
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
        let pool = StackPool::new_accounted(tenant, capacity, 2.min(capacity))?;
        Ok(Self {
            identity,
            slots,
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
        let actor = self.actor_mut(pid)?;
        if !std::sync::Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let schema = handler.schema();
        if schema.reply.is_some() {
            return Err("send requires a one-way handler".into());
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
                    _message: message,
                    _execution_charge: execution_charge,
                });
            }
            return match actor.task.resume(slice_steps) {
                Ok(ExecutionStatus::Suspended) => Ok(WorkerEvent::Suspended(pid)),
                Ok(ExecutionStatus::Complete(None)) => {
                    actor.active.take();
                    Ok(WorkerEvent::Delivered(pid))
                }
                Ok(ExecutionStatus::Complete(Some(_))) => {
                    self.remove(index);
                    Err("one-way handler returned a request reply".into())
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
        self.slots[index].actor.take();
        self.slots[index].generation = self.slots[index].generation.checked_add(1).unwrap_or(0);
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
