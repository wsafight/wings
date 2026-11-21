use std::sync::atomic::Ordering;
use std::time::Instant;

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarEntry, ScalarOutput};

use crate::aot_heap::AotHeap;
use crate::message_values::{BytePayload, ListPayload};
use crate::stack_pool::{FiberStack, PooledStack};
use crate::string_reply::BufferReply;
use crate::{
    BytesReply, CancellationToken, HandlerId, HeapStats, IntListReply, PreemptionToken,
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

struct FiberStart {
    entry: ScalarEntry,
    state: i64,
    arguments: *const i64,
    argument_count: usize,
    max_steps: u64,
    slice_remaining: Option<u64>,
    cancellation: *const std::sync::atomic::AtomicU32,
    heap: Option<Box<AotHeap>>,
    effects_enabled: bool,
}

enum FiberCommand {
    Start(FiberStart),
    Run(u64),
    Effect(u32, i64, u64),
    Cancel,
    Shutdown,
}

enum FiberEvent {
    Primed,
    Suspended(Checkpoint),
    Complete {
        status: u32,
        output: ScalarOutput,
        heap: Option<Box<AotHeap>>,
        checkpoint: Checkpoint,
    },
}

struct ReusableControl<'scope> {
    yielder: &'scope Yielder<FiberCommand, FiberEvent>,
    preemption: &'scope PreemptionToken,
}

struct ParkedFiber {
    coroutine: Option<ReusableCoroutine>,
    stack_charge: Option<Reservation>,
    pool: StackPool,
}

impl Drop for ParkedFiber {
    fn drop(&mut self) {
        if let Some(mut coroutine) = self.coroutine.take() {
            if coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Shutdown),
                    CoroutineResult::Return(())
                )
            {
                std::process::abort();
            }
            self.pool.drop_parked_fiber();
        }
    }
}

struct ActiveReusableFiber {
    coroutine: Option<ReusableCoroutine>,
    stack_charge: Option<Reservation>,
    pool: StackPool,
    _cancellation: Option<CancellationToken>,
}

impl Drop for ActiveReusableFiber {
    fn drop(&mut self) {
        if let Some(mut coroutine) = self.coroutine.take() {
            if self._cancellation.take().is_some()
                && coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Cancel),
                    CoroutineResult::Yield(FiberEvent::Complete { .. })
                )
            {
                std::process::abort();
            }
            if coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Shutdown),
                    CoroutineResult::Return(())
                )
            {
                std::process::abort();
            }
            self.pool.drop_active_fiber();
        }
    }
}

pub(crate) struct WorkerFibers {
    pool: StackPool,
    preemption: PreemptionToken,
    slots: [Option<ParkedFiber>; 2],
    limit: usize,
    native_stack_limit: usize,
}

enum DirectFiberLease {
    Parked(ParkedFiber),
    Demand {
        pool: StackPool,
        lease: crate::stack_pool::DirectStackLease,
    },
}

pub(crate) struct PreparedDirect {
    stack_lease: Option<DirectFiberLease>,
    _control_charge: Reservation,
    native_stack_limit: usize,
}

pub(crate) struct PreparedFiber {
    active: ActiveReusableFiber,
    control_charge: Reservation,
}

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
    pub record_mask: u64,
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
        0,
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
            0,
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
        let invocation = Invocation::new(
            self,
            handler,
            arguments,
            false,
            max_steps,
            || (cancellation.clone(), preemption.clone()),
            pool,
            false,
            0,
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
            0,
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
        record_mask: u64,
        lists: &ListPayload,
        bytes: &mut BytePayload,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("scalar task already has an active invocation".into());
        }
        let handler = self.process.validate_handler_payload(
            handler,
            arguments,
            system,
            record_mask,
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
            record_mask,
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
        record_mask: u64,
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
            record_mask,
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
        record_mask: u64,
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
            record_mask,
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

impl WorkerFibers {
    pub(crate) fn new(
        pool: &StackPool,
        preemption: &PreemptionToken,
        native_stack_limit: usize,
    ) -> Self {
        Self {
            pool: pool.clone(),
            preemption: preemption.clone(),
            slots: [None, None],
            limit: pool.max_cached_slots().min(2),
            native_stack_limit,
        }
    }

    pub(crate) fn can_run_direct(
        &self,
        process: &ScalarProcess,
        handler: &HandlerId,
        max_steps: u64,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        !yield_observed
            && self.native_stack_limit != 0
            && self.preemption.flag.load(Ordering::Acquire) == 0
            && handler
                .direct_max_steps(process.heap.as_deref())
                .is_some_and(|bound| bound <= max_steps && bound <= slice_steps)
    }

    pub(crate) fn can_run_direct_image(
        &self,
        image: &crate::DetachedProcess,
        handler: &HandlerId,
        max_steps: u64,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        !yield_observed
            && !image.is_failed()
            && self.native_stack_limit != 0
            && self.preemption.flag.load(Ordering::Acquire) == 0
            && handler
                .direct_max_steps(image.heap_ref())
                .is_some_and(|bound| bound <= max_steps && bound <= slice_steps)
    }

    pub(crate) fn prepare_direct_account(
        &mut self,
        failed: bool,
        process_account: &ResourceAccount,
    ) -> Result<PreparedDirect, String> {
        if failed {
            return Err("scalar process has failed and must be restarted".into());
        }
        if process_account.level() != ResourceLevel::Process {
            return Err("accounted stack pool must be bound to a process".into());
        }
        let control_charge = process_account.reserve(Resources {
            bytes: INVOCATION_OVERHEAD,
            ..Resources::default()
        })?;
        for slot in &mut self.slots[..self.limit] {
            if let Some(mut parked) = slot.take() {
                let charge_ref = parked
                    .stack_charge
                    .as_mut()
                    .expect("parked fiber stack charge");
                match self.pool.activate_parked_fiber(charge_ref, process_account) {
                    Ok(()) => {
                        return Ok(PreparedDirect {
                            stack_lease: Some(DirectFiberLease::Parked(parked)),
                            _control_charge: control_charge,
                            native_stack_limit: self.native_stack_limit,
                        });
                    }
                    Err(error) => {
                        *slot = Some(parked);
                        return Err(error);
                    }
                }
            }
        }
        let lease = self.pool.acquire_direct_stack_charge(process_account)?;
        Ok(PreparedDirect {
            stack_lease: Some(DirectFiberLease::Demand {
                pool: self.pool.clone(),
                lease,
            }),
            _control_charge: control_charge,
            native_stack_limit: self.native_stack_limit,
        })
    }

    pub(crate) fn prepare_direct(
        &mut self,
        process: &ScalarProcess,
        process_account: &ResourceAccount,
    ) -> Result<PreparedDirect, String> {
        self.prepare_direct_account(process.failed, process_account)
    }

    pub(crate) fn can_run_fast(
        handler: &HandlerId,
        record_mask: u64,
        lists: &ListPayload,
        bytes: &BytePayload,
    ) -> bool {
        record_mask == 0
            && lists.mask == 0
            && lists.count == 0
            && bytes.is_trivially_empty()
            && !matches!(
                handler.schema().reply,
                Some(
                    wings_abi::ValueType::ListRef
                        | wings_abi::ValueType::BytesRef
                        | wings_abi::ValueType::StringRef
                        | wings_abi::ValueType::RecordRef(_)
                )
            )
    }

    pub(crate) fn prepare(
        &mut self,
        process: &ScalarProcess,
        process_account: &ResourceAccount,
    ) -> Result<PreparedFiber, String> {
        if process.failed {
            return Err("scalar process has failed and must be restarted".into());
        }
        if process_account.level() != ResourceLevel::Process {
            return Err("accounted stack pool must be bound to a process".into());
        }
        let control_charge = process_account.reserve(Resources {
            bytes: INVOCATION_OVERHEAD,
            ..Resources::default()
        })?;
        for slot in &mut self.slots[..self.limit] {
            if let Some(mut parked) = slot.take() {
                let charge_ref = parked
                    .stack_charge
                    .as_mut()
                    .expect("parked fiber stack charge");
                match self.pool.activate_parked_fiber(charge_ref, process_account) {
                    Ok(()) => {
                        let coroutine = parked.coroutine.take().expect("parked fiber coroutine");
                        let stack_charge =
                            parked.stack_charge.take().expect("activated stack charge");
                        return Ok(PreparedFiber {
                            active: ActiveReusableFiber {
                                coroutine: Some(coroutine),
                                stack_charge: Some(stack_charge),
                                pool: self.pool.clone(),
                                _cancellation: None,
                            },
                            control_charge,
                        });
                    }
                    Err(error) => {
                        *slot = Some(parked);
                        return Err(error);
                    }
                }
            }
        }
        let (stack, stack_charge) = self.pool.allocate_fiber_stack(process_account)?;
        let coroutine = new_reusable_coroutine(stack, self.preemption.clone());
        Ok(PreparedFiber {
            active: ActiveReusableFiber {
                coroutine: Some(coroutine),
                stack_charge: Some(stack_charge),
                pool: self.pool.clone(),
                _cancellation: None,
            },
            control_charge,
        })
    }

    fn recycle(
        &mut self,
        mut coroutine: ReusableCoroutine,
        mut stack_charge: Reservation,
        pool: StackPool,
    ) {
        if self.limit > 0 && pool.park_active_fiber(&mut stack_charge) {
            for slot in &mut self.slots[..self.limit] {
                if slot.is_none() {
                    *slot = Some(ParkedFiber {
                        coroutine: Some(coroutine),
                        stack_charge: Some(stack_charge),
                        pool,
                    });
                    return;
                }
            }
            drop(ParkedFiber {
                coroutine: Some(coroutine),
                stack_charge: Some(stack_charge),
                pool,
            });
            return;
        }
        if coroutine.started()
            && !coroutine.done()
            && !matches!(
                coroutine.resume(FiberCommand::Shutdown),
                CoroutineResult::Return(())
            )
        {
            std::process::abort();
        }
    }
}

impl Drop for PreparedDirect {
    fn drop(&mut self) {
        if let Some(lease) = self.stack_lease.take() {
            match lease {
                DirectFiberLease::Parked(mut parked) => {
                    if let Some(mut coroutine) = parked.coroutine.take() {
                        if coroutine.started()
                            && !coroutine.done()
                            && !matches!(
                                coroutine.resume(FiberCommand::Shutdown),
                                CoroutineResult::Return(())
                            )
                        {
                            std::process::abort();
                        }
                    }
                    parked.pool.drop_active_fiber();
                }
                DirectFiberLease::Demand { pool, lease } => {
                    pool.release_direct_stack_charge(lease);
                }
            }
        }
    }
}

static DIRECT_NEVER_YIELD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

unsafe extern "C" fn direct_unexpected_slow(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    if unsafe { &*context.cancellation }.load(Ordering::Acquire) != 0 {
        return STATUS_CANCELLED;
    }
    if context.remaining_steps == 0 {
        return wings_abi::STATUS_BUDGET_EXHAUSTED;
    }
    wings_abi::STATUS_INVALID_CONTROL
}

impl PreparedDirect {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        mut self,
        fibers: &mut WorkerFibers,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        slice_steps: u64,
        cancellation: &CancellationToken,
    ) -> (
        ScalarProcess,
        ExecutionSnapshot,
        Result<ExecutionStatus, String>,
    ) {
        let mut output = ScalarOutput::default();
        let mut heap = process.heap.take();
        let mut context = ScalarContext {
            cancellation: &*cancellation.flag,
            remaining_steps: max_steps,
            yield_request: &DIRECT_NEVER_YIELD,
            slice_remaining: slice_steps,
            slow_path: Some(direct_unexpected_slow),
            data: std::ptr::null_mut(),
            stack_limit: self.native_stack_limit,
            roots: std::ptr::null(),
            heap_call: heap
                .as_ref()
                .map(|_| crate::aot_heap::heap_call as wings_abi::HeapCall),
            heap_data: heap.as_mut().map_or(std::ptr::null_mut(), |heap| {
                std::ptr::from_mut(&mut **heap).cast()
            }),
            effect_call: None,
        };
        let entry = process.module.inner.entries[handler];
        let status = unsafe { entry(process.state, arguments.as_ptr(), &mut output, &mut context) };
        let status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        if let Some(heap_ref) = &mut heap {
            heap_ref.clear_incoming();
        }
        let remaining_steps = context.remaining_steps;
        let heap_stats = heap.as_deref().map(AotHeap::stats);
        if let Some(lease) = self.stack_lease.take() {
            match lease {
                DirectFiberLease::Parked(mut parked) => {
                    let coroutine = parked.coroutine.take().expect("parked fiber coroutine");
                    let stack_charge = parked
                        .stack_charge
                        .take()
                        .expect("activated parked stack charge");
                    let pool = parked.pool.clone();
                    fibers.recycle(coroutine, stack_charge, pool);
                }
                DirectFiberLease::Demand { pool, lease } => {
                    pool.release_direct_stack_charge(lease);
                }
            }
        }
        drop(self);
        process.heap = heap;
        let result = process.finish_invocation(handler, status, output, None, None, None);
        let snapshot = ExecutionSnapshot {
            phase: if result.is_ok() {
                ExecutionPhase::Complete
            } else {
                ExecutionPhase::Failed
            },
            fault: if result.is_err() {
                Some(ExecutionFault::from_status(status).unwrap_or(ExecutionFault::InvalidOutput))
            } else {
                None
            },
            max_steps,
            remaining_steps,
            heap: heap_stats,
        };
        (process, snapshot, result)
    }
}

impl PreparedFiber {
    pub(crate) fn into_ready_task(
        mut self,
        fibers: &WorkerFibers,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<ScalarTask, (ScalarProcess, String)> {
        let snapshot = ExecutionSnapshot {
            phase: ExecutionPhase::Ready,
            fault: None,
            max_steps,
            remaining_steps: max_steps,
            heap: process.heap_stats(),
        };
        let entry = process.module.inner.entries[handler];
        let start = FiberStart {
            entry,
            state: process.state,
            arguments: arguments.as_ptr(),
            argument_count: arguments.len(),
            max_steps,
            slice_remaining: None,
            cancellation: &*cancellation.flag,
            heap: process.heap.take(),
            effects_enabled: true,
        };
        self.active._cancellation = Some(cancellation.clone());
        let primed = self
            .active
            .coroutine
            .as_mut()
            .expect("prepared reusable coroutine")
            .resume(FiberCommand::Start(start));
        if !matches!(primed, CoroutineResult::Yield(FiberEvent::Primed)) {
            std::process::abort();
        }
        let invocation = Invocation {
            handler,
            backend: Some(InvocationBackend::Reusable(self.active)),
            snapshot,
            effect: None,
            acknowledgement: None,
            preemption: fibers.preemption.clone(),
            _charge: Some(self.control_charge),
        };
        Ok(ScalarTask {
            invocation: Some(invocation),
            process,
            last: None,
            effects_enabled: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_first_slice(
        mut self,
        fibers: &mut WorkerFibers,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        slice_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<
        (
            ScalarProcess,
            ExecutionSnapshot,
            Result<ExecutionStatus, String>,
        ),
        Box<ScalarTask>,
    > {
        let initial_heap = process.heap_stats();
        let entry = process.module.inner.entries[handler];
        let start = FiberStart {
            entry,
            state: process.state,
            arguments: arguments.as_ptr(),
            argument_count: arguments.len(),
            max_steps,
            slice_remaining: Some(slice_steps),
            cancellation: &*cancellation.flag,
            heap: process.heap.take(),
            effects_enabled: true,
        };
        let step = self
            .active
            .coroutine
            .as_mut()
            .expect("prepared reusable coroutine")
            .resume(FiberCommand::Start(start));
        match step {
            CoroutineResult::Yield(FiberEvent::Complete {
                status,
                output,
                heap,
                checkpoint,
            }) => {
                let coroutine = self.active.coroutine.take().expect("completed coroutine");
                let stack_charge = self
                    .active
                    .stack_charge
                    .take()
                    .expect("completed stack charge");
                let pool = self.active.pool.clone();
                fibers.recycle(coroutine, stack_charge, pool);
                drop(self.control_charge);
                process.heap = heap;
                let result = process.finish_invocation(handler, status, output, None, None, None);
                let snapshot = ExecutionSnapshot {
                    phase: if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    },
                    fault: if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    },
                    max_steps,
                    remaining_steps: checkpoint.remaining_steps,
                    heap: checkpoint.heap,
                };
                Ok((process, snapshot, result))
            }
            CoroutineResult::Yield(FiberEvent::Suspended(checkpoint)) => {
                if let Some(requested_at) = checkpoint.yield_requested {
                    fibers.preemption.confirm_yield(requested_at);
                }
                self.active._cancellation = Some(cancellation.clone());
                let snapshot = ExecutionSnapshot {
                    phase: ExecutionPhase::Suspended,
                    fault: None,
                    max_steps,
                    remaining_steps: checkpoint.remaining_steps,
                    heap: checkpoint.heap.or(initial_heap),
                };
                let invocation = Invocation {
                    handler,
                    backend: Some(InvocationBackend::Reusable(self.active)),
                    snapshot,
                    effect: checkpoint.effect,
                    acknowledgement: None,
                    preemption: fibers.preemption.clone(),
                    _charge: Some(self.control_charge),
                };
                Err(Box::new(ScalarTask {
                    invocation: Some(invocation),
                    process,
                    last: None,
                    effects_enabled: true,
                }))
            }
            CoroutineResult::Yield(FiberEvent::Primed) | CoroutineResult::Return(()) => {
                std::process::abort()
            }
        }
    }
}

fn new_reusable_coroutine(stack: FiberStack, preemption: PreemptionToken) -> ReusableCoroutine {
    let stack_limit = stack.managed_limit();
    Coroutine::with_stack(stack, move |yielder, mut command| {
        let mut arguments = [0i64; 64];
        loop {
            let FiberCommand::Start(mut start) = command else {
                return;
            };
            if start.argument_count != 0 {
                arguments[..start.argument_count].copy_from_slice(unsafe {
                    std::slice::from_raw_parts(start.arguments, start.argument_count)
                });
            }
            let mut output = ScalarOutput::default();
            let slice_remaining = match start.slice_remaining {
                Some(steps) => steps,
                None => match yielder.suspend(FiberEvent::Primed) {
                    FiberCommand::Run(steps) => steps,
                    _ => {
                        let checkpoint = Checkpoint {
                            remaining_steps: start.max_steps,
                            heap: start.heap.as_deref().map(AotHeap::stats),
                            effect: None,
                            yield_requested: None,
                        };
                        command = yielder.suspend(FiberEvent::Complete {
                            status: STATUS_CANCELLED,
                            output,
                            heap: start.heap,
                            checkpoint,
                        });
                        continue;
                    }
                },
            };
            let mut control = ReusableControl {
                yielder,
                preemption: &preemption,
            };
            let mut context = ScalarContext {
                cancellation: start.cancellation,
                remaining_steps: start.max_steps,
                yield_request: &*preemption.flag,
                slice_remaining,
                slow_path: Some(suspend_reusable),
                data: std::ptr::from_mut(&mut control).cast(),
                stack_limit,
                roots: std::ptr::null(),
                heap_call: start
                    .heap
                    .as_ref()
                    .map(|_| crate::aot_heap::heap_call as wings_abi::HeapCall),
                heap_data: start.heap.as_mut().map_or(std::ptr::null_mut(), |heap| {
                    std::ptr::from_mut(&mut **heap).cast()
                }),
                effect_call: start
                    .effects_enabled
                    .then_some(send_self_reusable as wings_abi::EffectCall),
            };
            let raw_status = unsafe {
                (start.entry)(start.state, arguments.as_ptr(), &mut output, &mut context)
            };
            let status = if context.roots.is_null() {
                raw_status
            } else {
                wings_abi::STATUS_INVALID_ROOT
            };
            let checkpoint = Checkpoint {
                remaining_steps: context.remaining_steps,
                heap: start.heap.as_deref().map(AotHeap::stats),
                effect: None,
                yield_requested: None,
            };
            if let Some(heap) = &mut start.heap {
                heap.clear_incoming();
            }
            command = yielder.suspend(FiberEvent::Complete {
                status,
                output,
                heap: start.heap,
                checkpoint,
            });
        }
    })
}

fn reserve_control(pool: &StackPool) -> Result<Option<Reservation>, String> {
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

#[allow(clippy::too_many_arguments)]
fn new_coroutine(
    entry: ScalarEntry,
    state: i64,
    mut owned_arguments: InvocationArgs,
    max_steps: u64,
    cancellation: &CancellationToken,
    preemption: &PreemptionToken,
    stack: PooledStack,
    heap: &mut Option<Box<AotHeap>>,
    effects_enabled: bool,
    record_mask: u64,
    lists: ListPayload,
    bytes: &mut BytePayload,
    module: Option<ScalarModule>,
    output_types: [wings_abi::ValueType; 2],
) -> Result<ExecutionCoroutine, String> {
    let stack_limit = stack.managed_limit();
    let cancellation = cancellation.clone();
    let preemption = preemption.clone();
    let mut heap = heap.take();
    let bytes = std::mem::take(bytes);
    let coroutine = Coroutine::with_stack(stack, move |yielder, command| {
        let mut output = ScalarOutput::default();
        let ResumeCommand::Run(slice_remaining) = command else {
            let checkpoint = Checkpoint {
                remaining_steps: max_steps,
                heap: heap.as_deref().map(AotHeap::stats),
                effect: None,
                yield_requested: None,
            };
            return (STATUS_CANCELLED, output, heap, checkpoint, None, None, None);
        };
        let mut control = InvocationControl {
            yielder,
            preemption: &preemption,
        };
        let mut context = ScalarContext {
            cancellation: &*cancellation.flag,
            remaining_steps: max_steps,
            yield_request: &*preemption.flag,
            slice_remaining,
            slow_path: Some(suspend_scalar),
            data: std::ptr::from_mut(&mut control).cast(),
            stack_limit,
            roots: std::ptr::null(),
            heap_call: heap
                .as_ref()
                .map(|_| crate::aot_heap::heap_call as wings_abi::HeapCall),
            heap_data: heap.as_mut().map_or(std::ptr::null_mut(), |heap| {
                std::ptr::from_mut(&mut **heap).cast()
            }),
            effect_call: effects_enabled.then_some(send_self as wings_abi::EffectCall),
        };
        let imported = unsafe {
            crate::aot_heap::import_arguments(
                &mut context,
                owned_arguments.as_mut_slice(),
                record_mask,
                &lists,
                &bytes,
            )
        };
        drop(bytes);
        let status = match imported {
            Ok(()) => unsafe { entry(state, owned_arguments.as_ptr(), &mut output, &mut context) },
            Err(status) => status,
        };
        let mut status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        let mut list = None;
        let mut bytes = None;
        let mut record = None;
        if status == STATUS_OK && output_types[1] == wings_abi::ValueType::ListRef {
            let roots = [output.reply as u64, output.state as u64];
            let frame = wings_abi::ShadowFrame {
                previous: std::ptr::null(),
                slots: roots.as_ptr(),
                count: 1 + usize::from(output_types[0].is_reference()),
            };
            context.roots = &frame;
            let mut arguments = [output.reply];
            match unsafe { crate::aot_heap::export_lists(&mut context, &mut arguments, 1) } {
                Ok(payload) => {
                    let copied = heap
                        .as_ref()
                        .ok_or(wings_abi::STATUS_INVALID_ROOT)
                        .and_then(|heap| {
                            IntListReply::copy(&heap.account, &payload.values[..payload.count])
                                .map_err(|_| wings_abi::STATUS_HEAP_EXHAUSTED)
                        });
                    match copied {
                        Ok(value) => list = Some(value),
                        Err(error) => status = error,
                    }
                }
                Err(wings_abi::STATUS_SEND_REJECTED) => status = wings_abi::STATUS_REPLY_REJECTED,
                Err(error) => status = error,
            }
            context.roots = std::ptr::null();
        }
        if status == STATUS_OK {
            if let wings_abi::ValueType::RecordRef(schema) = output_types[1] {
                let roots = [output.reply as u64, output.state as u64];
                let frame = wings_abi::ShadowFrame {
                    previous: std::ptr::null(),
                    slots: roots.as_ptr(),
                    count: 1 + usize::from(output_types[0].is_reference()),
                };
                context.roots = &frame;
                let exported = module
                    .as_ref()
                    .ok_or(wings_abi::STATUS_INVALID_ROOT)
                    .and_then(|module| unsafe {
                        crate::aot_heap::export_record_reply(
                            &mut context,
                            output.reply,
                            schema,
                            &module.manifest().records,
                        )
                    });
                match exported {
                    Ok(value) => record = Some(value),
                    Err(error) => status = error,
                }
                context.roots = std::ptr::null();
            }
        }
        if status == STATUS_OK
            && matches!(
                output_types[1],
                wings_abi::ValueType::BytesRef | wings_abi::ValueType::StringRef
            )
        {
            let roots = [output.reply as u64, output.state as u64];
            let frame = wings_abi::ShadowFrame {
                previous: std::ptr::null(),
                slots: roots.as_ptr(),
                count: 1 + usize::from(output_types[0].is_reference()),
            };
            context.roots = &frame;
            let mut arguments = [output.reply];
            let is_string = output_types[1] == wings_abi::ValueType::StringRef;
            match unsafe {
                crate::aot_heap::export_buffers(
                    &mut context,
                    &mut arguments,
                    u64::from(!is_string),
                    u64::from(is_string),
                )
            } {
                Ok(payload) => {
                    let account = heap.as_ref().map(|heap| heap.account.clone());
                    let result =
                        account
                            .ok_or(wings_abi::STATUS_INVALID_ROOT)
                            .and_then(|account| {
                                if is_string {
                                    crate::StringReply::from_payload(
                                        &account,
                                        payload,
                                        &mut context,
                                    )
                                    .map(BufferReply::String)
                                } else {
                                    BytesReply::from_payload(&account, payload)
                                        .map(BufferReply::Bytes)
                                        .map_err(|_| wings_abi::STATUS_HEAP_EXHAUSTED)
                                }
                            });
                    match result {
                        Ok(value) => bytes = Some(value),
                        Err(error) => status = error,
                    }
                }
                Err(error) => status = error,
            }
            context.roots = std::ptr::null();
        }
        let checkpoint = Checkpoint {
            remaining_steps: context.remaining_steps,
            heap: heap.as_deref().map(AotHeap::stats),
            effect: None,
            yield_requested: None,
        };
        if let Some(heap) = &mut heap {
            heap.clear_incoming();
        }
        (status, output, heap, checkpoint, list, bytes, record)
    });
    Ok(coroutine)
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

fn resume_invocation(
    process: &mut ScalarProcess,
    invocation: &mut Option<Invocation>,
    last: &mut Option<ExecutionSnapshot>,
    max_slice_steps: u64,
    fibers: Option<&mut WorkerFibers>,
) -> Result<ExecutionStatus, String> {
    let active = invocation
        .as_mut()
        .ok_or("scalar execution has already completed or was not started")?;
    if max_slice_steps == 0 {
        return Err("execution time slice must contain at least one step".into());
    }
    let acknowledgement = active.acknowledgement.take();
    match active.backend.as_mut().expect("active invocation backend") {
        InvocationBackend::OneShot(coroutine) => {
            let command = match acknowledgement {
                Some((status, value)) => ResumeCommand::Effect(status, value, max_slice_steps),
                None => ResumeCommand::Run(max_slice_steps),
            };
            match coroutine.resume(command) {
                CoroutineResult::Yield(checkpoint) => {
                    if let Some(requested_at) = checkpoint.yield_requested {
                        active.preemption.confirm_yield(requested_at);
                    }
                    active.snapshot.phase = ExecutionPhase::Suspended;
                    active.snapshot.remaining_steps = checkpoint.remaining_steps;
                    active.snapshot.heap = checkpoint.heap;
                    active.effect = checkpoint.effect;
                    Ok(ExecutionStatus::Suspended)
                }
                CoroutineResult::Return((
                    status,
                    output,
                    heap,
                    checkpoint,
                    list,
                    bytes,
                    record,
                )) => {
                    let handler = active.handler;
                    let mut snapshot = active.snapshot;
                    snapshot.remaining_steps = checkpoint.remaining_steps;
                    snapshot.heap = checkpoint.heap;
                    active.backend.take();
                    invocation.take();
                    process.heap = heap;
                    let result =
                        process.finish_invocation(handler, status, output, list, bytes, record);
                    snapshot.phase = if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    };
                    snapshot.fault = if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    };
                    *last = Some(snapshot);
                    result
                }
            }
        }
        InvocationBackend::Reusable(reusable) => {
            let command = match acknowledgement {
                Some((status, value)) => FiberCommand::Effect(status, value, max_slice_steps),
                None => FiberCommand::Run(max_slice_steps),
            };
            let step = reusable
                .coroutine
                .as_mut()
                .expect("active reusable coroutine")
                .resume(command);
            match step {
                CoroutineResult::Yield(FiberEvent::Suspended(checkpoint)) => {
                    if let Some(requested_at) = checkpoint.yield_requested {
                        active.preemption.confirm_yield(requested_at);
                    }
                    active.snapshot.phase = ExecutionPhase::Suspended;
                    active.snapshot.remaining_steps = checkpoint.remaining_steps;
                    active.snapshot.heap = checkpoint.heap;
                    active.effect = checkpoint.effect;
                    Ok(ExecutionStatus::Suspended)
                }
                CoroutineResult::Yield(FiberEvent::Complete {
                    status,
                    output,
                    heap,
                    checkpoint,
                }) => {
                    let coroutine = reusable.coroutine.take().expect("completed coroutine");
                    let stack_charge = reusable
                        .stack_charge
                        .take()
                        .expect("completed stack charge");
                    let pool = reusable.pool.clone();
                    if let Some(fibers) = fibers {
                        fibers.recycle(coroutine, stack_charge, pool);
                    } else {
                        let mut shutdown = coroutine;
                        if shutdown.started()
                            && !shutdown.done()
                            && !matches!(
                                shutdown.resume(FiberCommand::Shutdown),
                                CoroutineResult::Return(())
                            )
                        {
                            std::process::abort();
                        }
                        pool.drop_active_fiber();
                        drop(stack_charge);
                    }
                    let handler = active.handler;
                    let mut snapshot = active.snapshot;
                    snapshot.remaining_steps = checkpoint.remaining_steps;
                    snapshot.heap = checkpoint.heap;
                    active.backend.take();
                    invocation.take();
                    process.heap = heap;
                    let result =
                        process.finish_invocation(handler, status, output, None, None, None);
                    snapshot.phase = if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    };
                    snapshot.fault = if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    };
                    *last = Some(snapshot);
                    result
                }
                CoroutineResult::Yield(FiberEvent::Primed) | CoroutineResult::Return(()) => {
                    std::process::abort()
                }
            }
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

unsafe extern "C" fn suspend_reusable(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<ReusableControl<'_>>() };
    let yield_requested = control.preemption.take_request();
    let checkpoint = Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: None,
        yield_requested,
    };
    match control.yielder.suspend(FiberEvent::Suspended(checkpoint)) {
        FiberCommand::Run(steps) => {
            context.slice_remaining = steps;
            STATUS_OK
        }
        _ => STATUS_CANCELLED,
    }
}

unsafe extern "C" fn suspend_scalar(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<InvocationControl<'_>>() };
    let yield_requested = control.preemption.take_request();
    let checkpoint = Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: None,
        yield_requested,
    };
    match control.yielder.suspend(checkpoint) {
        ResumeCommand::Run(steps) => {
            context.slice_remaining = steps;
            STATUS_OK
        }
        ResumeCommand::Cancel | ResumeCommand::Effect(_, _, _) => STATUS_CANCELLED,
    }
}

pub(crate) fn runtime_checkpoint(context: &mut ScalarContext, consume: bool) -> Result<(), u32> {
    if context.cancellation.is_null() || context.yield_request.is_null() {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    loop {
        if unsafe { &*context.cancellation }.load(Ordering::SeqCst) != 0 {
            return Err(STATUS_CANCELLED);
        }
        if context.remaining_steps == 0 {
            return Err(wings_abi::STATUS_BUDGET_EXHAUSTED);
        }
        if context.slice_remaining == 0
            || unsafe { &*context.yield_request }.load(Ordering::SeqCst) != 0
        {
            let callback = context.slow_path.ok_or(wings_abi::STATUS_INVALID_CONTROL)?;
            let status = unsafe { callback(context) };
            if status != STATUS_OK {
                return Err(status);
            }
            continue;
        }
        if consume {
            context.remaining_steps -= 1;
            context.slice_remaining -= 1;
        }
        return Ok(());
    }
}

pub(crate) fn runtime_checkpoint_batch(
    context: &mut ScalarContext,
    max_steps: usize,
) -> Result<usize, u32> {
    if context.cancellation.is_null() || context.yield_request.is_null() {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    if unsafe { &*context.cancellation }.load(Ordering::SeqCst) == 0
        && unsafe { &*context.yield_request }.load(Ordering::SeqCst) == 0
        && context.remaining_steps > 0
        && context.slice_remaining > 0
    {
        let steps = max_steps
            .max(1)
            .min(usize::try_from(context.remaining_steps).unwrap_or(usize::MAX))
            .min(usize::try_from(context.slice_remaining).unwrap_or(usize::MAX));
        context.remaining_steps -= steps as u64;
        context.slice_remaining -= steps as u64;
        return Ok(steps);
    }
    runtime_checkpoint(context, true)?;
    Ok(1)
}

unsafe fn prepare_effect_checkpoint(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> Result<Checkpoint, u32> {
    let Some(request) = (unsafe { request.as_ref() }) else {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    };
    let count = request.count;
    let arguments = request.arguments;
    if output.is_null()
        || count > 64
        || (count != 0 && arguments.is_null())
        || ((request.record_mask | request.list_mask | request.bytes_mask | request.string_mask)
            != 0
            && !matches!(
                request.operation,
                wings_abi::EFFECT_SEND_SELF
                    | wings_abi::EFFECT_SEND
                    | wings_abi::EFFECT_SEND_ACTOR
                    | wings_abi::EFFECT_REQUEST
                    | wings_abi::EFFECT_REQUEST_ACTOR
            ))
    {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    let context = unsafe { &mut *context };
    let mut copied = [0; 64];
    if count != 0 {
        copied[..count].copy_from_slice(unsafe { std::slice::from_raw_parts(arguments, count) });
    }
    let buffer_mask = request.bytes_mask | request.string_mask;
    if request.bytes_mask & request.string_mask != 0
        || request.record_mask & (request.list_mask | buffer_mask) != 0
        || request.list_mask & buffer_mask != 0
        || (count < 64 && (request.record_mask | request.list_mask | buffer_mask) >> count != 0)
    {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    if request.record_mask != 0 {
        let Some(heap) = (unsafe { context.heap_data.cast::<AotHeap>().as_ref() }) else {
            return Err(wings_abi::STATUS_INVALID_ROOT);
        };
        for (index, argument) in copied[..count].iter_mut().enumerate() {
            if request.record_mask & (1u64 << index) != 0 {
                *argument = heap.record_value(*argument)?;
            }
        }
    }
    let lists =
        unsafe { crate::aot_heap::export_lists(context, &mut copied[..count], request.list_mask) }?;
    let bytes = unsafe {
        crate::aot_heap::export_buffers(
            context,
            &mut copied[..count],
            request.bytes_mask,
            request.string_mask,
        )
    }?;
    Ok(Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: Some(ProcessEffect {
            operation: request.operation,
            target: request.target,
            handler: request.handler as usize,
            arguments: copied,
            count,
            callback: request.callback as usize,
            timeout_ms: request.timeout_ms,
            record_mask: request.record_mask,
            lists,
            bytes,
        }),
        yield_requested: None,
    })
}

unsafe extern "C" fn send_self(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> u32 {
    let checkpoint = match unsafe { prepare_effect_checkpoint(context, request, output) } {
        Ok(checkpoint) => checkpoint,
        Err(status) => return status,
    };
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<InvocationControl<'_>>() };
    match control.yielder.suspend(checkpoint) {
        ResumeCommand::Effect(status, value, steps) => {
            context.slice_remaining = steps;
            if status == STATUS_OK {
                unsafe {
                    output.write(value);
                }
            }
            status
        }
        _ => STATUS_CANCELLED,
    }
}

unsafe extern "C" fn send_self_reusable(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> u32 {
    let checkpoint = match unsafe { prepare_effect_checkpoint(context, request, output) } {
        Ok(checkpoint) => checkpoint,
        Err(status) => return status,
    };
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<ReusableControl<'_>>() };
    match control.yielder.suspend(FiberEvent::Suspended(checkpoint)) {
        FiberCommand::Effect(status, value, steps) => {
            context.slice_remaining = steps;
            if status == STATUS_OK {
                unsafe {
                    output.write(value);
                }
            }
            status
        }
        _ => STATUS_CANCELLED,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    #[test]
    fn fixed_core_invocation_reservation_covers_owned_payload_and_control() {
        let maximum = std::mem::size_of::<Invocation>()
            + std::mem::size_of::<ExecutionCoroutine>()
            + std::mem::size_of::<Checkpoint>()
            + std::mem::size_of::<InvocationControl<'_>>()
            + std::mem::size_of::<ListPayload>()
            + std::mem::size_of::<BytePayload>()
            + 64 * std::mem::size_of::<i64>()
            + 2 * crate::control_flag::ControlFlag::UNACCOUNTED_BYTES
            + 256;
        assert!(
            maximum as u64 <= INVOCATION_OVERHEAD,
            "invocation control needs {maximum} bytes"
        );
    }

    #[test]
    fn effect_reference_masks_reject_overlap_missing_heap_and_invalid_bits_before_yield() {
        let cancelled = AtomicU32::new(0);
        let mut context = ScalarContext {
            cancellation: &cancelled,
            remaining_steps: 1,
            yield_request: &cancelled,
            slice_remaining: 1,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::null_mut(),
            effect_call: None,
        };
        let arguments = [1];
        let mut request = wings_abi::EffectRequest {
            operation: wings_abi::EFFECT_SEND,
            handler: 0,
            target: 0,
            arguments: arguments.as_ptr(),
            count: 1,
            callback: 0,
            timeout_ms: 0,
            record_mask: 2,
            list_mask: 0,
            bytes_mask: 0,
            string_mask: 0,
        };
        let mut output = 777;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.record_mask = 1;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_ROOT
        );
        request.record_mask = 0;
        request.list_mask = 2;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.record_mask = 0;
        request.list_mask = 0;
        request.bytes_mask = 2;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.bytes_mask = 1;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_ROOT
        );
        for (record, list) in [(1, 0), (0, 1), (1, 1)] {
            request.record_mask = record;
            request.list_mask = list;
            assert_eq!(
                unsafe { send_self(&mut context, &request, &mut output) },
                wings_abi::STATUS_INVALID_CONTROL
            );
        }
        request.bytes_mask = 0;
        request.record_mask = 0;
        request.list_mask = 0;
        context.remaining_steps = 8;
        context.slice_remaining = 8;
        request.string_mask = 2;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.string_mask = 1;
        request.bytes_mask = 1;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.bytes_mask = 0;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_ROOT
        );
        request.string_mask = 0;
        context.remaining_steps = 8;
        context.slice_remaining = 8;
        request.list_mask = 1;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_ROOT
        );
        request.record_mask = 1;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
        request.list_mask = 0;
        let all_arguments = [1; 64];
        request.arguments = all_arguments.as_ptr();
        request.count = 64;
        request.record_mask = u64::MAX;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_ROOT
        );
        for operation in [
            wings_abi::EFFECT_SELF_PID,
            wings_abi::EFFECT_SPAWN_SELF,
            wings_abi::EFFECT_CANCEL_REQUEST,
            u32::MAX,
        ] {
            request.operation = operation;
            assert_eq!(
                unsafe { send_self(&mut context, &request, &mut output) },
                wings_abi::STATUS_INVALID_CONTROL
            );
            request.record_mask = 0;
            request.list_mask = u64::MAX;
            assert_eq!(
                unsafe { send_self(&mut context, &request, &mut output) },
                wings_abi::STATUS_INVALID_CONTROL
            );
        }
        assert_eq!(output, 777);
    }

    unsafe extern "C" fn resume_runtime_chunk(context: *mut ScalarContext) -> u32 {
        let context = unsafe { &mut *context };
        unsafe { &*context.yield_request }.store(0, Ordering::SeqCst);
        context.slice_remaining = 1;
        STATUS_OK
    }

    #[test]
    fn runtime_chunks_recheck_cancellation_budget_and_atomic_yield_before_work() {
        let cancellation = AtomicU32::new(0);
        let request = AtomicU32::new(1);
        let mut context = ScalarContext {
            cancellation: &cancellation,
            remaining_steps: 2,
            yield_request: &request,
            slice_remaining: 0,
            slow_path: Some(resume_runtime_chunk),
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::null_mut(),
            effect_call: None,
        };
        assert_eq!(runtime_checkpoint(&mut context, false), Ok(()));
        assert_eq!(
            (
                context.remaining_steps,
                context.slice_remaining,
                request.load(Ordering::SeqCst)
            ),
            (2, 1, 0)
        );
        assert_eq!(runtime_checkpoint(&mut context, true), Ok(()));
        assert_eq!((context.remaining_steps, context.slice_remaining), (1, 0));
        assert_eq!(runtime_checkpoint(&mut context, true), Ok(()));
        assert_eq!((context.remaining_steps, context.slice_remaining), (0, 0));
        assert_eq!(
            runtime_checkpoint(&mut context, true),
            Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
        );
        cancellation.store(1, Ordering::SeqCst);
        assert_eq!(
            runtime_checkpoint(&mut context, true),
            Err(STATUS_CANCELLED)
        );
        cancellation.store(0, Ordering::SeqCst);
        context.remaining_steps = 1;
        context.slow_path = None;
        assert_eq!(
            runtime_checkpoint(&mut context, true),
            Err(wings_abi::STATUS_INVALID_CONTROL)
        );
        context.cancellation = std::ptr::null();
        assert_eq!(
            runtime_checkpoint(&mut context, true),
            Err(wings_abi::STATUS_INVALID_CONTROL)
        );
    }

    #[test]
    fn faults_classify_raw_abi_statuses_without_parsing_error_text() {
        assert_eq!(ExecutionFault::from_status(STATUS_OK), None);
        for (status, fault) in [
            (wings_abi::STATUS_OVERFLOW, ExecutionFault::Overflow),
            (
                wings_abi::STATUS_DIVISION_BY_ZERO,
                ExecutionFault::DivisionByZero,
            ),
            (STATUS_CANCELLED, ExecutionFault::Cancelled),
            (
                wings_abi::STATUS_BUDGET_EXHAUSTED,
                ExecutionFault::BudgetExhausted,
            ),
            (
                wings_abi::STATUS_INVALID_CONTROL,
                ExecutionFault::InvalidControl,
            ),
            (
                wings_abi::STATUS_STACK_EXHAUSTED,
                ExecutionFault::StackExhausted,
            ),
            (
                wings_abi::STATUS_HEAP_EXHAUSTED,
                ExecutionFault::HeapExhausted,
            ),
            (wings_abi::STATUS_INVALID_ROOT, ExecutionFault::InvalidRoot),
            (wings_abi::STATUS_EMPTY_LIST, ExecutionFault::EmptyList),
            (
                wings_abi::STATUS_SEND_REJECTED,
                ExecutionFault::SendRejected,
            ),
            (
                wings_abi::STATUS_SPAWN_REJECTED,
                ExecutionFault::SpawnRejected,
            ),
            (
                wings_abi::STATUS_REQUEST_REJECTED,
                ExecutionFault::RequestRejected,
            ),
            (u32::MAX, ExecutionFault::UnknownStatus(u32::MAX)),
        ] {
            assert_eq!(ExecutionFault::from_status(status), Some(fault));
        }
    }
}
