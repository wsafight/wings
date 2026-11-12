use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarEntry, ScalarOutput};

use crate::aot_heap::AotHeap;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPhase {
    Ready,
    Suspended,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    pub phase: ExecutionPhase,
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
}

struct Invocation {
    handler: usize,
    coroutine: Option<ExecutionCoroutine>,
    snapshot: ExecutionSnapshot,
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
        }
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
        if self.is_active() {
            return Err("scalar task already has an active invocation".into());
        }
        let handler = self.process.validate_handler(handler, arguments)?;
        self.invocation = Some(Invocation::new(
            &mut self.process,
            handler,
            arguments,
            max_steps,
            cancellation,
            preemption,
            pool,
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
    fn new(
        process: &mut ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
    ) -> Result<Self, String> {
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
        )?;
        Ok(Self {
            handler,
            coroutine: Some(coroutine),
            snapshot,
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
        };
        let status = unsafe { entry(state, owned_arguments.as_ptr(), &mut output, &mut context) };
        let status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        let checkpoint = Checkpoint {
            remaining_steps: context.remaining_steps,
            heap: heap.as_deref().map(AotHeap::stats),
        };
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
    match coroutine.resume(ResumeCommand::Run(max_slice_steps)) {
        CoroutineResult::Yield(checkpoint) => {
            active.snapshot.phase = ExecutionPhase::Suspended;
            active.snapshot.remaining_steps = checkpoint.remaining_steps;
            active.snapshot.heap = checkpoint.heap;
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
    };
    match yielder.suspend(checkpoint) {
        ResumeCommand::Run(steps) => {
            context.slice_remaining = steps;
            STATUS_OK
        }
        ResumeCommand::Cancel => STATUS_CANCELLED,
    }
}
