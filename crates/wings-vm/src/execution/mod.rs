use std::sync::atomic::Ordering;
use std::time::Instant;

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarEntry, ScalarOutput};

use crate::aot_heap::AotHeap;
use crate::message_values::{BytePayload, ListPayload};
use crate::reply::BufferReply;
use crate::stack_pool::{FiberStack, PooledStack};
use crate::{
    BytesReply, CancellationToken, HandlerId, HeapStats, IntListReply, PreemptionToken, ReplyValue,
    Reservation, ResourceAccount, ResourceLevel, Resources, ScalarModule, ScalarProcess, StackPool,
};

thread_local! {
    static DEFAULT_STACK_POOL: std::cell::RefCell<Option<StackPool>> = const { std::cell::RefCell::new(None) };
}

pub(crate) const INVOCATION_OVERHEAD: u64 = 4352;

type InvocationResult = (
    u32,
    ScalarOutput,
    Option<Box<AotHeap>>,
    Checkpoint,
    Option<IntListReply>,
    Option<BufferReply>,
    Option<crate::RecordReply>,
);
type ExecutionCoroutine = Coroutine<ResumeCommand, Checkpoint, InvocationResult, PooledStack>;
type ReusableCoroutine = Coroutine<FiberCommand, FiberEvent, (), FiberStack>;

mod abi;
mod fibers;
#[cfg(test)]
mod tests;

use abi::{new_coroutine, resume_invocation, send_self_reusable, suspend_reusable};
pub(crate) use abi::{runtime_checkpoint, runtime_checkpoint_batch};
use fibers::{ActiveReusableFiber, FiberCommand, FiberEvent, ReusableControl};
pub(crate) use fibers::{PreparedDirect, WorkerFibers};

enum InvocationBackend {
    OneShot(ExecutionCoroutine),
    Reusable(ActiveReusableFiber),
}

struct Checkpoint {
    remaining_steps: u64,
    heap: Option<HeapStats>,
    effect: Option<ProcessEffect>,
    yield_requested: Option<Instant>,
}

struct InvocationControl<'scope> {
    yielder: &'scope Yielder<ResumeCommand, Checkpoint>,
    preemption: &'scope PreemptionToken,
}

pub(crate) struct ProcessEffect {
    pub operation: u32,
    pub target: i64,
    pub handler: usize,
    pub arguments: [i64; 64],
    pub count: usize,
    pub callback: usize,
    pub timeout_ms: i64,
    pub lists: ListPayload,
    pub bytes: BytePayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPhase {
    Ready,
    Suspended,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionFault {
    Overflow,
    DivisionByZero,
    Cancelled,
    BudgetExhausted,
    InvalidControl,
    StackExhausted,
    HeapExhausted,
    InvalidRoot,
    InvalidOutput,
    EmptyList,
    InvalidBytes,
    InvalidString,
    InvalidStringIndex,
    ReplyRejected,
    SendRejected,
    SpawnRejected,
    RequestRejected,
    UnknownStatus(u32),
}

impl ExecutionFault {
    fn from_status(status: u32) -> Option<Self> {
        Some(match status {
            STATUS_OK => return None,
            wings_abi::STATUS_OVERFLOW => Self::Overflow,
            wings_abi::STATUS_DIVISION_BY_ZERO => Self::DivisionByZero,
            STATUS_CANCELLED => Self::Cancelled,
            wings_abi::STATUS_BUDGET_EXHAUSTED => Self::BudgetExhausted,
            wings_abi::STATUS_INVALID_CONTROL => Self::InvalidControl,
            wings_abi::STATUS_STACK_EXHAUSTED => Self::StackExhausted,
            wings_abi::STATUS_HEAP_EXHAUSTED => Self::HeapExhausted,
            wings_abi::STATUS_INVALID_ROOT => Self::InvalidRoot,
            wings_abi::STATUS_EMPTY_LIST => Self::EmptyList,
            wings_abi::STATUS_INVALID_BYTES => Self::InvalidBytes,
            wings_abi::STATUS_INVALID_STRING => Self::InvalidString,
            wings_abi::STATUS_INVALID_STRING_INDEX => Self::InvalidStringIndex,
            wings_abi::STATUS_REPLY_REJECTED => Self::ReplyRejected,
            wings_abi::STATUS_SEND_REJECTED => Self::SendRejected,
            wings_abi::STATUS_SPAWN_REJECTED => Self::SpawnRejected,
            wings_abi::STATUS_REQUEST_REJECTED => Self::RequestRejected,
            other => Self::UnknownStatus(other),
        })
    }

    pub(crate) fn exit_code(self) -> i64 {
        match self {
            Self::Overflow => i64::from(wings_abi::STATUS_OVERFLOW),
            Self::DivisionByZero => i64::from(wings_abi::STATUS_DIVISION_BY_ZERO),
            Self::Cancelled => i64::from(STATUS_CANCELLED),
            Self::BudgetExhausted => i64::from(wings_abi::STATUS_BUDGET_EXHAUSTED),
            Self::InvalidControl | Self::InvalidOutput => {
                i64::from(wings_abi::STATUS_INVALID_CONTROL)
            }
            Self::StackExhausted => i64::from(wings_abi::STATUS_STACK_EXHAUSTED),
            Self::HeapExhausted => i64::from(wings_abi::STATUS_HEAP_EXHAUSTED),
            Self::InvalidRoot => i64::from(wings_abi::STATUS_INVALID_ROOT),
            Self::EmptyList => i64::from(wings_abi::STATUS_EMPTY_LIST),
            Self::InvalidBytes => i64::from(wings_abi::STATUS_INVALID_BYTES),
            Self::InvalidString => i64::from(wings_abi::STATUS_INVALID_STRING),
            Self::InvalidStringIndex => i64::from(wings_abi::STATUS_INVALID_STRING_INDEX),
            Self::ReplyRejected => i64::from(wings_abi::STATUS_REPLY_REJECTED),
            Self::SendRejected => i64::from(wings_abi::STATUS_SEND_REJECTED),
            Self::SpawnRejected => i64::from(wings_abi::STATUS_SPAWN_REJECTED),
            Self::RequestRejected => i64::from(wings_abi::STATUS_REQUEST_REJECTED),
            Self::UnknownStatus(other) => i64::from(other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    pub phase: ExecutionPhase,
    pub fault: Option<ExecutionFault>,
    pub max_steps: u64,
    pub remaining_steps: u64,
    pub heap: Option<HeapStats>,
}

impl ExecutionSnapshot {
    pub fn consumed_steps(&self) -> u64 {
        self.max_steps.saturating_sub(self.remaining_steps)
    }
}

pub(crate) fn default_stack_pool() -> Result<StackPool, String> {
    DEFAULT_STACK_POOL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(StackPool::new(64, 2)?);
        }
        Ok(slot.as_ref().expect("initialized stack pool").clone())
    })
}

pub(crate) fn initialize(
    entry: ScalarEntry,
    pool: &StackPool,
    mut heap: Option<Box<AotHeap>>,
) -> Result<(u32, ScalarOutput, Option<Box<AotHeap>>), String> {
    let _charge = reserve_control(pool)?;
    let mut coroutine = new_coroutine(
        entry,
        0,
        InvocationArgs::from_slice(&[], false)?,
        wings_abi::DEFAULT_MAX_STEPS,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        pool.acquire()?,
        &mut heap,
        false,
        ListPayload::default(),
        &mut BytePayload::default(),
        None,
        [wings_abi::ValueType::Unit; 2],
    )?;
    match coroutine.resume(ResumeCommand::Run(u64::MAX)) {
        CoroutineResult::Return((status, output, heap, _, _, _, _)) => Ok((status, output, heap)),
        CoroutineResult::Yield(_) => {
            if !matches!(
                coroutine.resume(ResumeCommand::Cancel),
                CoroutineResult::Return(_)
            ) {
                std::process::abort();
            }
            Err("scalar initializer unexpectedly yielded".into())
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionStatus {
    Suspended,
    Complete(Option<i64>),
    CompleteList(IntListReply),
    CompleteBytes(BytesReply),
    CompleteString(crate::StringReply),
    CompleteRecord(crate::RecordReply),
}

impl ExecutionStatus {
    pub fn into_reply_value(
        self,
        reply_type: Option<wings_abi::ValueType>,
    ) -> Result<Option<ReplyValue>, &'static str> {
        match (self, reply_type) {
            (Self::Suspended, _) => Err("execution is still suspended"),
            (Self::Complete(None), None) => Ok(None),
            (Self::Complete(Some(raw)), Some(ty)) => ReplyValue::from_scalar(ty, raw).map(Some),
            (Self::CompleteList(v), Some(wings_abi::ValueType::ListRef)) => {
                Ok(Some(ReplyValue::IntList(v)))
            }
            (Self::CompleteBytes(v), Some(wings_abi::ValueType::BytesRef)) => {
                Ok(Some(ReplyValue::Bytes(v)))
            }
            (Self::CompleteString(v), Some(wings_abi::ValueType::StringRef)) => {
                Ok(Some(ReplyValue::String(v)))
            }
            (Self::CompleteRecord(v), Some(wings_abi::ValueType::RecordRef(_))) => {
                Ok(Some(ReplyValue::Record(v)))
            }
            _ => Err("execution reply does not match handler schema"),
        }
    }
}

enum ResumeCommand {
    Run(u64),
    Effect(u32, i64, u64),
    Cancel,
}

pub struct ScalarExecution<'process> {
    process: &'process mut ScalarProcess,
    invocation: Option<Invocation>,
    last: Option<ExecutionSnapshot>,
}

pub struct ScalarTask {
    invocation: Option<Invocation>,
    process: ScalarProcess,
    last: Option<ExecutionSnapshot>,
    effects_enabled: bool,
}

struct Invocation {
    handler: usize,
    backend: Option<InvocationBackend>,
    snapshot: ExecutionSnapshot,
    effect: Option<ProcessEffect>,
    acknowledgement: Option<(u32, i64)>,
    preemption: PreemptionToken,
    _charge: Option<Reservation>,
}

impl ScalarProcess {
    pub fn start_controlled(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
    ) -> Result<ScalarExecution<'_>, String> {
        let handler = self.validate_invocation(name, arguments)?;
        if self.module.manifest().handlers[handler].reply == Some(wings_abi::ValueType::Pid) {
            return Err("Pid reply requires an actor worker or scheduler".into());
        }
        self.start_index(handler, arguments, max_steps, || {
            (cancellation.clone(), preemption.clone())
        })
    }

    pub(crate) fn start_index(
        &mut self,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        controls: impl FnOnce() -> (CancellationToken, PreemptionToken),
    ) -> Result<ScalarExecution<'_>, String> {
        let pool = match self.pool.as_ref() {
            Some(pool) => pool.clone(),
            None => default_stack_pool()?,
        };
        let invocation = Invocation::new(
            self,
            handler,
            arguments,
            false,
            max_steps,
            controls,
            &pool,
            false,
            &ListPayload::default(),
            &mut BytePayload::default(),
        )?;
        Ok(ScalarExecution {
            process: self,
            invocation: Some(invocation),
            last: None,
        })
    }

    pub fn start_in_pool(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
    ) -> Result<ScalarExecution<'_>, String> {
        let handler = self.validate_invocation(name, arguments)?;
        if self.module.manifest().handlers[handler].reply == Some(wings_abi::ValueType::Pid) {
            return Err("Pid reply requires an actor worker or scheduler".into());
        }
        let invocation = Invocation::new(
            self,
            handler,
            arguments,
            false,
            max_steps,
            || (cancellation.clone(), preemption.clone()),
            pool,
            false,
            &ListPayload::default(),
            &mut BytePayload::default(),
        )?;
        Ok(ScalarExecution {
            process: self,
            invocation: Some(invocation),
            last: None,
        })
    }
}

impl ScalarTask {
    pub fn new(process: ScalarProcess) -> Self {
        Self {
            invocation: None,
            process,
            last: None,
            effects_enabled: false,
        }
    }

    pub(crate) fn enable_effects(&mut self) {
        self.effects_enabled = true;
    }

    pub(crate) fn take_effect(&mut self) -> Option<ProcessEffect> {
        self.invocation
            .as_mut()
            .and_then(|active| active.effect.take())
    }

    pub(crate) fn acknowledge_effect(&mut self, status: u32, value: i64) {
        self.invocation
            .as_mut()
            .expect("active effect invocation")
            .acknowledgement = Some((status, value));
    }

    pub fn module(&self) -> &ScalarModule {
        self.process.module()
    }

    pub fn is_active(&self) -> bool {
        self.invocation.is_some()
    }

    pub fn snapshot(&self) -> Option<ExecutionSnapshot> {
        self.invocation
            .as_ref()
            .map(|active| active.snapshot)
            .or(self.last)
    }

    pub fn heap_stats(&self) -> Option<HeapStats> {
        match self.invocation.as_ref() {
            Some(active) => active.snapshot.heap,
            None => self.process.heap_stats(),
        }
    }

    pub(crate) fn committed_state(&self) -> i64 {
        self.process.state
    }

    pub(crate) fn byte_state_snapshot(&self) -> Option<crate::ByteStateSnapshot> {
        if self.process.module.manifest().state != wings_abi::ValueType::BytesRef {
            return None;
        }
        self.process.heap.as_deref()?.byte_state_snapshot()
    }

    pub fn start(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
    ) -> Result<(), String> {
        self.start_kind(
            handler,
            arguments,
            max_steps,
            || (cancellation.clone(), preemption.clone()),
            pool,
            false,
            &ListPayload::default(),
            &mut BytePayload::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_kind(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        controls: impl FnOnce() -> (CancellationToken, PreemptionToken),
        pool: &StackPool,
        system: bool,
        lists: &ListPayload,
        bytes: &mut BytePayload,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("scalar task already has an active invocation".into());
        }
        let handler = self.process.validate_handler_payload_internal(
            handler,
            arguments,
            system,
            self.effects_enabled,
            lists,
            bytes,
        )?;
        self.invocation = Some(Invocation::new(
            &mut self.process,
            handler,
            arguments,
            false,
            max_steps,
            controls,
            pool,
            self.effects_enabled,
            lists,
            bytes,
        )?);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_scheduled(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        controls: impl FnOnce() -> (CancellationToken, PreemptionToken),
        pool: &StackPool,
        lists: &ListPayload,
        bytes: &mut BytePayload,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("scalar task already has an active invocation".into());
        }
        if self.process.failed {
            return Err("scalar process has failed and must be restarted".into());
        }
        if !std::sync::Arc::ptr_eq(&self.process.module.inner, &handler.module.inner) {
            return Err("handler belongs to a different module".into());
        }
        self.invocation = Some(Invocation::new(
            &mut self.process,
            handler.index,
            arguments,
            true,
            max_steps,
            controls,
            pool,
            self.effects_enabled,
            lists,
            bytes,
        )?);
        Ok(())
    }

    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        resume_invocation(
            &mut self.process,
            &mut self.invocation,
            &mut self.last,
            max_slice_steps,
            None,
        )
    }

    pub(crate) fn resume_with_fibers(
        &mut self,
        max_slice_steps: u64,
        fibers: &mut WorkerFibers,
    ) -> Result<ExecutionStatus, String> {
        resume_invocation(
            &mut self.process,
            &mut self.invocation,
            &mut self.last,
            max_slice_steps,
            Some(fibers),
        )
    }

    pub fn into_process(mut self) -> ScalarProcess {
        if self.invocation.is_some() {
            self.process.failed = true;
            self.invocation.take();
        }
        self.process
    }
}

enum InvocationArgs {
    Inline([i64; 8], u8),
    Heap(Vec<i64>),
}

impl InvocationArgs {
    fn from_slice(arguments: &[i64], inline_small: bool) -> Result<Self, String> {
        if inline_small && arguments.len() <= 8 {
            let mut inline = [0i64; 8];
            inline[..arguments.len()].copy_from_slice(arguments);
            return Ok(Self::Inline(inline, arguments.len() as u8));
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(arguments.len())
            .map_err(|error| error.to_string())?;
        owned.extend_from_slice(arguments);
        Ok(Self::Heap(owned))
    }

    fn as_mut_slice(&mut self) -> &mut [i64] {
        match self {
            Self::Inline(values, len) => &mut values[..usize::from(*len)],
            Self::Heap(values) => values.as_mut_slice(),
        }
    }

    fn as_ptr(&self) -> *const i64 {
        match self {
            Self::Inline(values, _) => values.as_ptr(),
            Self::Heap(values) => values.as_ptr(),
        }
    }
}

impl Invocation {
    #[allow(clippy::too_many_arguments)]
    fn new(
        process: &mut ScalarProcess,
        handler: usize,
        arguments: &[i64],
        inline_small: bool,
        max_steps: u64,
        controls: impl FnOnce() -> (CancellationToken, PreemptionToken),
        pool: &StackPool,
        effects_enabled: bool,
        lists: &ListPayload,
        bytes: &mut BytePayload,
    ) -> Result<Self, String> {
        process.module.validate_pool(pool)?;
        if process
            .account
            .as_ref()
            .is_some_and(|account| !pool.is_for_process(account))
        {
            return Err(
                "scalar process execution requires its original process-accounted stack pool"
                    .into(),
            );
        }
        if process
            .heap
            .as_ref()
            .is_some_and(|heap| !pool.is_for_process(&heap.account))
        {
            return Err("AOT heap execution requires its process-accounted stack pool".into());
        }
        let charge = reserve_control(pool)?;
        let snapshot = ExecutionSnapshot {
            phase: ExecutionPhase::Ready,
            fault: None,
            max_steps,
            remaining_steps: max_steps,
            heap: process.heap_stats(),
        };
        let stack = pool.acquire()?;
        let owned_arguments = InvocationArgs::from_slice(arguments, inline_small)?;
        let (cancellation, preemption) = controls();
        let coroutine = new_coroutine(
            process.module.inner.entries[handler],
            process.state,
            owned_arguments,
            max_steps,
            &cancellation,
            &preemption,
            stack,
            &mut process.heap,
            effects_enabled,
            *lists,
            bytes,
            Some(process.module.clone()),
            [
                process.module.manifest().state,
                process.module.manifest().handlers[handler]
                    .reply
                    .unwrap_or(wings_abi::ValueType::Unit),
            ],
        )?;
        Ok(Self {
            handler,
            backend: Some(InvocationBackend::OneShot(coroutine)),
            snapshot,
            effect: None,
            acknowledgement: None,
            preemption,
            _charge: charge,
        })
    }
}

pub(super) fn reserve_control(pool: &StackPool) -> Result<Option<Reservation>, String> {
    pool.account()
        .map(|account| {
            if account.level() != ResourceLevel::Process {
                return Err("accounted stack pool must be bound to a process".into());
            }
            account.reserve(Resources {
                bytes: INVOCATION_OVERHEAD,
                ..Resources::default()
            })
        })
        .transpose()
}

impl ScalarExecution<'_> {
    pub fn snapshot(&self) -> ExecutionSnapshot {
        self.invocation
            .as_ref()
            .map(|active| active.snapshot)
            .or(self.last)
            .expect("started execution snapshot")
    }

    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        resume_invocation(
            self.process,
            &mut self.invocation,
            &mut self.last,
            max_slice_steps,
            None,
        )
    }
}

impl Drop for ScalarExecution<'_> {
    fn drop(&mut self) {
        if self.invocation.is_some() {
            self.process.failed = true;
        }
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        if let InvocationBackend::OneShot(coroutine) = backend {
            if coroutine.started()
                && !matches!(
                    coroutine.resume(ResumeCommand::Cancel),
                    CoroutineResult::Return(_)
                )
            {
                std::process::abort();
            }
        }
    }
}
