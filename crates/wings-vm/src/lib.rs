use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use libloading::Library;
use wings_abi::{
    DEFAULT_MAX_STEPS, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_INVALID_CONTROL, STATUS_OK, STATUS_OVERFLOW, ScalarContext, ScalarEntry, ScalarOutput,
    ValueType,
};
use wings_artifact::{
    INITIALIZER_SYMBOL, MAX_LIBRARY_BYTES, Manifest, handler_symbol, library_name,
};

mod execution;
mod stack_pool;

pub use execution::{ExecutionStatus, PreemptionToken, ScalarExecution};
pub use stack_pool::{StackPool, StackPoolStats};

pub struct ScalarProcess {
    manifest: Manifest,
    entries: Vec<ScalarEntry>,
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
        let directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let manifest = Manifest::read(&directory)?;
        let library_path = directory.join(library_name());
        let metadata = library_path
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES {
            return Err("artifact library must be a regular file of at most 64 MiB".into());
        }
        let library = unsafe { Library::new(&library_path) }.map_err(|error| error.to_string())?;
        let initializer = unsafe {
            *library
                .get::<ScalarEntry>(INITIALIZER_SYMBOL.as_bytes())
                .map_err(|error| error.to_string())?
        };
        let mut entries = Vec::new();
        for index in 0..manifest.handlers.len() {
            entries.push(unsafe {
                *library
                    .get::<ScalarEntry>(handler_symbol(index).as_bytes())
                    .map_err(|error| error.to_string())?
            });
        }
        std::mem::forget(library);
        let mut output = ScalarOutput::default();
        let cancellation = AtomicU32::new(0);
        let yield_request = AtomicU32::new(0);
        let mut context = ScalarContext {
            cancellation: &cancellation,
            remaining_steps: DEFAULT_MAX_STEPS,
            yield_request: &yield_request,
            slice_remaining: u64::MAX,
            slow_path: None,
            data: std::ptr::null_mut(),
        };
        let status = unsafe { initializer(0, [].as_ptr(), &mut output, &mut context) };
        check_status(status)?;
        if !manifest.state.accepts(output.state) {
            return Err("initializer returned an invalid scalar state".into());
        }
        Ok(Self {
            manifest,
            entries,
            state: output.state,
            failed: false,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
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
        let mut output = ScalarOutput::default();
        let yield_request = AtomicU32::new(0);
        let mut context = ScalarContext {
            cancellation: Arc::as_ptr(&cancellation.flag),
            remaining_steps: max_steps,
            yield_request: &yield_request,
            slice_remaining: u64::MAX,
            slow_path: None,
            data: std::ptr::null_mut(),
        };
        let status = unsafe {
            (self.entries[index])(self.state, arguments.as_ptr(), &mut output, &mut context)
        };
        self.finish_invocation(index, status, output)
    }

    fn validate_invocation(&self, name: &str, arguments: &[i64]) -> Result<usize, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        let index = self
            .manifest
            .handlers
            .iter()
            .position(|handler| handler.name == name)
            .ok_or_else(|| format!("unknown handler {name}"))?;
        let handler = &self.manifest.handlers[index];
        if arguments.len() != handler.parameters.len()
            || arguments
                .iter()
                .zip(&handler.parameters)
                .any(|(value, (_, ty))| !ty.accepts(*value))
        {
            return Err("handler argument count or scalar type mismatch".into());
        }
        Ok(index)
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
        let handler = &self.manifest.handlers[index];
        if !self.manifest.state.accepts(output.state)
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
