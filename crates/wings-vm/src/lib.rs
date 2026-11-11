use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use wings_abi::{
    DEFAULT_MAX_STEPS, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_INVALID_CONTROL, STATUS_OK, STATUS_OVERFLOW, ScalarContext, ScalarOutput, ValueType,
};
use wings_artifact::Manifest;

mod execution;
mod module;
mod resources;
mod stack_pool;

pub use execution::{ExecutionStatus, PreemptionToken, ScalarExecution};
pub use module::{HandlerId, ScalarModule};
pub use resources::{Reservation, ResourceAccount, ResourceLevel, Resources};
pub use stack_pool::{StackPool, StackPoolStats};

pub struct ScalarProcess {
    module: ScalarModule,
    state: i64,
    failed: bool,
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicU32>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.flag.store(1, Ordering::SeqCst);
    }
}

impl ScalarProcess {
    /// # Safety
    /// The library must be trusted Wings scalar AOT code matching its manifest,
    /// with valid entry signatures, bounded pointer access and no unwinding.
    /// Entries must honor execution controls, return normally from a failed
    /// slow path without writing output, and fit the fixed execution stack.
    pub unsafe fn load_trusted(directory: &Path) -> Result<Self, String> {
        unsafe { ScalarModule::load_trusted(directory) }?.instantiate()
    }

    pub fn manifest(&self) -> &Manifest {
        self.module.manifest()
    }

    pub fn module(&self) -> &ScalarModule {
        &self.module
    }

    pub fn invoke(&mut self, name: &str, arguments: &[i64]) -> Result<Option<i64>, String> {
        self.invoke_controlled(
            name,
            arguments,
            DEFAULT_MAX_STEPS,
            &CancellationToken::default(),
        )
    }

    pub fn invoke_controlled(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<i64>, String> {
        let index = self.validate_invocation(name, arguments)?;
        self.invoke_index(index, arguments, max_steps, cancellation)
    }

    pub fn invoke_handler(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<i64>, String> {
        let index = self.validate_handler(handler, arguments)?;
        self.invoke_index(index, arguments, max_steps, cancellation)
    }

    fn invoke_index(
        &mut self,
        index: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<i64>, String> {
        let mut output = ScalarOutput::default();
        let yield_request = AtomicU32::new(0);
        let mut context = direct_context(&cancellation.flag, &yield_request, max_steps);
        let status = unsafe {
            (self.module.inner.entries[index])(
                self.state,
                arguments.as_ptr(),
                &mut output,
                &mut context,
            )
        };
        self.finish_invocation(index, status, output)
    }

    fn validate_invocation(&self, name: &str, arguments: &[i64]) -> Result<usize, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        let handler = self.module.handler(name)?;
        self.validate_handler(&handler, arguments)
    }

    fn validate_handler(&self, handler: &HandlerId, arguments: &[i64]) -> Result<usize, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        if !Arc::ptr_eq(&self.module.inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let schema = handler.schema();
        if arguments.len() != schema.parameters.len()
            || arguments
                .iter()
                .zip(&schema.parameters)
                .any(|(value, (_, ty))| !ty.accepts(*value))
        {
            return Err("handler argument count or scalar type mismatch".into());
        }
        Ok(handler.index)
    }

    fn finish_invocation(
        &mut self,
        index: usize,
        status: u32,
        output: ScalarOutput,
    ) -> Result<Option<i64>, String> {
        if let Err(error) = check_status(status) {
            self.failed = true;
            return Err(error);
        }
        let handler = &self.module.inner.manifest.handlers[index];
        if !self.module.inner.manifest.state.accepts(output.state)
            || !handler
                .reply
                .unwrap_or(ValueType::Unit)
                .accepts(output.reply)
        {
            self.failed = true;
            return Err("entry returned an invalid scalar state or reply".into());
        }
        self.state = output.state;
        Ok(handler.reply.map(|_| output.reply))
    }
}

fn direct_context(
    cancellation: &AtomicU32,
    yield_request: &AtomicU32,
    remaining_steps: u64,
) -> ScalarContext {
    ScalarContext {
        cancellation,
        remaining_steps,
        yield_request,
        slice_remaining: u64::MAX,
        slow_path: None,
        data: std::ptr::null_mut(),
    }
}

fn check_status(status: u32) -> Result<(), String> {
    match status {
        STATUS_OK => Ok(()),
        STATUS_OVERFLOW => Err("managed integer overflow".into()),
        STATUS_DIVISION_BY_ZERO => Err("managed division by zero".into()),
        STATUS_CANCELLED => Err("managed execution cancelled".into()),
        STATUS_BUDGET_EXHAUSTED => Err("managed execution step budget exhausted".into()),
        STATUS_INVALID_CONTROL => Err("managed execution requires a valid yield callback".into()),
        _ => Err(format!("unknown scalar ABI status {status}")),
    }
}
