use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarOutput};

use crate::stack_pool::PooledStack;
use crate::{CancellationToken, ScalarProcess, StackPool};

thread_local! {
    static DEFAULT_STACK_POOL: std::cell::RefCell<Option<StackPool>> = const { std::cell::RefCell::new(None) };
}

type ExecutionCoroutine = Coroutine<ResumeCommand, (), (u32, ScalarOutput), PooledStack>;

#[derive(Clone, Default)]
pub struct PreemptionToken {
    flag: Arc<AtomicU32>,
}

impl PreemptionToken {
    pub fn request(&self) {
        self.flag.store(1, Ordering::SeqCst);
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
        self.validate_invocation(name, arguments)?;
        let pool = DEFAULT_STACK_POOL.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(StackPool::new(64, 2)?);
            }
            Ok::<_, String>(slot.as_ref().expect("initialized stack pool").clone())
        })?;
        self.start_in_pool(name, arguments, max_steps, cancellation, preemption, &pool)
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
        let stack = pool.acquire()?;
        let arguments = arguments.to_vec();
        let entry = self.entries[handler];
        let state = self.state;
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
            };
            let status = unsafe { entry(state, arguments.as_ptr(), &mut output, &mut context) };
            (status, output)
        });
        Ok(ScalarExecution {
            process: self,
            handler,
            coroutine: Some(coroutine),
        })
    }
}

impl ScalarExecution<'_> {
    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        let coroutine = self
            .coroutine
            .as_mut()
            .ok_or("scalar execution has already completed")?;
        if max_slice_steps == 0 {
            return Err("execution time slice must contain at least one step".into());
        }
        match coroutine.resume(ResumeCommand::Run(max_slice_steps)) {
            CoroutineResult::Yield(()) => Ok(ExecutionStatus::Suspended),
            CoroutineResult::Return((status, output)) => {
                self.coroutine.take();
                self.process
                    .finish_invocation(self.handler, status, output)
                    .map(ExecutionStatus::Complete)
            }
        }
    }
}

impl Drop for ScalarExecution<'_> {
    fn drop(&mut self) {
        let Some(coroutine) = self.coroutine.as_mut() else {
            return;
        };
        self.process.failed = true;
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
