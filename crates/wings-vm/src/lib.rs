use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use wings_abi::{
    DEFAULT_MAX_STEPS, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, STATUS_OVERFLOW,
    STATUS_STACK_EXHAUSTED, ScalarOutput, ValueType,
};
use wings_artifact::Manifest;

mod aot_heap;
mod bytes_reply;
mod control_flag;
mod detached;
mod execution;
mod heap;
mod host_values;
mod ingress;
mod ingress_requests;
mod list_reply;
mod message_values;
mod module;
mod module_image;
mod module_path;
mod native_stack;
mod native_sync;
mod preemption;
mod record_reply;
mod resources;
mod scheduler;
mod stack_pool;
mod startup_trace;
mod string_reply;
mod supervision;
mod timer;
mod wakeup;
mod worker;

#[cfg(test)]
mod tests;

pub use bytes_reply::BytesReply;
pub use detached::DetachedProcess;
pub use execution::{
    ExecutionFault, ExecutionPhase, ExecutionSnapshot, ExecutionStatus, ScalarExecution, ScalarTask,
};
pub use heap::{
    ByteStateSnapshot, CollectionStats, FieldType, HeapRef, HeapStats, RecordHeap, RecordLayout,
    RecordValue, RootFrame, ShadowRoots,
};
pub use host_values::MessageValue;
pub use ingress::{IngressSender, IngressWake};
pub use ingress_requests::IngressRequest;
pub use list_reply::IntListReply;
pub use module::{HandlerId, ScalarModule};
pub use preemption::{
    PreemptionCounters, PreemptionObservation, PreemptionToken, YieldObservation,
};
pub use record_reply::{RecordReply, RecordReplyValues};
pub use resources::{Reservation, ResourceAccount, ResourceLevel, Resources};
pub use scheduler::{
    Scheduler, SchedulerChildId, SchedulerConfig, SchedulerExitReason, SchedulerExitSnapshot,
    SchedulerHandle, SchedulerLockMetrics, SchedulerLockStatistics, SchedulerPid,
    SchedulerRestartStatus, SchedulerSliceOutcome, SchedulerSliceSample, SchedulerSliceStatistics,
    SchedulerSnapshot, SchedulerSupervisor, SchedulerSupervisorEvent, SchedulerTermination,
    SchedulerWorkerCpu, SchedulerYieldSample, SchedulerYieldStatistics,
};
pub use stack_pool::{StackPool, StackPoolStats};
pub use string_reply::StringReply;
pub use supervision::{ChildId, RestartPolicy, RestartStatus, ScalarSupervisor, SupervisorEvent};
pub use wakeup::{WorkerNotifier, WorkerParker};
pub use worker::{
    ExitSnapshot, Pid, ProcessPhase, ProcessSnapshot, RequestId, RequestOutcome, ScalarWorker,
    WorkerEvent, WorkerWait,
};

pub struct ScalarProcess {
    module: ScalarModule,
    account: Option<ResourceAccount>,
    state: i64,
    failed: bool,
    heap: Option<Box<aot_heap::AotHeap>>,
    pool: Option<StackPool>,
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: control_flag::ControlFlag,
}

impl CancellationToken {
    pub fn new_accounted(account: &ResourceAccount) -> Result<Self, String> {
        Ok(Self {
            flag: control_flag::ControlFlag::new_accounted(account)?,
        })
    }

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
        let index = self.validate_invocation(name, arguments)?;
        self.invoke_index(index, arguments, DEFAULT_MAX_STEPS, None)
    }

    pub fn invoke_controlled(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<i64>, String> {
        let index = self.validate_invocation(name, arguments)?;
        self.invoke_index(index, arguments, max_steps, Some(cancellation))
    }

    pub fn invoke_handler(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<i64>, String> {
        let index = self.validate_handler(handler, arguments)?;
        self.invoke_index(index, arguments, max_steps, Some(cancellation))
    }

    fn invoke_index(
        &mut self,
        index: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<i64>, String> {
        if self.module.manifest().handlers[index].reply == Some(ValueType::ListRef) {
            return Err("list reply requires invoke_list or explicit execution".into());
        }
        if self.module.manifest().handlers[index].reply == Some(ValueType::BytesRef) {
            return Err("byte reply requires invoke_bytes or explicit execution".into());
        }
        if self.module.manifest().handlers[index].reply == Some(ValueType::StringRef) {
            return Err("string reply requires invoke_string or explicit execution".into());
        }
        if matches!(
            self.module.manifest().handlers[index].reply,
            Some(ValueType::RecordRef(_))
        ) {
            return Err("named record reply requires invoke_record or explicit execution".into());
        }
        if self.module.manifest().handlers[index].reply == Some(ValueType::Pid) {
            return Err("Pid reply requires an actor worker or scheduler".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (
                cancellation.cloned().unwrap_or_default(),
                PreemptionToken::default(),
            )
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::Complete(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
                ExecutionStatus::CompleteList(_) => unreachable!("validated scalar reply"),
                ExecutionStatus::CompleteBytes(_) => unreachable!("validated scalar reply"),
                ExecutionStatus::CompleteString(_) => unreachable!("validated scalar reply"),
                ExecutionStatus::CompleteRecord(_) => unreachable!("validated scalar reply"),
            }
        }
    }

    pub fn invoke_list(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<IntListReply, String> {
        let index = self.validate_invocation(name, arguments)?;
        if self.module.manifest().handlers[index].reply != Some(ValueType::ListRef) {
            return Err("invoke_list requires an IntList reply".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (cancellation.clone(), PreemptionToken::default())
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::CompleteList(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
                ExecutionStatus::Complete(_) => unreachable!("validated list reply"),
                ExecutionStatus::CompleteBytes(_) => unreachable!("validated list reply"),
                ExecutionStatus::CompleteString(_) => unreachable!("validated list reply"),
                ExecutionStatus::CompleteRecord(_) => unreachable!("validated list reply"),
            }
        }
    }

    pub fn invoke_bytes(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<BytesReply, String> {
        let index = self.validate_invocation(name, arguments)?;
        if self.module.manifest().handlers[index].reply != Some(ValueType::BytesRef) {
            return Err("invoke_bytes requires a Bytes reply".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (cancellation.clone(), PreemptionToken::default())
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::CompleteBytes(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
                _ => unreachable!("validated byte reply"),
            }
        }
    }

    pub fn invoke_string(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<StringReply, String> {
        let index = self.validate_invocation(name, arguments)?;
        if self.module.manifest().handlers[index].reply != Some(ValueType::StringRef) {
            return Err("invoke_string requires a String reply".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (cancellation.clone(), PreemptionToken::default())
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::CompleteString(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
                _ => unreachable!("validated string reply"),
            }
        }
    }

    pub fn invoke_record(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<RecordReply, String> {
        let index = self.validate_invocation(name, arguments)?;
        if !matches!(
            self.module.manifest().handlers[index].reply,
            Some(ValueType::RecordRef(_))
        ) {
            return Err("invoke_record requires a named record reply".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (cancellation.clone(), PreemptionToken::default())
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::CompleteRecord(reply) => return Ok(reply),
                ExecutionStatus::Suspended => {}
                _ => unreachable!("validated named record reply"),
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
            &message_values::BytePayload::default(),
        )
    }

    fn validate_handler_payload(
        &self,
        handler: &HandlerId,
        arguments: &[i64],
        system: bool,
        record_mask: u64,
        lists: &message_values::ListPayload,
        bytes: &message_values::BytePayload,
    ) -> Result<usize, String> {
        self.validate_handler_payload_internal(
            handler,
            arguments,
            system,
            false,
            record_mask,
            lists,
            bytes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_handler_payload_internal(
        &self,
        handler: &HandlerId,
        arguments: &[i64],
        system: bool,
        allow_pid: bool,
        record_mask: u64,
        lists: &message_values::ListPayload,
        bytes: &message_values::BytePayload,
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
        let bytes_mask =
            schema
                .parameters
                .iter()
                .enumerate()
                .fold(0u64, |mask, (index, (_, ty))| {
                    mask | (u64::from(matches!(ty, ValueType::BytesRef | ValueType::StringRef))
                        << index)
                });
        if record_mask != expected_mask
            || !lists.valid(arguments, list_mask)
            || !bytes.valid(arguments, bytes_mask)
            || bytes.string_mask
                != schema
                    .parameters
                    .iter()
                    .enumerate()
                    .fold(0u64, |mask, (index, (_, ty))| {
                        mask | (u64::from(*ty == wings_abi::ValueType::StringRef) << index)
                    })
            || arguments.len() != schema.parameters.len()
            || arguments
                .iter()
                .zip(&schema.parameters)
                .any(|(value, (_, ty))| {
                    !ty.accepts(*value)
                        && !ty.is_reference()
                        && !(allow_pid && *ty == ValueType::Pid && *value >= 0)
                        && !(system && *ty == ValueType::RequestId && *value > 0)
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
        list: Option<IntListReply>,
        bytes: Option<string_reply::BufferReply>,
        record: Option<RecordReply>,
    ) -> Result<ExecutionStatus, String> {
        if let Err(error) = check_status(status) {
            self.failed = true;
            return Err(error);
        }
        let handler = &self.module.inner.manifest.handlers[index];
        let reply = if handler.reply == Some(ValueType::Ref) {
            match self
                .heap
                .as_deref()
                .and_then(|heap| heap.record_value(output.reply).ok())
            {
                Some(value) => value,
                None => {
                    self.failed = true;
                    return Err("entry returned an invalid record reply".into());
                }
            }
        } else if (handler.reply == Some(ValueType::ListRef) && list.is_some())
            || (matches!(handler.reply, Some(ValueType::RecordRef(_))) && record.is_some())
            || matches!(
                (handler.reply, &bytes),
                (
                    Some(ValueType::BytesRef),
                    Some(string_reply::BufferReply::Bytes(_))
                ) | (
                    Some(ValueType::StringRef),
                    Some(string_reply::BufferReply::String(_))
                )
            )
        {
            0
        } else if handler
            .reply
            .unwrap_or(ValueType::Unit)
            .accepts(output.reply)
            || (handler.reply == Some(ValueType::Pid) && output.reply >= 0)
        {
            output.reply
        } else {
            self.failed = true;
            return Err("entry returned an invalid scalar state or reply".into());
        };
        if let Err(error) = commit_state(
            self.module.inner.manifest.state,
            &mut self.heap,
            output.state,
        ) {
            self.failed = true;
            return Err(error);
        }
        self.state = output.state;
        if let Some(bytes) = bytes {
            return Ok(match bytes {
                string_reply::BufferReply::Bytes(value) => ExecutionStatus::CompleteBytes(value),
                string_reply::BufferReply::String(value) => ExecutionStatus::CompleteString(value),
            });
        }
        if let Some(record) = record {
            return Ok(ExecutionStatus::CompleteRecord(record));
        }
        match list {
            Some(list) if handler.reply == Some(ValueType::ListRef) => {
                Ok(ExecutionStatus::CompleteList(list))
            }
            _ => Ok(ExecutionStatus::Complete(handler.reply.map(|_| reply))),
        }
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
    } else if ty.accepts(value) || (ty == ValueType::Pid && value >= 0) {
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
        wings_abi::STATUS_REPLY_REJECTED => Err(
            "managed reply exceeds 64 nodes/integers or 65536 bytes, or its schema is invalid"
                .into(),
        ),
        wings_abi::STATUS_EMPTY_LIST => Err("managed empty IntList access".into()),
        wings_abi::STATUS_INVALID_BYTES => {
            Err("managed invalid byte buffer length, value or index".into())
        }
        wings_abi::STATUS_INVALID_STRING => {
            Err("managed invalid UTF-8 string or string length".into())
        }
        wings_abi::STATUS_INVALID_STRING_INDEX => {
            Err("managed string index or slice is out of bounds".into())
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
