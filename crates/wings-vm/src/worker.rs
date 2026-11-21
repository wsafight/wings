use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::host_values::HostMessage;
use crate::ingress::IngressReceiver;
use crate::ingress_requests::ReplyPublisher;
use crate::message_values::{BytePayload, ListPayload};
use crate::timer::DeadlineTimer;
use crate::{
    CancellationToken, ExecutionSnapshot, ExecutionStatus, HandlerId, HeapStats, IngressSender,
    MessageValue, PreemptionToken, Reservation, ResourceAccount, ResourceLevel, Resources,
    ScalarModule, ScalarTask, StackPool,
};

static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
const MAX_PROCESSES: usize = 256;
const MAX_MAILBOX: usize = 64;
const MAX_ARGUMENTS: usize = 64;
const MAX_TIMERS: usize = 256;
const MAX_MONITORS: usize = 256;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Reply(i64),
    ReplyList(crate::IntListReply),
    ReplyBytes(crate::BytesReply),
    ReplyString(crate::StringReply),
    ReplyRecord(crate::RecordReply),
    TimedOut,
    Cancelled,
    Failed,
}

impl RequestOutcome {
    pub fn into_reply_value(
        self,
        reply_type: Option<wings_abi::ValueType>,
    ) -> Result<crate::ReplyValue, Self> {
        match (self, reply_type) {
            (Self::Reply(raw), Some(ty)) => {
                crate::ReplyValue::from_scalar(ty, raw).map_err(|_| Self::Failed)
            }
            (Self::ReplyList(v), Some(wings_abi::ValueType::ListRef)) => {
                Ok(crate::ReplyValue::IntList(v))
            }
            (Self::ReplyBytes(v), Some(wings_abi::ValueType::BytesRef)) => {
                Ok(crate::ReplyValue::Bytes(v))
            }
            (Self::ReplyString(v), Some(wings_abi::ValueType::StringRef)) => {
                Ok(crate::ReplyValue::String(v))
            }
            (Self::ReplyRecord(v), Some(wings_abi::ValueType::RecordRef(_))) => {
                Ok(crate::ReplyValue::Record(v))
            }
            (other, _) => Err(other),
        }
    }
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
    pub byte_state: Option<crate::ByteStateSnapshot>,
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
    timers: Option<Box<ScheduledTimer>>,
    timer_count: usize,
    monitors: Option<Box<MonitorEntry>>,
    monitor_count: usize,
    recent_exits: [(i64, i64); 32],
    recent_exit_cursor: usize,
    pool: StackPool,
    tenant: ResourceAccount,
    modules: Option<(Box<[ScalarModule]>, Reservation)>,
    cursor: usize,
    last_exit: Option<ExitSnapshot>,
    _charge: Reservation,
}

impl Drop for ScalarWorker {
    fn drop(&mut self) {
        while let Some(mut node) = self.timers.take() {
            self.timers = node.next.take();
        }
        while let Some(mut node) = self.monitors.take() {
            self.monitors = node.next.take();
        }
    }
}

struct ScheduledTimer {
    target: Pid,
    deadline: Instant,
    message: Message,
    _node_charge: Reservation,
    next: Option<Box<ScheduledTimer>>,
}

struct MonitorEntry {
    owner: Pid,
    target_capability: i64,
    handler: HandlerId,
    budget: u64,
    reason: Option<i64>,
    message_charge: Reservation,
    _node_charge: Reservation,
    next: Option<Box<MonitorEntry>>,
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
    result_taken: bool,
    publisher: bool,
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
}

struct Message {
    handler: HandlerId,
    arguments: [i64; MAX_ARGUMENTS],
    count: usize,
    max_steps: u64,
    request: Option<RequestId>,
    system: bool,
    external: Option<ReplyPublisher>,
    lists: ListPayload,
    bytes: BytePayload,
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
            preemption: PreemptionToken::new_accounted(tenant)?,
            identity,
            slots,
            requests,
            timers: None,
            timer_count: 0,
            monitors: None,
            monitor_count: 0,
            recent_exits: [(0, 0); 32],
            recent_exit_cursor: 0,
            pool,
            tenant: tenant.clone(),
            modules: None,
            cursor: 0,
            last_exit: None,
            _charge: charge,
        })
    }

    pub fn register_modules(&mut self, modules: &[ScalarModule]) -> Result<(), String> {
        if modules.is_empty() || modules.len() > wings_artifact::MAX_BUNDLE_ACTORS {
            return Err("worker module registry requires 1..=32 modules".into());
        }
        let bytes = std::mem::size_of_val(modules);
        let charge = self.tenant.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(modules.len())
            .map_err(|error| error.to_string())?;
        if owned.capacity() != modules.len() {
            return Err("allocator changed worker module registry capacity".into());
        }
        owned.extend_from_slice(modules);
        self.modules = Some((owned.into_boxed_slice(), charge));
        Ok(())
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
        let parker = crate::WorkerParker::new_accounted(&self.tenant)?;
        self.idle_wait = Some(IdleWait { parker });
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
        let timers_fired = self.flush_timers();
        self.flush_monitors();
        self.flush_completions();
        if self.has_ready() {
            return Ok(WorkerWait::Ready);
        }
        if expired || timers_fired {
            return Ok(WorkerWait::Deadline);
        }
        let mut earliest_timer: Option<Instant> = None;
        let mut timer_ref = self.timers.as_deref();
        while let Some(node) = timer_ref {
            earliest_timer =
                Some(earliest_timer.map_or(node.deadline, |cur| cur.min(node.deadline)));
            timer_ref = node.next.as_deref();
        }
        let deadline = self
            .requests
            .iter()
            .filter_map(|slot| slot.pending.as_ref())
            .filter(|pending| pending.outcome.is_none())
            .map(|pending| pending.deadline)
            .chain(earliest_timer)
            .min()
            .map_or(end, |deadline| deadline.min(end));
        self.idle_wait
            .as_mut()
            .expect("enabled worker wait")
            .parker
            .wait(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| format!("worker wait failed: {error}"))?;
        let expired = self.expire_requests();
        let timers_fired = self.flush_timers();
        self.flush_monitors();
        self.flush_completions();
        Ok(if self.has_ready() {
            WorkerWait::Ready
        } else if expired || timers_fired {
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
        let capability =
            wings_abi::encode_capability(self.identity, index as u32, self.slots[index].generation)
                .ok_or("process capabilities exhausted")?;
        let bytes = std::mem::size_of::<Actor>()
            + mailbox_capacity * std::mem::size_of::<Option<Message>>();
        let charge = self.tenant.reserve(Resources {
            bytes: bytes as u64,
            processes: 1,
            ..Resources::default()
        })?;
        let account = self.tenant.process_accounted(limits)?;
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
            result_taken: false,
            publisher: true,
            _charge: charge,
        });
        Ok(request)
    }

    pub fn send_values(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        max_steps: u64,
    ) -> Result<(), String> {
        let message = self.host_message(pid, handler, values, false)?;
        self.enqueue_payload(
            pid,
            handler,
            &message.arguments[..message.count],
            max_steps,
            None,
            &message.lists,
            message.bytes,
        )
    }

    pub fn request_values(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        max_steps: u64,
        timeout: Duration,
    ) -> Result<RequestId, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("request deadline is out of range")?;
        let message = self.host_message(pid, handler, values, true)?;
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
        let charge = self.actor(pid)?.account.reserve(Resources {
            requests: 1,
            ..Resources::default()
        })?;
        self.enqueue_payload(
            pid,
            handler,
            &message.arguments[..message.count],
            max_steps,
            Some(request),
            &message.lists,
            message.bytes,
        )?;
        self.requests[index].pending = Some(PendingRequest {
            target: pid,
            deadline,
            outcome: None,
            completion: None,
            result_taken: false,
            publisher: true,
            _charge: charge,
        });
        Ok(request)
    }

    fn host_message(
        &self,
        pid: Pid,
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        request: bool,
    ) -> Result<HostMessage, String> {
        let actor = self.actor(pid)?;
        if !Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner)
            || handler.schema().is_completion()
            || handler.schema().reply.is_some() != request
        {
            return Err("host message module or handler mode mismatch".into());
        }
        if actor.mailbox.count == actor.mailbox.slots.len() {
            return Err("mailbox_full".into());
        }
        HostMessage::copy(handler, values, &actor.account)
    }

    pub fn take_reply(&mut self, request: RequestId) -> Result<Option<RequestOutcome>, String> {
        self.expire_requests();
        if self.pending(request)?.result_taken {
            return Err("stale or consumed request ID".into());
        }
        if self.pending(request)?.completion.is_some() {
            return Err("language request results require completion events".into());
        }
        let outcome = self.pending(request)?.outcome.clone();
        if outcome.is_some() {
            let slot = &mut self.requests[request.slot as usize];
            slot.pending
                .as_mut()
                .expect("validated request")
                .result_taken = true;
            retire_request(slot);
        }
        Ok(outcome)
    }

    pub fn cancel_request(&mut self, request: RequestId) -> Result<(), String> {
        self.expire_requests();
        if self.pending(request)?.result_taken {
            return Err("stale or consumed request ID".into());
        }
        if self.pending(request)?.completion.is_some() {
            return Err("language request cancellation requires its owner".into());
        }
        self.pending(request)?;
        self.select_request(request, RequestOutcome::Cancelled);
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
        self.enqueue_payload(
            pid,
            handler,
            arguments,
            max_steps,
            request,
            &ListPayload::default(),
            BytePayload::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_payload(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        lists: &ListPayload,
        bytes: BytePayload,
    ) -> Result<(), String> {
        self.enqueue_payload_kind(
            pid, handler, arguments, max_steps, request, false, lists, bytes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_message_payload(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        allow_pid: bool,
        require_mailbox_slot: bool,
        lists: &ListPayload,
        bytes: BytePayload,
    ) -> Result<Message, String> {
        self.prepare_message_payload_checked(
            pid,
            handler,
            arguments,
            max_steps,
            request,
            allow_pid,
            require_mailbox_slot,
            lists,
            bytes,
        )
        .map_err(String::from)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_message_payload_checked(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        allow_pid: bool,
        require_mailbox_slot: bool,
        lists: &ListPayload,
        mut bytes: BytePayload,
    ) -> Result<Message, crate::VmError> {
        let actor = self.actor_mut(pid).map_err(|_| crate::VmError::StalePid)?;
        if !std::sync::Arc::ptr_eq(&actor.task.module().inner, &handler.module.inner) {
            return Err(crate::VmError::Other(
                "handler ID belongs to a different module",
            ));
        }
        let schema = handler.schema();
        if schema.is_completion() {
            return Err(crate::VmError::Other(
                "completion events cannot be sent directly",
            ));
        }
        if schema.reply.is_some() != request.is_some() {
            return Err(crate::VmError::Other(
                "send requires a one-way handler; request requires a reply handler",
            ));
        }
        crate::message_values::validate_message_payload(
            &schema.parameters,
            arguments,
            false,
            allow_pid,
            lists,
            &bytes,
        )
        .map_err(|_| {
            crate::VmError::InvalidArguments("handler argument count or scalar type mismatch")
        })?;
        if require_mailbox_slot && actor.mailbox.count == actor.mailbox.slots.len() {
            return Err(crate::VmError::MailboxFull);
        }
        bytes.transfer_checked(&actor.account)?;
        let charge = actor.account.reserve_checked(Resources {
            messages: 1,
            bytes: std::mem::size_of::<Message>() as u64,
            ..Resources::default()
        })?;
        let mut owned_arguments = [0; MAX_ARGUMENTS];
        owned_arguments[..arguments.len()].copy_from_slice(arguments);
        Ok(Message {
            handler: handler.clone(),
            arguments: owned_arguments,
            count: arguments.len(),
            max_steps,
            request,
            system: false,
            external: None,
            lists: *lists,
            bytes,
            _charge: charge,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_payload_kind(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        allow_pid: bool,
        lists: &ListPayload,
        bytes: BytePayload,
    ) -> Result<(), String> {
        let message = self.prepare_message_payload(
            pid, handler, arguments, max_steps, request, allow_pid, true, lists, bytes,
        )?;
        let actor = self.actor_mut(pid)?;
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
        self.flush_timers();
        self.flush_monitors();
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
                            lists: envelope.lists,
                            bytes: envelope.bytes,
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
                let message = actor.mailbox.slots[actor.mailbox.head]
                    .as_mut()
                    .expect("mailbox head");
                actor.task.start_kind(
                    &message.handler,
                    &message.arguments[..message.count],
                    message.max_steps,
                    || (CancellationToken::default(), self.preemption.clone()),
                    &actor.pool,
                    message.system,
                    &message.lists,
                    &mut message.bytes,
                )?;
                let message = actor.mailbox.slots[actor.mailbox.head]
                    .take()
                    .expect("started message");
                actor.mailbox.head = (actor.mailbox.head + 1) % actor.mailbox.slots.len();
                actor.mailbox.count -= 1;
                actor.active = Some(ActiveMessage { message });
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
                Ok(ExecutionStatus::CompleteList(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, active.message.external) {
                        (Some(request), None) => {
                            self.finish_request(request, RequestOutcome::ReplyList(reply))
                        }
                        (None, Some(publisher)) => publisher.complete_list(reply),
                        _ => {
                            self.remove(index);
                            return Err("list reply does not match message kind".into());
                        }
                    }
                    Ok(WorkerEvent::Delivered(pid))
                }
                Ok(ExecutionStatus::CompleteBytes(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, active.message.external) {
                        (Some(request), None) => {
                            self.finish_request(request, RequestOutcome::ReplyBytes(reply))
                        }
                        (None, Some(publisher)) => publisher.complete_bytes(reply),
                        _ => {
                            self.remove(index);
                            return Err("byte reply does not match message kind".into());
                        }
                    }
                    Ok(WorkerEvent::Delivered(pid))
                }
                Ok(ExecutionStatus::CompleteString(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, active.message.external) {
                        (Some(request), None) => {
                            self.finish_request(request, RequestOutcome::ReplyString(reply))
                        }
                        (None, Some(publisher)) => publisher.complete_string(reply),
                        _ => {
                            self.remove(index);
                            return Err("string reply does not match message kind".into());
                        }
                    }
                    Ok(WorkerEvent::Delivered(pid))
                }
                Ok(ExecutionStatus::CompleteRecord(reply)) => {
                    let active = self.slots[index]
                        .actor
                        .as_mut()
                        .expect("running actor")
                        .active
                        .take()
                        .expect("completed message");
                    match (active.message.request, active.message.external) {
                        (Some(request), None) => {
                            self.finish_request(request, RequestOutcome::ReplyRecord(reply))
                        }
                        (None, Some(publisher)) => publisher.complete_record(reply),
                        _ => {
                            self.remove(index);
                            return Err("record reply does not match message kind".into());
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
                    let execution_snapshot =
                        actor.task.snapshot().expect("failed invocation snapshot");
                    let exit_reason = execution_snapshot
                        .fault
                        .map_or(i64::from(wings_abi::STATUS_INVALID_CONTROL), |fault| {
                            fault.exit_code()
                        });
                    self.last_exit = Some(ExitSnapshot {
                        pid,
                        execution: execution_snapshot,
                        state_type,
                        committed_state: (!state_type.is_reference())
                            .then(|| actor.task.committed_state()),
                        byte_state: actor.task.byte_state_snapshot(),
                        queued: actor.mailbox.count,
                        resources_before_cleanup: actor.account.used(),
                        handler,
                        handler_len: name.len(),
                    });
                    self.remove_with_reason(index, exit_reason);
                    Ok(WorkerEvent::Exited { pid, reason })
                }
            };
        }
        Ok(WorkerEvent::Idle)
    }

    pub fn stop(&mut self, pid: Pid) -> Result<(), String> {
        self.actor_mut(pid)?;
        self.expire_requests();
        self.remove_with_reason(pid.slot as usize, 0);
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

    pub fn byte_state_snapshot(
        &self,
        pid: Pid,
    ) -> Result<Option<crate::ByteStateSnapshot>, String> {
        let actor = self.actor(pid)?;
        if actor.active.is_some() {
            return Err("byte state snapshot is unavailable during an invocation".into());
        }
        Ok(actor.task.byte_state_snapshot())
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

    fn target_by_capability(&self, capability: i64) -> Option<Pid> {
        let (index, generation) = wings_abi::decode_capability(self.identity, capability)?;
        self.slots
            .get(index)
            .filter(|slot| slot.generation == generation && slot.actor.is_some())
            .map(|_| self.pid(index))
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
            wings_abi::EFFECT_REQUEST | wings_abi::EFFECT_REQUEST_ACTOR => {
                let result = self.language_request(pid, effect);
                match result {
                    Ok(value) => (wings_abi::STATUS_OK, value),
                    Err(_) => (wings_abi::STATUS_REQUEST_REJECTED, 0),
                }
            }
            wings_abi::EFFECT_CANCEL_REQUEST => {
                let request = wings_abi::decode_request_capability(self.identity, effect.target)
                    .and_then(|(index, generation)| {
                        self.requests
                            .get(index)
                            .filter(|slot| slot.generation == generation)
                            .and_then(|slot| slot.pending.as_ref())
                            .and_then(|pending| pending.completion.as_ref())
                            .filter(|completion| {
                                completion.owner == pid && completion.capability == effect.target
                            })
                            .map(|_| RequestId {
                                worker: self.identity,
                                slot: index as u32,
                                generation,
                            })
                    });
                if let Some(request) = request {
                    self.select_request(request, RequestOutcome::Cancelled);
                    (wings_abi::STATUS_OK, 0)
                } else {
                    (wings_abi::STATUS_REQUEST_REJECTED, 0)
                }
            }
            wings_abi::EFFECT_SELF_PID if effect.count == 0 => {
                (wings_abi::STATUS_OK, actor.capability)
            }
            wings_abi::EFFECT_SPAWN_SELF | wings_abi::EFFECT_SPAWN_ACTOR if effect.count == 0 => {
                let limits = actor.account.limit();
                let capacity = actor.mailbox.slots.len();
                let heap_records = actor.heap_records;
                let target_mod = if effect.operation == wings_abi::EFFECT_SPAWN_SELF {
                    Some(module)
                } else {
                    self.modules
                        .as_ref()
                        .and_then(|(mods, _)| mods.get(effect.handler).cloned())
                };
                match target_mod
                    .ok_or_else(|| String::from("unregistered spawn actor"))
                    .and_then(|m| self.spawn_configured(&m, limits, capacity, heap_records))
                {
                    Ok(child) => (
                        wings_abi::STATUS_OK,
                        self.actor(child).expect("spawned actor").capability,
                    ),
                    Err(_) => (wings_abi::STATUS_SPAWN_REJECTED, 0),
                }
            }
            wings_abi::EFFECT_MONITOR => {
                let result: Result<(), crate::VmError> = (|| {
                    if effect.count != 0 || effect.target <= 0 || self.monitor_count >= MAX_MONITORS
                    {
                        return Err(crate::VmError::InvalidArguments(
                            "invalid monitor target or capacity",
                        ));
                    }
                    let handler = module.handler_by_index(effect.handler)?;
                    let schema = handler.schema();
                    if schema.is_completion()
                        || schema.reply.is_some()
                        || schema.parameters.len() != 2
                        || schema.parameters[0].1 != wings_abi::ValueType::Pid
                        || schema.parameters[1].1 != wings_abi::ValueType::Int
                    {
                        return Err(crate::VmError::InvalidHandler);
                    }
                    let owner = self.actor(pid).map_err(|_| crate::VmError::StalePid)?;
                    let message_charge = owner.account.reserve_checked(Resources {
                        messages: 1,
                        bytes: std::mem::size_of::<Message>() as u64,
                        ..Resources::default()
                    })?;
                    let node_charge = owner.account.reserve_checked(Resources {
                        bytes: std::mem::size_of::<MonitorEntry>() as u64,
                        ..Resources::default()
                    })?;
                    let reason = if self.target_by_capability(effect.target).is_some() {
                        None
                    } else {
                        Some(self.lookup_recent_exit(effect.target).unwrap_or(-1))
                    };
                    let entry = Box::new(MonitorEntry {
                        owner: pid,
                        target_capability: effect.target,
                        handler,
                        budget,
                        reason,
                        message_charge,
                        _node_charge: node_charge,
                        next: None,
                    });
                    let mut tail = &mut self.monitors;
                    while let Some(node) = tail {
                        tail = &mut node.next;
                    }
                    *tail = Some(entry);
                    self.monitor_count += 1;
                    if reason.is_some() {
                        self.flush_monitors();
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => (wings_abi::STATUS_OK, 0),
                    Err(_) => (wings_abi::STATUS_REQUEST_REJECTED, 0),
                }
            }
            wings_abi::EFFECT_SEND_SELF
            | wings_abi::EFFECT_SEND
            | wings_abi::EFFECT_SEND_ACTOR
            | wings_abi::EFFECT_SEND_AFTER_SELF
            | wings_abi::EFFECT_SEND_AFTER
            | wings_abi::EFFECT_SEND_AFTER_ACTOR => {
                let is_after = matches!(
                    effect.operation,
                    wings_abi::EFFECT_SEND_AFTER_SELF
                        | wings_abi::EFFECT_SEND_AFTER
                        | wings_abi::EFFECT_SEND_AFTER_ACTOR
                );
                let is_cross_actor = matches!(
                    effect.operation,
                    wings_abi::EFFECT_SEND_ACTOR | wings_abi::EFFECT_SEND_AFTER_ACTOR
                );
                let target = if matches!(
                    effect.operation,
                    wings_abi::EFFECT_SEND_SELF | wings_abi::EFFECT_SEND_AFTER_SELF
                ) {
                    Some(pid)
                } else {
                    self.target_by_capability(effect.target)
                };
                let target_and_handler = if is_cross_actor {
                    let (actor_idx, h_idx) = wings_abi::decode_actor_handler(effect.handler);
                    target.and_then(|target| {
                        let reg_mod = self.modules.as_ref()?.0.get(actor_idx)?;
                        let handler = reg_mod.handler_by_index(h_idx).ok()?;
                        Some((target, handler))
                    })
                } else {
                    target.zip(module.handler_by_index(effect.handler).ok())
                };
                let accepted = if is_after {
                    target_and_handler.is_some_and(|(target, handler)| {
                        self.schedule_timer(
                            target,
                            &handler,
                            effect.timeout_ms,
                            &effect.arguments[..effect.count],
                            budget,
                            &effect.lists,
                            effect.bytes,
                        )
                        .is_ok()
                    })
                } else {
                    target_and_handler.is_some_and(|(target, handler)| {
                        self.enqueue_payload_kind_checked(
                            target,
                            &handler,
                            &effect.arguments[..effect.count],
                            budget,
                            None,
                            true,
                            &effect.lists,
                            effect.bytes,
                        )
                        .is_ok()
                    })
                };
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

    #[allow(clippy::too_many_arguments)]
    fn enqueue_payload_kind_checked(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        request: Option<RequestId>,
        allow_pid: bool,
        lists: &ListPayload,
        bytes: BytePayload,
    ) -> Result<(), crate::VmError> {
        let message = self.prepare_message_payload_checked(
            pid, handler, arguments, max_steps, request, allow_pid, true, lists, bytes,
        )?;
        let actor = self.actor_mut(pid).expect("validated recipient");
        let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
        actor.mailbox.slots[tail] = Some(message);
        actor.mailbox.count += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn schedule_timer(
        &mut self,
        target: Pid,
        handler: &HandlerId,
        delay_ms: i64,
        arguments: &[i64],
        budget: u64,
        lists: &ListPayload,
        bytes: BytePayload,
    ) -> Result<(), crate::VmError> {
        if !(0..=86_400_000).contains(&delay_ms) || self.timer_count >= MAX_TIMERS {
            return Err(crate::VmError::InvalidArguments(
                "invalid timer delay or capacity",
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(delay_ms as u64))
            .ok_or(crate::VmError::InvalidArguments("timer deadline overflow"))?;
        let node_bytes =
            std::mem::size_of::<ScheduledTimer>().saturating_sub(std::mem::size_of::<Message>());
        let node_charge = self
            .actor(target)
            .map_err(|_| crate::VmError::StalePid)?
            .account
            .reserve_checked(Resources {
                bytes: node_bytes as u64,
                ..Resources::default()
            })?;
        let message = self.prepare_message_payload_checked(
            target, handler, arguments, budget, None, true, false, lists, bytes,
        )?;
        let mut cursor = &mut self.timers;
        while cursor
            .as_ref()
            .is_some_and(|existing| existing.deadline <= deadline)
        {
            cursor = &mut cursor.as_mut().expect("timer node").next;
        }
        let next = cursor.take();
        *cursor = Some(Box::new(ScheduledTimer {
            target,
            deadline,
            message,
            _node_charge: node_charge,
            next,
        }));
        self.timer_count += 1;
        if delay_ms == 0 {
            self.flush_timers();
        }
        Ok(())
    }

    fn record_exit(&mut self, capability: i64, reason: i64) {
        let idx = self.recent_exit_cursor % self.recent_exits.len();
        self.recent_exits[idx] = (capability, reason);
        self.recent_exit_cursor = self.recent_exit_cursor.wrapping_add(1);
    }

    fn lookup_recent_exit(&self, capability: i64) -> Option<i64> {
        let len = self.recent_exits.len();
        for offset in 0..len {
            let idx = self.recent_exit_cursor.wrapping_add(len - 1 - offset) % len;
            let (cap, reason) = self.recent_exits[idx];
            if cap == capability && cap > 0 {
                return Some(reason);
            }
        }
        None
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
        self.remove_with_reason(index, i64::from(wings_abi::STATUS_INVALID_CONTROL));
    }

    fn remove_with_reason(&mut self, index: usize, exit_reason: i64) {
        let pid = self.pid(index);
        if let Some(dead_capability) = self.slots[index].actor.as_ref().map(|a| a.capability) {
            self.record_exit(dead_capability, exit_reason);
            let mut timer_cursor = &mut self.timers;
            while timer_cursor.is_some() {
                if timer_cursor.as_ref().expect("timer node").target == pid {
                    let mut removed = timer_cursor.take().expect("removed timer");
                    *timer_cursor = removed.next.take();
                    self.timer_count -= 1;
                    drop(removed);
                } else {
                    timer_cursor = &mut timer_cursor.as_mut().expect("timer node").next;
                }
            }
            let mut mon_cursor = &mut self.monitors;
            while mon_cursor.is_some() {
                if mon_cursor.as_ref().expect("monitor node").owner == pid {
                    let mut removed = mon_cursor.take().expect("removed monitor");
                    *mon_cursor = removed.next.take();
                    self.monitor_count -= 1;
                    drop(removed);
                } else {
                    let entry = mon_cursor.as_mut().expect("monitor node");
                    if entry.target_capability == dead_capability && entry.reason.is_none() {
                        entry.reason = Some(exit_reason);
                    }
                    mon_cursor = &mut entry.next;
                }
            }
        }
        for slot in &mut self.requests {
            if let Some(pending) = &mut slot.pending {
                if pending
                    .completion
                    .as_ref()
                    .is_some_and(|completion| completion.owner == pid)
                {
                    pending.completion.take();
                    pending.result_taken = true;
                }
                if pending.target == pid {
                    pending.publisher = false;
                    if pending.outcome.is_none() {
                        pending.outcome = Some(if Instant::now() >= pending.deadline {
                            RequestOutcome::TimedOut
                        } else {
                            RequestOutcome::Failed
                        });
                    }
                }
            }
            retire_request(slot);
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

    fn select_request(&mut self, request: RequestId, outcome: RequestOutcome) {
        if self.pending(request).is_ok() {
            let pending = self.requests[request.slot as usize]
                .pending
                .as_mut()
                .expect("validated request");
            if pending.outcome.is_none() {
                pending.outcome = Some(if Instant::now() >= pending.deadline {
                    RequestOutcome::TimedOut
                } else {
                    outcome
                });
            }
        }
    }

    fn finish_request(&mut self, request: RequestId, outcome: RequestOutcome) {
        self.select_request(request, outcome);
        if self.pending(request).is_ok() {
            let slot = &mut self.requests[request.slot as usize];
            slot.pending.as_mut().expect("validated request").publisher = false;
            retire_request(slot);
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
    ) -> Result<i64, crate::VmError> {
        if !(0..=86_400_000).contains(&effect.timeout_ms) {
            return Err(crate::VmError::InvalidArguments("invalid request timeout"));
        }
        let actor = self.actor(owner).map_err(|_| crate::VmError::StalePid)?;
        let module = actor.task.module().clone();
        let target = self
            .target_by_capability(effect.target)
            .ok_or(crate::VmError::InvalidCapability)?;
        let (target_module, handler_idx) = if effect.operation == wings_abi::EFFECT_REQUEST_ACTOR {
            let (actor_idx, h_idx) = wings_abi::decode_actor_handler(effect.handler);
            let reg_mod = self
                .modules
                .as_ref()
                .and_then(|(mods, _)| mods.get(actor_idx).cloned())
                .ok_or(crate::VmError::InvalidHandler)?;
            (reg_mod, h_idx)
        } else {
            (module.clone(), effect.handler)
        };
        let handler = target_module.handler_by_index(handler_idx)?;
        let schema = handler.schema();
        if !schema.reply.is_some_and(|ty| ty.is_supported_reply()) {
            return Err(crate::VmError::InvalidHandler);
        }
        let callback = module.handler_by_index(effect.callback)?;
        let valid_callback = match (callback.schema().completion_reply(), schema.reply) {
            (Some(cb_ty), Some(rep_ty)) => wings_abi::equivalent_types(
                &module.manifest().records,
                cb_ty,
                &target_module.manifest().records,
                rep_ty,
            ),
            _ => false,
        };
        if !valid_callback {
            return Err(crate::VmError::InvalidHandler);
        }
        let index = self
            .requests
            .iter()
            .position(|slot| slot.pending.is_none() && slot.generation != 0)
            .ok_or(crate::VmError::RequestTableFull)?;
        let generation = self.requests[index].generation;
        let capability =
            wings_abi::encode_request_capability(self.identity, index as u32, generation as u64)
                .ok_or(crate::VmError::InvalidCapability)?;
        let request = RequestId {
            worker: self.identity,
            slot: index as u32,
            generation,
        };
        let budget = actor
            .active
            .as_ref()
            .expect("request active message")
            .message
            .max_steps;
        let charge = actor.account.reserve_checked(Resources {
            requests: 1,
            ..Resources::default()
        })?;
        let event_charge = actor.account.reserve_checked(Resources {
            messages: 1,
            bytes: std::mem::size_of::<Message>() as u64,
            ..Resources::default()
        })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(effect.timeout_ms as u64))
            .ok_or(crate::VmError::InvalidArguments(
                "request deadline overflow",
            ))?;
        self.enqueue_payload_kind_checked(
            target,
            &handler,
            &effect.arguments[..effect.count],
            budget,
            Some(request),
            true,
            &effect.lists,
            effect.bytes,
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
            result_taken: false,
            publisher: true,
            _charge: charge,
        });
        Ok(capability)
    }

    fn flush_timers(&mut self) -> bool {
        let now = Instant::now();
        let mut fired = false;
        let mut pending = self.timers.take();
        let mut kept_head: Option<Box<ScheduledTimer>> = None;
        let mut kept_tail = &mut kept_head;
        while let Some(mut node) = pending {
            if node.deadline > now {
                *kept_tail = Some(node);
                break;
            }
            pending = node.next.take();
            let target = node.target;
            let can_deliver = match self.actor(target) {
                Ok(actor) => actor.mailbox.count < actor.mailbox.slots.len(),
                Err(_) => {
                    self.timer_count -= 1;
                    drop(node);
                    continue;
                }
            };
            if !can_deliver {
                let slot = kept_tail.insert(node);
                kept_tail = &mut slot.next;
                continue;
            }
            self.timer_count -= 1;
            let actor = self.actor_mut(target).expect("timer recipient");
            let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
            actor.mailbox.slots[tail] = Some(node.message);
            actor.mailbox.count += 1;
            fired = true;
        }
        self.timers = kept_head;
        fired
    }

    fn flush_monitors(&mut self) {
        let mut pending = self.monitors.take();
        let mut kept_head: Option<Box<MonitorEntry>> = None;
        let mut kept_tail = &mut kept_head;
        while let Some(mut entry) = pending {
            pending = entry.next.take();
            let Some(reason) = entry.reason else {
                let slot = kept_tail.insert(entry);
                kept_tail = &mut slot.next;
                continue;
            };
            let owner = entry.owner;
            let can_deliver = match self.actor(owner) {
                Ok(actor) => actor.mailbox.count < actor.mailbox.slots.len(),
                Err(_) => {
                    self.monitor_count -= 1;
                    drop(entry);
                    continue;
                }
            };
            if !can_deliver {
                let slot = kept_tail.insert(entry);
                kept_tail = &mut slot.next;
                continue;
            }
            self.monitor_count -= 1;
            let mut arguments = [0; MAX_ARGUMENTS];
            arguments[0] = entry.target_capability;
            arguments[1] = reason;
            let message = Message {
                handler: entry.handler,
                arguments,
                count: 2,
                max_steps: entry.budget,
                request: None,
                system: false,
                external: None,
                lists: ListPayload::default(),
                bytes: BytePayload::default(),
                _charge: entry.message_charge,
            };
            let actor = self.actor_mut(owner).expect("monitor recipient");
            let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
            actor.mailbox.slots[tail] = Some(message);
            actor.mailbox.count += 1;
        }
        self.monitors = kept_head;
    }

    fn flush_completions(&mut self) {
        for index in 0..self.requests.len() {
            let Some(pending) = self.requests[index].pending.as_ref() else {
                continue;
            };
            let Some(outcome) = pending.outcome.clone() else {
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
            let pending = self.requests[index]
                .pending
                .as_mut()
                .expect("completed request");
            let completion = pending.completion.take().expect("completion route");
            pending.result_taken = true;
            retire_request(&mut self.requests[index]);
            let mut arguments = [0; MAX_ARGUMENTS];
            let mut lists = ListPayload::default();
            let mut bytes = BytePayload::default();
            let mut count = 3;
            if let Some(wings_abi::ValueType::RecordRef(schema)) =
                completion.handler.schema().completion_reply()
            {
                let (reply, status) = match &outcome {
                    RequestOutcome::ReplyRecord(reply) => (Some(reply), 0),
                    RequestOutcome::TimedOut => (None, 1),
                    RequestOutcome::Cancelled => (None, 2),
                    _ => (None, 3),
                };
                let records = &completion.handler.module.manifest().records;
                let response = crate::RecordReply::completion(
                    reply,
                    schema,
                    records,
                    completion.capability,
                    status,
                )
                .or_else(|_| {
                    crate::RecordReply::completion(None, schema, records, completion.capability, 3)
                })
                .expect("validated named record completion");
                arguments = response.arguments;
                count = response.count;
                lists = response.lists;
                bytes = response.bytes;
            } else {
                arguments[0] = completion.capability;
                (arguments[1], arguments[2]) = match outcome {
                    RequestOutcome::Reply(value) => (0, value),
                    RequestOutcome::ReplyList(value) => {
                        lists.count = value.as_slice().len();
                        lists.values[..lists.count].copy_from_slice(value.as_slice());
                        (0, ListPayload::encode(0, lists.count))
                    }
                    RequestOutcome::ReplyBytes(value) => {
                        let length = value.as_slice().len();
                        bytes = BytePayload::completion(Some(value));
                        (0, ListPayload::encode(0, length))
                    }
                    RequestOutcome::ReplyString(value) => {
                        let length = value.as_str().len();
                        bytes = BytePayload::string_completion(Some(value));
                        (0, ListPayload::encode(0, length))
                    }
                    RequestOutcome::ReplyRecord(_) => (3, 0),
                    RequestOutcome::TimedOut => (1, 0),
                    RequestOutcome::Cancelled => (2, 0),
                    RequestOutcome::Failed => (3, 0),
                };
                if completion.handler.schema().completion_reply()
                    == Some(wings_abi::ValueType::ListRef)
                {
                    lists.mask = 4;
                }
                if completion.handler.schema().completion_reply()
                    == Some(wings_abi::ValueType::BytesRef)
                    && bytes.mask == 0
                {
                    bytes = BytePayload::completion(None);
                }
                if completion.handler.schema().completion_reply()
                    == Some(wings_abi::ValueType::StringRef)
                    && bytes.mask == 0
                {
                    bytes = BytePayload::string_completion(None);
                }
            }
            let message = Message {
                handler: completion.handler,
                arguments,
                count,
                max_steps: completion.budget,
                request: None,
                system: true,
                external: None,
                lists,
                bytes,
                _charge: completion.charge,
            };
            let actor = self.actor_mut(owner).expect("completion recipient");
            let tail = (actor.mailbox.head + actor.mailbox.count) % actor.mailbox.slots.len();
            actor.mailbox.slots[tail] = Some(message);
            actor.mailbox.count += 1;
        }
    }
}

fn retire_request(slot: &mut RequestSlot) {
    if slot
        .pending
        .as_ref()
        .is_some_and(|pending| pending.result_taken && !pending.publisher)
    {
        slot.pending.take();
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
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
mod payload_tests {
    use super::*;

    #[test]
    fn worker_control_flag_remains_charged_until_its_last_owner_exits() {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let worker = ScalarWorker::new(&tenant, 1).unwrap();
        let token = worker.preemption.clone();
        drop(worker);
        assert!(tenant.used().bytes > 0);
        token.request();
        drop(token);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn reference_completion_waits_for_mailbox_capacity_and_releases_results_when_owner_exits() {
        for reply_type in [
            wings_abi::ValueType::ListRef,
            wings_abi::ValueType::BytesRef,
            wings_abi::ValueType::StringRef,
        ] {
            for owner_exits in [false, true] {
                let limits = Resources {
                    bytes: 32 << 20,
                    processes: 4,
                    messages: 8,
                    requests: 4,
                };
                let vm = ResourceAccount::vm(limits);
                let tenant = vm.tenant_accounted(limits).unwrap();
                let mut module = crate::tests::module();
                let data = Arc::get_mut(&mut module.inner).unwrap();
                data.manifest.handlers.push(wings_artifact::Handler {
                    name: "DoneList".into(),
                    reply: None,
                    completion: None,
                    parameters: vec![
                        ("id".into(), wings_abi::ValueType::RequestId),
                        ("status".into(), wings_abi::ValueType::Int),
                        ("value".into(), reply_type),
                    ],
                });
                let mut entries = data.entries.to_vec();
                entries.push(if reply_type == wings_abi::ValueType::ListRef {
                    crate::tests::list_completion
                } else if reply_type == wings_abi::ValueType::StringRef {
                    crate::tests::string_completion
                } else {
                    crate::tests::bytes_completion
                });
                data.entries = entries.into_boxed_slice();
                data.manifest.validate().unwrap();
                let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 2).unwrap();
                let owner = worker.spawn_with_heap(&module, limits, 1, 4).unwrap();
                let target = worker.spawn_with_heap(&module, limits, 1, 4).unwrap();
                let target_account = worker.actor(target).unwrap().account.clone();
                let target_baseline = target_account.used();
                let retained = if reply_type == wings_abi::ValueType::ListRef {
                    RequestOutcome::ReplyList(
                        crate::IntListReply::copy(&target_account, &[i64::MIN, i64::MAX]).unwrap(),
                    )
                } else if reply_type == wings_abi::ValueType::StringRef {
                    RequestOutcome::ReplyString(crate::tests::copied_string_reply(
                        &target_account,
                        "中😀",
                    ))
                } else {
                    let mut payload = BytePayload::new(&target_account, 1, 2).unwrap();
                    payload.data.extend_from_slice(&[0, 255]);
                    RequestOutcome::ReplyBytes(
                        crate::BytesReply::from_payload(&target_account, payload).unwrap(),
                    )
                };
                worker
                    .send(owner, &module.handler("Add").unwrap(), &[0], 128)
                    .unwrap();
                let account = worker.actor(owner).unwrap().account.clone();
                worker.requests[0].pending = Some(PendingRequest {
                    target,
                    deadline: Instant::now() + Duration::from_secs(60),
                    outcome: Some(retained.clone()),
                    publisher: false,
                    result_taken: false,
                    completion: Some(Completion {
                        owner,
                        capability: 17,
                        handler: module.handler("DoneList").unwrap(),
                        budget: 128,
                        charge: account
                            .reserve(Resources {
                                messages: 1,
                                bytes: std::mem::size_of::<Message>() as u64,
                                ..Resources::default()
                            })
                            .unwrap(),
                    }),
                    _charge: account
                        .reserve(Resources {
                            requests: 1,
                            ..Resources::default()
                        })
                        .unwrap(),
                });
                let baseline = vm.used();
                worker.flush_completions();
                assert_eq!(vm.used(), baseline);
                assert!(
                    worker.requests[0]
                        .pending
                        .as_ref()
                        .unwrap()
                        .completion
                        .is_some()
                );
                if owner_exits {
                    worker.stop(owner).unwrap();
                } else {
                    let actor = worker.actor_mut(owner).unwrap();
                    actor.mailbox.slots[actor.mailbox.head].take();
                    actor.mailbox.count = 0;
                    worker.flush_completions();
                    let actor = worker.actor_mut(owner).unwrap();
                    let message = actor.mailbox.slots[actor.mailbox.head].take().unwrap();
                    actor.mailbox.count = 0;
                    assert_eq!(
                        message.arguments[..3],
                        [
                            17,
                            0,
                            ListPayload::encode(
                                0,
                                if reply_type == wings_abi::ValueType::StringRef {
                                    7
                                } else {
                                    2
                                }
                            )
                        ]
                    );
                    if reply_type == wings_abi::ValueType::ListRef {
                        assert!(message.lists.valid(&message.arguments[..3], 4));
                        assert_eq!(message.lists.values[..2], [i64::MIN, i64::MAX]);
                    } else if reply_type == wings_abi::ValueType::StringRef {
                        assert!(message.bytes.valid(&message.arguments[..3], 4));
                        assert_eq!(message.bytes.string_mask, 4);
                        assert_eq!(message.bytes.as_slice(), "中😀".as_bytes());
                    } else {
                        assert!(message.bytes.valid(&message.arguments[..3], 4));
                        assert_eq!(message.bytes.as_slice(), [0, 255]);
                        assert!(message.bytes.data.is_empty());
                    }
                }
                assert!(worker.requests[0].pending.is_none());
                assert!(target_account.used().bytes > target_baseline.bytes);
                drop(retained);
                assert_eq!(target_account.used(), target_baseline);
                drop((worker, target_account, account, tenant));
                assert_eq!(vm.used(), Resources::default());
            }
        }
    }

    unsafe extern "C" fn invalid_scalar_reply(
        _state: i64,
        _arguments: *const i64,
        output: *mut wings_abi::ScalarOutput,
        _context: *mut wings_abi::ScalarContext,
    ) -> u32 {
        unsafe {
            *output = wings_abi::ScalarOutput {
                state: 77,
                reply: 2,
            };
        }
        wings_abi::STATUS_OK
    }

    unsafe extern "C" fn scalar_completion(
        _state: i64,
        arguments: *const i64,
        output: *mut wings_abi::ScalarOutput,
        _context: *mut wings_abi::ScalarContext,
    ) -> u32 {
        unsafe {
            *output = wings_abi::ScalarOutput {
                state: *arguments.add(1) * 100 + *arguments.add(2),
                reply: 0,
            };
        }
        wings_abi::STATUS_OK
    }

    #[test]
    fn copy_request_protocol_rejects_mismatches_before_admission_and_canonicalizes_failure() {
        use wings_abi::ValueType;
        for reply in [
            ValueType::Int,
            ValueType::Bool,
            ValueType::Unit,
            ValueType::Pid,
            ValueType::ListRef,
            ValueType::BytesRef,
            ValueType::StringRef,
        ] {
            for callback in [
                ValueType::Int,
                ValueType::Bool,
                ValueType::Unit,
                ValueType::Pid,
                ValueType::ListRef,
                ValueType::BytesRef,
                ValueType::StringRef,
            ] {
                for corrupt in [false, true] {
                    if corrupt
                        && (matches!(reply, ValueType::Int | ValueType::Pid) || callback != reply)
                    {
                        continue;
                    }
                    let limits = Resources {
                        bytes: 32 << 20,
                        processes: 4,
                        messages: 8,
                        requests: 4,
                    };
                    let vm = ResourceAccount::vm(limits);
                    let tenant = vm.tenant_accounted(limits).unwrap();
                    let mut module = crate::tests::module();
                    let data = Arc::get_mut(&mut module.inner).unwrap();
                    data.manifest.handlers[2].reply = Some(reply);
                    data.manifest.handlers.push(wings_artifact::Handler {
                        name: "Completed".into(),
                        reply: None,
                        completion: None,
                        parameters: vec![
                            ("id".into(), ValueType::RequestId),
                            ("status".into(), ValueType::Int),
                            ("value".into(), callback),
                        ],
                    });
                    let mut entries = data.entries.to_vec();
                    if corrupt {
                        entries[2] = invalid_scalar_reply;
                    } else if reply == ValueType::ListRef {
                        entries[2] = crate::tests::list_reply;
                    } else if reply == ValueType::StringRef {
                        entries[2] = crate::tests::string_reply;
                    } else if reply == ValueType::BytesRef {
                        entries[2] = crate::tests::bytes_reply;
                    }
                    entries.push(if callback == ValueType::ListRef {
                        crate::tests::list_completion
                    } else if callback == ValueType::StringRef {
                        crate::tests::string_completion
                    } else if callback == ValueType::BytesRef {
                        crate::tests::bytes_completion
                    } else {
                        scalar_completion
                    });
                    data.entries = entries.into_boxed_slice();
                    data.manifest.validate().unwrap();
                    let mut worker = ScalarWorker::with_request_capacity(&tenant, 2, 2).unwrap();
                    let owner = worker.spawn_with_heap(&module, limits, 1, 4).unwrap();
                    let target = worker.spawn_with_heap(&module, limits, 1, 4).unwrap();
                    worker
                        .send(owner, &module.handler("Add").unwrap(), &[0], 128)
                        .unwrap();
                    let actor = worker.actor_mut(owner).unwrap();
                    let message = actor.mailbox.slots[actor.mailbox.head].take().unwrap();
                    actor.mailbox.count = 0;
                    actor.active = Some(ActiveMessage { message });
                    let baseline = vm.used();
                    let effect = crate::execution::ProcessEffect {
                        operation: wings_abi::EFFECT_REQUEST,
                        target: worker.actor(target).unwrap().capability,
                        handler: 2,
                        arguments: [0; 64],
                        count: 0,
                        callback: 8,
                        timeout_ms: 60000,
                        lists: ListPayload::default(),
                        bytes: BytePayload::default(),
                    };
                    let result = worker.language_request(owner, effect);
                    if callback != reply {
                        assert!(result.is_err());
                        assert_eq!(vm.used(), baseline);
                        assert_eq!(worker.queued(owner).unwrap(), 0);
                        assert_eq!(worker.queued(target).unwrap(), 0);
                    } else {
                        assert!(result.unwrap() > 0);
                        worker.actor_mut(owner).unwrap().active.take();
                        for _ in 0..8 {
                            worker.tick(128).unwrap();
                        }
                        assert_eq!(
                            worker.committed_state(owner).unwrap(),
                            if corrupt { 300 } else { 0 }
                        );
                        if corrupt {
                            assert!(worker.committed_state(target).is_err());
                            let exit = worker.last_exit().unwrap();
                            assert_eq!(exit.committed_state, Some(0));
                            assert_eq!(
                                exit.execution.fault,
                                Some(
                                    if matches!(
                                        reply,
                                        ValueType::ListRef
                                            | ValueType::BytesRef
                                            | ValueType::StringRef
                                    ) {
                                        crate::ExecutionFault::InvalidRoot
                                    } else {
                                        crate::ExecutionFault::InvalidOutput
                                    }
                                )
                            );
                        }
                        assert_eq!(vm.used().requests, 0);
                        assert_eq!(vm.used().messages, 0);
                    }
                    drop((worker, tenant));
                    assert_eq!(vm.used(), Resources::default());
                }
            }
        }
    }

    #[test]
    fn typed_host_queues_keep_copies_and_request_tickets_through_cancel_and_rejection() {
        let limits = Resources {
            bytes: 32 << 20,
            processes: 4,
            messages: 8,
            requests: 4,
        };
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant_accounted(limits).unwrap();
        let mut module = crate::tests::module();
        let manifest = &mut Arc::get_mut(&mut module.inner).unwrap().manifest;
        for index in [0, 1] {
            manifest.handlers[index].parameters =
                vec![("value".into(), wings_abi::ValueType::BytesRef)];
        }
        let mut worker = ScalarWorker::with_request_capacity(&tenant, 1, 1).unwrap();
        let pid = worker
            .spawn_with_heap(
                &module,
                Resources {
                    bytes: 8 << 20,
                    ..limits
                },
                2,
                4,
            )
            .unwrap();
        let store = module.handler("Add").unwrap();
        let query = module.handler("Bump").unwrap();
        let baseline = vm.used();
        for (handler, request) in [
            (&store, true),
            (&query, false),
            (&crate::tests::module().handler("Add").unwrap(), false),
        ] {
            let result = if request {
                worker
                    .request_values(
                        pid,
                        handler,
                        &[MessageValue::Bytes(&[17; 8])],
                        32,
                        Duration::from_secs(60),
                    )
                    .map(|_| ())
            } else {
                worker.send_values(pid, handler, &[MessageValue::Bytes(&[17; 8])], 32)
            };
            assert!(result.is_err());
            assert_eq!(vm.used(), baseline);
        }
        assert!(
            worker
                .send_values(pid, &store, &[MessageValue::Int(0)], 32)
                .is_err()
        );
        assert!(worker.send(pid, &store, &[0], 32).is_err());
        assert_eq!(vm.used(), baseline);
        let account = worker.actor(pid).unwrap().account.clone();
        let fill = account
            .reserve(Resources {
                bytes: account.limit().bytes - account.used().bytes - 8,
                ..Resources::default()
            })
            .unwrap();
        let filled = vm.used();
        assert!(
            worker
                .send_values(pid, &store, &[MessageValue::Bytes(&[17; 8])], 32)
                .is_err()
        );
        assert!(
            worker
                .request_values(
                    pid,
                    &query,
                    &[MessageValue::Bytes(&[17; 8])],
                    32,
                    Duration::from_secs(60)
                )
                .is_err()
        );
        assert_eq!(vm.used(), filled);
        drop((fill, account));
        assert_eq!(vm.used(), baseline);
        let mut source = [17; 8];
        worker
            .send_values(pid, &store, &[MessageValue::Bytes(&source)], 32)
            .unwrap();
        source.fill(99);
        let request = worker
            .request_values(
                pid,
                &query,
                &[MessageValue::Bytes(&source)],
                32,
                Duration::from_secs(60),
            )
            .unwrap();
        worker.cancel_request(request).unwrap();
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Cancelled)
        );
        assert!(worker.take_reply(request).is_err());
        let actor = worker.actor(pid).unwrap();
        assert_eq!(actor.mailbox.slots[0].as_ref().unwrap().bytes.data, [17; 8]);
        assert_eq!(actor.mailbox.slots[1].as_ref().unwrap().bytes.data, [99; 8]);
        assert_eq!(actor.account.used().requests, 1);
        assert_eq!(actor.account.used().messages, 2);
        let full = vm.used();
        assert!(
            worker
                .send_values(pid, &store, &[MessageValue::Bytes(&source)], 32)
                .is_err()
        );
        assert!(
            worker
                .request_values(
                    pid,
                    &query,
                    &[MessageValue::Bytes(&source)],
                    32,
                    Duration::from_secs(60)
                )
                .is_err()
        );
        assert_eq!(vm.used(), full);
        let mailbox = &mut worker.actor_mut(pid).unwrap().mailbox;
        let removed = mailbox.slots[mailbox.head].take();
        mailbox.head = (mailbox.head + 1) % mailbox.slots.len();
        mailbox.count -= 1;
        drop(removed);
        let full_request = vm.used();
        assert_eq!(
            worker
                .request_values(
                    pid,
                    &query,
                    &[MessageValue::Bytes(&source)],
                    32,
                    Duration::from_secs(60)
                )
                .err()
                .unwrap(),
            "request_table_full"
        );
        assert_eq!(vm.used(), full_request);
        worker.stop(pid).unwrap();
        assert_eq!(vm.used().requests, 0);
        let replacement = worker.spawn_with_heap(&module, limits, 2, 4).unwrap();
        assert_ne!(pid, replacement);
        let replaced = vm.used();
        assert!(
            worker
                .send_values(pid, &store, &[MessageValue::Bytes(&source)], 32)
                .is_err()
        );
        assert_eq!(vm.used(), replaced);
        drop((worker, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    fn limits() -> Resources {
        Resources {
            bytes: 16 << 20,
            processes: 4,
            messages: 8,
            requests: 8,
        }
    }

    fn module() -> ScalarModule {
        let mut module = crate::tests::module();
        let manifest = &mut Arc::get_mut(&mut module.inner).unwrap().manifest;
        manifest.handlers[0].parameters = vec![("data".into(), wings_abi::ValueType::BytesRef)];
        manifest.handlers[0].reply = None;
        module
    }

    #[test]
    fn queued_bytes_move_charge_reject_full_and_return_storage_on_stop() {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let source = tenant.process_accounted(limits()).unwrap();
        let module = module();
        let handler = module.handler(&module.manifest().handlers[0].name).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let pid = worker.spawn_with_heap(&module, limits(), 1, 4).unwrap();
        let baseline = worker.resources(pid).unwrap();
        let mut bytes = BytePayload::new(&source, 1, 8).unwrap();
        bytes.data.extend_from_slice(b"abcdefgh");
        let before_publish = vm.used();
        worker
            .enqueue_payload(
                pid,
                &handler,
                &[ListPayload::encode(0, 8)],
                4096,
                None,
                &ListPayload::default(),
                bytes,
            )
            .unwrap();
        assert_eq!(source.used().bytes, 0);
        assert_eq!(
            worker.resources(pid).unwrap().bytes,
            baseline.bytes + 8 + std::mem::size_of::<Message>() as u64
        );
        assert_eq!(
            vm.used().bytes,
            before_publish.bytes + std::mem::size_of::<Message>() as u64
        );
        let full = vm.used();
        let mut rejected = BytePayload::new(&source, 1, 1).unwrap();
        rejected.data.push(9);
        assert!(
            worker
                .enqueue_payload(
                    pid,
                    &handler,
                    &[ListPayload::encode(0, 1)],
                    4096,
                    None,
                    &ListPayload::default(),
                    rejected
                )
                .is_err()
        );
        assert_eq!(vm.used(), full);
        assert_eq!(source.used(), Resources::default());
        let queued = worker.actor(pid).unwrap().mailbox.slots[0]
            .as_ref()
            .unwrap();
        assert_eq!(queued.bytes.data, b"abcdefgh");
        worker.stop(pid).unwrap();
        assert_eq!(tenant.used().messages, 0);
        drop((worker, source, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn rejected_byte_transfer_and_execution_preparation_do_not_lose_payloads() {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let source = tenant.process_accounted(limits()).unwrap();
        let module = module();
        let handler = module.handler(&module.manifest().handlers[0].name).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let mut quota = limits();
        quota.bytes = 1 << 20;
        let pid = worker.spawn_with_heap(&module, quota, 1, 4).unwrap();
        let account = worker.actor(pid).unwrap().account.clone();
        let filler = account
            .reserve(Resources {
                bytes: quota.bytes - account.used().bytes,
                ..Resources::default()
            })
            .unwrap();
        let baseline = vm.used();
        let mut rejected = BytePayload::new(&source, 1, 8).unwrap();
        rejected.data.extend_from_slice(b"abcdefgh");
        assert!(
            worker
                .enqueue_payload(
                    pid,
                    &handler,
                    &[ListPayload::encode(0, 8)],
                    4096,
                    None,
                    &ListPayload::default(),
                    rejected
                )
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        assert_eq!(worker.queued(pid).unwrap(), 0);
        drop(filler);
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let busy = bound.acquire().unwrap();
        let mut retained = BytePayload::new(&account, 1, 8).unwrap();
        retained.data.extend_from_slice(b"abcdefgh");
        let baseline = vm.used();
        let actor = worker.actor_mut(pid).unwrap();
        assert!(
            actor
                .task
                .start_kind(
                    &handler,
                    &[ListPayload::encode(0, 8)],
                    4096,
                    || (CancellationToken::default(), PreemptionToken::default()),
                    &bound,
                    false,
                    &ListPayload::default(),
                    &mut retained
                )
                .is_err()
        );
        assert!(retained.valid(&[ListPayload::encode(0, 8)], 1));
        assert_eq!(vm.used(), baseline);
        assert!(!actor.task.is_active());
        drop(busy);
        actor
            .task
            .start_kind(
                &handler,
                &[ListPayload::encode(0, 8)],
                4096,
                || (CancellationToken::default(), PreemptionToken::default()),
                &bound,
                false,
                &ListPayload::default(),
                &mut retained,
            )
            .unwrap();
        assert_eq!(retained.mask, 0);
        assert!(actor.task.is_active());
        drop((worker, retained, bound, pool, account, source, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[cfg(test)]
mod wait_tests {
    use super::*;

    #[test]
    fn worker_notifier_retains_wait_charge_until_its_last_owner_exits() {
        let limit = Resources {
            bytes: 16 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        worker.enable_wait().unwrap();
        let notifier = worker.idle_wait.as_ref().unwrap().parker.notifier();
        let cloned = notifier.clone();
        drop((worker, notifier));
        assert_eq!(tenant.used().bytes, 4096);
        cloned.notify().unwrap();
        std::thread::spawn(move || drop(cloned)).join().unwrap();
        assert_eq!(tenant.used(), Resources::default());
    }

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
                result_taken: false,
                publisher: false,
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
