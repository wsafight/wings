use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use wings_abi::{
    DEFAULT_MAX_STEPS, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, STATUS_OVERFLOW,
    STATUS_STACK_EXHAUSTED, ScalarOutput, ValueType,
};
use wings_artifact::Manifest;

mod aot_heap;
mod detached;
mod execution;
mod heap;
mod ingress;
mod ingress_requests;
mod message_values;
mod module;
mod preemption;
mod resources;
mod scheduler;
mod stack_pool;
mod supervision;
mod timer;
mod wakeup;
mod worker;

#[cfg(test)]
mod tests;

pub use detached::DetachedProcess;
pub use execution::{
    ExecutionFault, ExecutionPhase, ExecutionSnapshot, ExecutionStatus, ScalarExecution, ScalarTask,
};
pub use heap::{
    ByteStateSnapshot, CollectionStats, FieldType, HeapRef, HeapStats, RecordHeap, RecordLayout,
    RecordValue, RootFrame, ShadowRoots,
};
pub use ingress::{IngressSender, IngressWake};
pub use ingress_requests::IngressRequest;
pub use module::{HandlerId, ScalarModule};
pub use preemption::{
    PreemptionCounters, PreemptionObservation, PreemptionToken, YieldObservation,
};
pub use resources::{Reservation, ResourceAccount, ResourceLevel, Resources};
pub use scheduler::{
    Scheduler, SchedulerChildId, SchedulerConfig, SchedulerExitReason, SchedulerExitSnapshot,
    SchedulerHandle, SchedulerPid, SchedulerRestartStatus, SchedulerSnapshot, SchedulerSupervisor,
    SchedulerSupervisorEvent, SchedulerTermination, SchedulerYieldSample, SchedulerYieldStatistics,
};
pub use stack_pool::{StackPool, StackPoolStats};
pub use supervision::{ChildId, RestartPolicy, RestartStatus, ScalarSupervisor, SupervisorEvent};
pub use wakeup::{WorkerNotifier, WorkerParker};
pub use worker::{
    ExitSnapshot, Pid, ProcessPhase, ProcessSnapshot, RequestId, RequestOutcome, ScalarWorker,
    WorkerEvent, WorkerWait,
};

pub struct ScalarProcess {
    module: ScalarModule,
    state: i64,
    failed: bool,
    heap: Option<Box<aot_heap::AotHeap>>,
    pool: Option<StackPool>,
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
    pub fn heap_stats(&self) -> Option<HeapStats> {
        self.heap.as_deref().map(aot_heap::AotHeap::stats)
    }

    /// # Safety
    /// The library must be trusted Wings scalar AOT code matching its manifest,
    /// with valid entry signatures, bounded pointer access and no unwinding.
    /// Entries must honor execution controls, return normally from a failed
    /// slow path without writing output, and fit the fixed execution stack.
    /// Shadow frames must reference valid live stack storage and be unlinked
    /// on every return; local references must never escape their owning heap.
    /// Code must not register external callbacks, start native background threads,
    /// or require its library to remain loaded after its last module owner drops.
    /// Shared module entries must allow concurrent calls by distinct processes
    /// and final unloading on any owner thread, without loading-thread TLS
    /// dependencies or mutable native globals.
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
        let mut execution = self.start_index(
            index,
            arguments,
            max_steps,
            cancellation,
            &PreemptionToken::default(),
        )?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::Complete(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
            }
        }
    }

    fn validate_invocation(&self, name: &str, arguments: &[i64]) -> Result<usize, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        let handler = self.module.handler(name)?;
        self.validate_handler(&handler, arguments)
    }

    fn validate_handler(&self, handler: &HandlerId, arguments: &[i64]) -> Result<usize, String> {
        self.validate_handler_kind(handler, arguments, false)
    }

    fn validate_handler_kind(
        &self,
        handler: &HandlerId,
        arguments: &[i64],
        system: bool,
    ) -> Result<usize, String> {
        self.validate_handler_payload(
            handler,
            arguments,
            system,
            0,
            &message_values::ListPayload::default(),
        )
    }

    fn validate_handler_payload(
        &self,
        handler: &HandlerId,
        arguments: &[i64],
        system: bool,
        record_mask: u64,
        lists: &message_values::ListPayload,
    ) -> Result<usize, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        if !Arc::ptr_eq(&self.module.inner, &handler.module.inner) {
            return Err("handler ID belongs to a different module".into());
        }
        let schema = handler.schema();
        if schema.is_completion() != system {
            return Err("completion events require the worker system path".into());
        }
        let expected_mask = schema
            .parameters
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, (_, ty))| {
                mask | (u64::from(*ty == ValueType::Ref) << index)
            });
        let list_mask = schema
            .parameters
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, (_, ty))| {
                mask | (u64::from(*ty == ValueType::ListRef) << index)
            });
        if record_mask != expected_mask
            || !lists.valid(arguments, list_mask)
            || arguments.len() != schema.parameters.len()
            || arguments
                .iter()
                .zip(&schema.parameters)
                .any(|(value, (_, ty))| {
                    *ty == ValueType::BytesRef
                        || (!ty.accepts(*value)
                            && !ty.is_reference()
                            && !(system && *ty == ValueType::RequestId && *value > 0))
                })
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
        if !handler
            .reply
            .unwrap_or(ValueType::Unit)
            .accepts(output.reply)
        {
            self.failed = true;
            return Err("entry returned an invalid scalar state or reply".into());
        }
        if let Err(error) = commit_state(
            self.module.inner.manifest.state,
            &mut self.heap,
            output.state,
        ) {
            self.failed = true;
            return Err(error);
        }
        self.state = output.state;
        Ok(handler.reply.map(|_| output.reply))
    }
}

fn commit_state(
    ty: ValueType,
    heap: &mut Option<Box<aot_heap::AotHeap>>,
    value: i64,
) -> Result<(), String> {
    if ty.is_reference() {
        heap.as_mut()
            .ok_or("record state requires an accounted heap")?
            .commit_typed(ty, value)
            .map_err(|error| format!("entry returned an invalid record state: {error}"))
    } else if ty.accepts(value) {
        Ok(())
    } else {
        Err("entry returned an invalid scalar state".into())
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
        STATUS_STACK_EXHAUSTED => Err("managed execution stack exhausted".into()),
        STATUS_HEAP_EXHAUSTED => Err("managed record heap exhausted".into()),
        STATUS_INVALID_ROOT => Err("managed invalid record reference or shadow roots".into()),
        wings_abi::STATUS_EMPTY_LIST => Err("managed empty IntList access".into()),
        wings_abi::STATUS_INVALID_BYTES => {
            Err("managed invalid byte buffer length, value or index".into())
        }
        wings_abi::STATUS_SEND_REJECTED => {
            Err("managed send rejected by identity, mailbox or quota".into())
        }
        wings_abi::STATUS_SPAWN_REJECTED => {
            Err("managed spawn rejected by capacity, initialization or quota".into())
        }
        wings_abi::STATUS_REQUEST_REJECTED => {
            Err("managed request rejected by protocol, capacity, identity or quota".into())
        }
        _ => Err(format!("unknown scalar ABI status {status}")),
    }
}
