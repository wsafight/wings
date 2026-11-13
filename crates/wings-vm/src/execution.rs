use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarEntry, ScalarOutput};

use crate::aot_heap::AotHeap;
use crate::message_values::ListPayload;
use crate::stack_pool::PooledStack;
use crate::{CancellationToken, HandlerId, HeapStats, ScalarModule, ScalarProcess, StackPool};

thread_local! {
    static DEFAULT_STACK_POOL: std::cell::RefCell<Option<StackPool>> = const { std::cell::RefCell::new(None) };
}

type InvocationResult = (u32, ScalarOutput, Option<Box<AotHeap>>, Checkpoint);
type ExecutionCoroutine = Coroutine<ResumeCommand, Checkpoint, InvocationResult, PooledStack>;

#[derive(Clone, Copy)]
struct Checkpoint {
    remaining_steps: u64,
    heap: Option<HeapStats>,
    effect: Option<ProcessEffect>,
}

#[derive(Clone, Copy)]
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
    let mut coroutine = new_coroutine(
        entry,
        0,
        &[],
        wings_abi::DEFAULT_MAX_STEPS,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        pool.acquire()?,
        &mut heap,
        false,
        0,
        ListPayload::default(),
    )?;
    match coroutine.resume(ResumeCommand::Run(u64::MAX)) {
        CoroutineResult::Return((status, output, heap, _)) => Ok((status, output, heap)),
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

#[derive(Clone, Default)]
pub struct PreemptionToken {
    flag: Arc<AtomicU32>,
}

impl PreemptionToken {
    pub fn request(&self) {
        self.flag.store(1, Ordering::SeqCst);
    }

    pub(crate) fn clear(&self) {
        self.flag.store(0, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionStatus {
    Suspended,
    Complete(Option<i64>),
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
    coroutine: Option<ExecutionCoroutine>,
    snapshot: ExecutionSnapshot,
    effect: Option<ProcessEffect>,
    acknowledgement: Option<(u32, i64)>,
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
        self.start_index(handler, arguments, max_steps, cancellation, preemption)
    }

    pub(crate) fn start_index(
        &mut self,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
    ) -> Result<ScalarExecution<'_>, String> {
        let pool = match self.heap.as_ref().and_then(|heap| heap.pool.as_ref()) {
            Some(pool) => pool.clone(),
            None => default_stack_pool()?,
        };
        let invocation = Invocation::new(
            self,
            handler,
            arguments,
            max_steps,
            cancellation,
            preemption,
            &pool,
            false,
            0,
            &ListPayload::default(),
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
            max_steps,
            cancellation,
            preemption,
            pool,
            false,
            0,
            &ListPayload::default(),
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
            cancellation,
            preemption,
            pool,
            false,
            0,
            &ListPayload::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_kind(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
        system: bool,
        record_mask: u64,
        lists: &ListPayload,
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
        )?;
        self.invocation = Some(Invocation::new(
            &mut self.process,
            handler,
            arguments,
            max_steps,
            cancellation,
            preemption,
            pool,
            self.effects_enabled,
            record_mask,
            lists,
        )?);
        Ok(())
    }

    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        resume_invocation(
            &mut self.process,
            &mut self.invocation,
            &mut self.last,
            max_slice_steps,
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

impl Invocation {
    #[allow(clippy::too_many_arguments)]
    fn new(
        process: &mut ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
        effects_enabled: bool,
        record_mask: u64,
        lists: &ListPayload,
    ) -> Result<Self, String> {
        process.module.validate_pool(pool)?;
        if process
            .heap
            .as_ref()
            .and_then(|heap| heap.pool.as_ref())
            .is_some_and(|owner| !owner.shares_process(pool))
        {
            return Err("AOT heap execution requires its process-accounted stack pool".into());
        }
        let snapshot = ExecutionSnapshot {
            phase: ExecutionPhase::Ready,
            fault: None,
            max_steps,
            remaining_steps: max_steps,
            heap: process.heap_stats(),
        };
        let coroutine = new_coroutine(
            process.module.inner.entries[handler],
            process.state,
            arguments,
            max_steps,
            cancellation,
            preemption,
            pool.acquire()?,
            &mut process.heap,
            effects_enabled,
            record_mask,
            *lists,
        )?;
        Ok(Self {
            handler,
            coroutine: Some(coroutine),
            snapshot,
            effect: None,
            acknowledgement: None,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn new_coroutine(
    entry: ScalarEntry,
    state: i64,
    arguments: &[i64],
    max_steps: u64,
    cancellation: &CancellationToken,
    preemption: &PreemptionToken,
    stack: PooledStack,
    heap: &mut Option<Box<AotHeap>>,
    effects_enabled: bool,
    record_mask: u64,
    lists: ListPayload,
) -> Result<ExecutionCoroutine, String> {
    let stack_limit = stack.managed_limit();
    let mut owned_arguments = Vec::new();
    owned_arguments
        .try_reserve_exact(arguments.len())
        .map_err(|error| error.to_string())?;
    owned_arguments.extend_from_slice(arguments);
    let cancellation = cancellation.clone();
    let preemption = preemption.clone();
    let mut heap = heap.take();
    let coroutine = Coroutine::with_stack(stack, move |yielder, command| {
        let mut output = ScalarOutput::default();
        let ResumeCommand::Run(slice_remaining) = command else {
            let checkpoint = Checkpoint {
                remaining_steps: max_steps,
                heap: heap.as_deref().map(AotHeap::stats),
                effect: None,
            };
            return (STATUS_CANCELLED, output, heap, checkpoint);
        };
        let mut context = ScalarContext {
            cancellation: Arc::as_ptr(&cancellation.flag),
            remaining_steps: max_steps,
            yield_request: Arc::as_ptr(&preemption.flag),
            slice_remaining,
            slow_path: Some(suspend_scalar),
            data: (yielder as *const Yielder<ResumeCommand, Checkpoint>)
                .cast_mut()
                .cast(),
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
        let status = match unsafe {
            crate::aot_heap::import_arguments(
                &mut context,
                &mut owned_arguments,
                record_mask,
                &lists,
            )
        } {
            Ok(()) => unsafe { entry(state, owned_arguments.as_ptr(), &mut output, &mut context) },
            Err(status) => status,
        };
        let status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        let checkpoint = Checkpoint {
            remaining_steps: context.remaining_steps,
            heap: heap.as_deref().map(AotHeap::stats),
            effect: None,
        };
        if let Some(heap) = &mut heap {
            heap.clear_incoming();
        }
        (status, output, heap, checkpoint)
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
) -> Result<ExecutionStatus, String> {
    let active = invocation
        .as_mut()
        .ok_or("scalar execution has already completed or was not started")?;
    if max_slice_steps == 0 {
        return Err("execution time slice must contain at least one step".into());
    }
    let coroutine = active.coroutine.as_mut().expect("active coroutine");
    let command = match active.acknowledgement.take() {
        Some((status, value)) => ResumeCommand::Effect(status, value, max_slice_steps),
        None => ResumeCommand::Run(max_slice_steps),
    };
    match coroutine.resume(command) {
        CoroutineResult::Yield(checkpoint) => {
            active.snapshot.phase = ExecutionPhase::Suspended;
            active.snapshot.remaining_steps = checkpoint.remaining_steps;
            active.snapshot.heap = checkpoint.heap;
            active.effect = checkpoint.effect;
            Ok(ExecutionStatus::Suspended)
        }
        CoroutineResult::Return((status, output, heap, checkpoint)) => {
            let handler = active.handler;
            let mut snapshot = active.snapshot;
            snapshot.remaining_steps = checkpoint.remaining_steps;
            snapshot.heap = checkpoint.heap;
            active.coroutine.take();
            invocation.take();
            process.heap = heap;
            let result = process
                .finish_invocation(handler, status, output)
                .map(ExecutionStatus::Complete);
            snapshot.phase = if result.is_ok() {
                ExecutionPhase::Complete
            } else {
                ExecutionPhase::Failed
            };
            snapshot.fault = if result.is_err() {
                Some(ExecutionFault::from_status(status).unwrap_or(ExecutionFault::InvalidOutput))
            } else {
                None
            };
            *last = Some(snapshot);
            result
        }
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        let Some(coroutine) = self.coroutine.as_mut() else {
            return;
        };
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

unsafe extern "C" fn suspend_scalar(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    let yielder = unsafe { &*context.data.cast::<Yielder<ResumeCommand, Checkpoint>>() };
    unsafe { &*context.yield_request }.swap(0, Ordering::SeqCst);
    let checkpoint = Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: None,
    };
    match yielder.suspend(checkpoint) {
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

unsafe extern "C" fn send_self(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> u32 {
    let Some(request) = (unsafe { request.as_ref() }) else {
        return wings_abi::STATUS_INVALID_CONTROL;
    };
    let count = request.count;
    let arguments = request.arguments;
    if output.is_null()
        || count > 64
        || (count != 0 && arguments.is_null())
        || ((request.record_mask | request.list_mask) != 0
            && !matches!(
                request.operation,
                wings_abi::EFFECT_SEND_SELF | wings_abi::EFFECT_SEND | wings_abi::EFFECT_REQUEST
            ))
    {
        return wings_abi::STATUS_INVALID_CONTROL;
    }
    let context = unsafe { &mut *context };
    let mut copied = [0; 64];
    if count != 0 {
        copied[..count].copy_from_slice(unsafe { std::slice::from_raw_parts(arguments, count) });
    }
    if request.record_mask & request.list_mask != 0
        || (count < 64 && (request.record_mask | request.list_mask) >> count != 0)
    {
        return wings_abi::STATUS_INVALID_CONTROL;
    }
    if request.record_mask != 0 {
        let Some(heap) = (unsafe { context.heap_data.cast::<AotHeap>().as_ref() }) else {
            return wings_abi::STATUS_INVALID_ROOT;
        };
        for (index, argument) in copied[..count].iter_mut().enumerate() {
            if request.record_mask & (1u64 << index) != 0 {
                match heap.record_value(*argument) {
                    Ok(value) => *argument = value,
                    Err(status) => return status,
                }
            }
        }
    }
    let lists = match unsafe {
        crate::aot_heap::export_lists(context, &mut copied[..count], request.list_mask)
    } {
        Ok(lists) => lists,
        Err(status) => return status,
    };
    let checkpoint = Checkpoint {
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
        }),
    };
    let yielder = unsafe { &*context.data.cast::<Yielder<ResumeCommand, Checkpoint>>() };
    match yielder.suspend(checkpoint) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_worker_invocation_reservation_covers_owned_payload_and_control() {
        let maximum = std::mem::size_of::<Invocation>()
            + std::mem::size_of::<ExecutionCoroutine>()
            + std::mem::size_of::<Checkpoint>()
            + std::mem::size_of::<ListPayload>()
            + 64 * std::mem::size_of::<i64>()
            + 256;
        assert!(maximum <= 4096, "invocation control needs {maximum} bytes");
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
