use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use wings_abi::{STATUS_CANCELLED, STATUS_OK, ScalarContext, ScalarOutput};

use crate::{CancellationToken, ScalarProcess};

const EXECUTION_STACK_BYTES: usize = 256 * 1024;

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
    coroutine: Coroutine<ResumeCommand, (), (u32, ScalarOutput)>,
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
        let stack = DefaultStack::new(EXECUTION_STACK_BYTES)
            .map_err(|error| format!("allocate execution stack: {error}"))?;
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
            coroutine,
        })
    }
}

impl ScalarExecution<'_> {
    pub fn resume(&mut self, max_slice_steps: u64) -> Result<ExecutionStatus, String> {
        if self.coroutine.done() {
            return Err("scalar execution has already completed".into());
        }
        if max_slice_steps == 0 {
            return Err("execution time slice must contain at least one step".into());
        }
        match self.coroutine.resume(ResumeCommand::Run(max_slice_steps)) {
            CoroutineResult::Yield(()) => Ok(ExecutionStatus::Suspended),
            CoroutineResult::Return((status, output)) => self
                .process
                .finish_invocation(self.handler, status, output)
                .map(ExecutionStatus::Complete),
        }
    }
}

impl Drop for ScalarExecution<'_> {
    fn drop(&mut self) {
        if self.coroutine.done() {
            return;
        }
        self.process.failed = true;
        if self.coroutine.started()
            && !matches!(
                self.coroutine.resume(ResumeCommand::Cancel),
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
