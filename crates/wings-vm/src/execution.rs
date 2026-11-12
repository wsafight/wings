use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarEntry, ScalarOutput};

use crate::stack_pool::PooledStack;
use crate::{CancellationToken, HandlerId, ScalarModule, ScalarProcess, StackPool};

thread_local! {
    static DEFAULT_STACK_POOL: std::cell::RefCell<Option<StackPool>> = const { std::cell::RefCell::new(None) };
}

type ExecutionCoroutine = Coroutine<ResumeCommand, (), (u32, ScalarOutput), PooledStack>;

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
) -> Result<(u32, ScalarOutput), String> {
    let mut coroutine = new_coroutine(
        entry,
        0,
        &[],
        wings_abi::DEFAULT_MAX_STEPS,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        pool.acquire()?,
    )?;
    match coroutine.resume(ResumeCommand::Run(u64::MAX)) {
        CoroutineResult::Return(result) => Ok(result),
        CoroutineResult::Yield(()) => {
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
}

pub struct ScalarTask {
    invocation: Option<Invocation>,
    process: ScalarProcess,
}

struct Invocation {
    handler: usize,
    coroutine: Option<ExecutionCoroutine>,
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
        let invocation = Invocation::new(
            self,
            handler,
            arguments,
            max_steps,
            cancellation,
            preemption,
            &default_stack_pool()?,
        )?;
        Ok(ScalarExecution {
            process: self,
            invocation: Some(invocation),
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
        })
    }
}

impl ScalarTask {
    pub fn new(process: ScalarProcess) -> Self {
        Self {
            invocation: None,
            process,
        }
    }

    pub fn module(&self) -> &ScalarModule {
        self.process.module()
    }

    pub fn is_active(&self) -> bool {
        self.invocation.is_some()
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
            &self.process,
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
        resume_invocation(&mut self.process, &mut self.invocation, max_slice_steps)
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
        process: &ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        preemption: &PreemptionToken,
        pool: &StackPool,
    ) -> Result<Self, String> {
        let coroutine = new_coroutine(
            process.module.inner.entries[handler],
            process.state,
            arguments,
            max_steps,
            cancellation,
            preemption,
            pool.acquire()?,
        )?;
        Ok(Self {
            handler,
            coroutine: Some(coroutine),
        })
    }
}

fn new_coroutine(
    entry: ScalarEntry,
    state: i64,
    arguments: &[i64],
    max_steps: u64,
    cancellation: &CancellationToken,
    preemption: &PreemptionToken,
    stack: PooledStack,
) -> Result<ExecutionCoroutine, String> {
    let stack_limit = stack.managed_limit();
    let mut owned_arguments = Vec::new();
    owned_arguments
        .try_reserve_exact(arguments.len())
        .map_err(|error| error.to_string())?;
    owned_arguments.extend_from_slice(arguments);
    let cancellation = cancellation.clone();
    let preemption = preemption.clone();
    let coroutine = Coroutine::with_stack(stack, move |yielder, command| {
        let mut output = ScalarOutput::default();
        let ResumeCommand::Run(slice_remaining) = command else {
            return (STATUS_CANCELLED, output);
        };
        let mut context = ScalarContext {
            cancellation: Arc::as_ptr(&cancellation.flag),
            remaining_steps: max_steps,
            yield_request: Arc::as_ptr(&preemption.flag),
            slice_remaining,
            slow_path: Some(suspend_scalar),
            data: (yielder as *const Yielder<ResumeCommand, ()>)
                .cast_mut()
                .cast(),
            stack_limit,
        };
        let status = unsafe { entry(state, owned_arguments.as_ptr(), &mut output, &mut context) };
        (status, output)
    });
    Ok(coroutine)
}

impl ScalarExecution<'_> {
    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        resume_invocation(self.process, &mut self.invocation, max_slice_steps)
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
        CoroutineResult::Yield(()) => Ok(ExecutionStatus::Suspended),
        CoroutineResult::Return((status, output)) => {
            let handler = active.handler;
            active.coroutine.take();
            invocation.take();
            process
                .finish_invocation(handler, status, output)
                .map(ExecutionStatus::Complete)
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
    let yielder = unsafe { &*context.data.cast::<Yielder<ResumeCommand, ()>>() };
    unsafe { &*context.yield_request }.swap(0, Ordering::SeqCst);
    match yielder.suspend(()) {
        ResumeCommand::Run(steps) => {
            context.slice_remaining = steps;
            STATUS_OK
        }
        ResumeCommand::Cancel => STATUS_CANCELLED,
    }
}
