use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use wings_abi::{
    DEFAULT_MAX_STEPS, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, STATUS_OVERFLOW,
    STATUS_STACK_EXHAUSTED, ScalarOutput, ValueType,
};
use wings_artifact::Manifest;

mod allocator;
mod aot_heap;
mod control_flag;
mod detached;
mod error;
mod execution;
mod heap;
mod host_values;
mod ingress;
mod ingress_requests;
mod message_values;
mod module;
mod module_image;
mod module_path;
mod native_memory;
mod native_stack;
mod native_sync;
mod preemption;
mod record_reply;
mod reply;
mod resources;
mod scheduler;
mod stack_pool;
mod startup_trace;
mod supervision;
mod timer;
mod wakeup;
mod worker;

#[cfg(test)]
mod tests;

pub use allocator::{
    NativeAllocatorStats, WingsAllocator, allocator_usable_size, global_allocator_stats,
    set_global_allocator_limit,
};
pub use detached::DetachedProcess;
pub use error::VmError;
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
pub use module::{HandlerId, ScalarModule};
pub use native_memory::{
    NativeMemoryLedger, NativeProcessMemory, native_memory_ledger, sample_process_memory,
};
pub use preemption::{
    PreemptionCounters, PreemptionObservation, PreemptionToken, YieldObservation,
};
pub use record_reply::{RecordReply, RecordReplyValues};
pub use reply::{BytesReply, IntListReply, ReplyValue, StringReply};
pub use resources::{Reservation, ResourceAccount, ResourceLevel, Resources};
pub use scheduler::{
    Scheduler, SchedulerChildId, SchedulerConfig, SchedulerExitReason, SchedulerExitSnapshot,
    SchedulerHandle, SchedulerLockMetrics, SchedulerLockStatistics, SchedulerPid,
    SchedulerRestartStatus, SchedulerSliceOutcome, SchedulerSliceSample, SchedulerSliceStatistics,
    SchedulerSnapshot, SchedulerSupervisor, SchedulerSupervisorEvent, SchedulerTermination,
    SchedulerWorkerCpu, SchedulerYieldSample, SchedulerYieldStatistics,
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

    pub fn invoke_value(
        &mut self,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<Option<ReplyValue>, String> {
        let index = self.validate_handler(handler, arguments)?;
        let reply_type = self.module.manifest().handlers[index].reply;
        if reply_type == Some(ValueType::Pid) {
            return Err("Pid reply requires an actor worker or scheduler".into());
        }
        let mut execution = self.start_index(index, arguments, max_steps, || {
            (cancellation.clone(), PreemptionToken::default())
        })?;
        loop {
            match execution.resume(u64::MAX)? {
                ExecutionStatus::Suspended => {}
                done => return done.into_reply_value(reply_type).map_err(Into::into),
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
        let handler = self.module.handler(name)?;
        if handler.schema().reply != Some(ValueType::ListRef) {
            let _ = self.validate_handler(&handler, arguments)?;
            return Err("invoke_list requires an IntList reply".into());
        }
        match self.invoke_value(&handler, arguments, max_steps, cancellation)? {
            Some(ReplyValue::IntList(reply)) => Ok(reply),
            _ => unreachable!("validated list reply"),
        }
    }

    pub fn invoke_bytes(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<BytesReply, String> {
        let handler = self.module.handler(name)?;
        if handler.schema().reply != Some(ValueType::BytesRef) {
            let _ = self.validate_handler(&handler, arguments)?;
            return Err("invoke_bytes requires a Bytes reply".into());
        }
        match self.invoke_value(&handler, arguments, max_steps, cancellation)? {
            Some(ReplyValue::Bytes(reply)) => Ok(reply),
            _ => unreachable!("validated byte reply"),
        }
    }

    pub fn invoke_string(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<StringReply, String> {
        let handler = self.module.handler(name)?;
        if handler.schema().reply != Some(ValueType::StringRef) {
            let _ = self.validate_handler(&handler, arguments)?;
            return Err("invoke_string requires a String reply".into());
        }
        match self.invoke_value(&handler, arguments, max_steps, cancellation)? {
            Some(ReplyValue::String(reply)) => Ok(reply),
            _ => unreachable!("validated string reply"),
        }
    }

    pub fn invoke_record(
        &mut self,
        name: &str,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
    ) -> Result<RecordReply, String> {
        let handler = self.module.handler(name)?;
        if !matches!(handler.schema().reply, Some(ValueType::RecordRef(_))) {
            let _ = self.validate_handler(&handler, arguments)?;
            return Err("invoke_record requires a named record reply".into());
        }
        match self.invoke_value(&handler, arguments, max_steps, cancellation)? {
            Some(ReplyValue::Record(reply)) => Ok(reply),
            _ => unreachable!("validated named record reply"),
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
        self.validate_handler_payload_internal(
            handler,
            arguments,
            system,
            false,
            &message_values::ListPayload::default(),
            &message_values::BytePayload::default(),
        )
    }

    pub(crate) fn validate_handler_payload_internal(
        &self,
        handler: &HandlerId,
        arguments: &[i64],
        system: bool,
        allow_pid: bool,
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
        message_values::validate_message_payload(
            &schema.parameters,
            arguments,
            system,
            allow_pid,
            lists,
            bytes,
        )
        .map_err(|_| String::from("handler argument count or scalar type mismatch"))?;
        Ok(handler.index)
    }

    fn finish_invocation(
        &mut self,
        index: usize,
        status: u32,
        output: ScalarOutput,
        list: Option<IntListReply>,
        bytes: Option<reply::BufferReply>,
        record: Option<RecordReply>,
    ) -> Result<ExecutionStatus, String> {
        if let Err(error) = check_status(status) {
            self.failed = true;
            return Err(error);
        }
        let handler = &self.module.inner.manifest.handlers[index];
        let reply = if (handler.reply == Some(ValueType::ListRef) && list.is_some())
            || (matches!(handler.reply, Some(ValueType::RecordRef(_))) && record.is_some())
            || matches!(
                (handler.reply, &bytes),
                (
                    Some(ValueType::BytesRef),
                    Some(reply::BufferReply::Bytes(_))
                ) | (
                    Some(ValueType::StringRef),
                    Some(reply::BufferReply::String(_))
                )
            ) {
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
                reply::BufferReply::Bytes(value) => ExecutionStatus::CompleteBytes(value),
                reply::BufferReply::String(value) => ExecutionStatus::CompleteString(value),
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
