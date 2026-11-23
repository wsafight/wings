use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn new_coroutine(
    entry: ScalarEntry,
    state: i64,
    mut owned_arguments: InvocationArgs,
    max_steps: u64,
    cancellation: &CancellationToken,
    preemption: &PreemptionToken,
    stack: PooledStack,
    heap: &mut Option<Box<AotHeap>>,
    effects_enabled: bool,
    lists: ListPayload,
    bytes: &mut BytePayload,
    module: Option<ScalarModule>,
    output_types: [wings_abi::ValueType; 2],
) -> Result<ExecutionCoroutine, String> {
    let stack_limit = stack.managed_limit();
    let cancellation = cancellation.clone();
    let preemption = preemption.clone();
    let mut heap = heap.take();
    let bytes = std::mem::take(bytes);
    let coroutine = Coroutine::with_stack(stack, move |yielder, command| {
        let mut output = ScalarOutput::default();
        let ResumeCommand::Run(slice_remaining) = command else {
            let checkpoint = Checkpoint {
                remaining_steps: max_steps,
                heap: heap.as_deref().map(AotHeap::stats),
                effect: None,
                yield_requested: None,
            };
            return (STATUS_CANCELLED, output, heap, checkpoint, None, None, None);
        };
        let mut control = InvocationControl {
            yielder,
            preemption: &preemption,
        };
        let mut context = ScalarContext {
            cancellation: &*cancellation.flag,
            remaining_steps: max_steps,
            yield_request: &*preemption.flag,
            slice_remaining,
            slow_path: Some(suspend_scalar),
            data: std::ptr::from_mut(&mut control).cast(),
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
        let imported = unsafe {
            crate::aot_heap::import_arguments(
                &mut context,
                owned_arguments.as_mut_slice(),
                &lists,
                &bytes,
            )
        };
        drop(bytes);
        let status = match imported {
            Ok(()) => unsafe { entry(state, owned_arguments.as_ptr(), &mut output, &mut context) },
            Err(status) => status,
        };
        let mut status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        let mut list = None;
        let mut bytes = None;
        let mut record = None;
        if status == STATUS_OK && output_types[1] == wings_abi::ValueType::ListRef {
            let roots = [output.reply as u64, output.state as u64];
            let frame = wings_abi::ShadowFrame {
                previous: std::ptr::null(),
                slots: roots.as_ptr(),
                count: 1 + usize::from(output_types[0].is_reference()),
            };
            context.roots = &frame;
            let mut arguments = [output.reply];
            match unsafe { crate::aot_heap::export_lists(&mut context, &mut arguments, 1) } {
                Ok(payload) => {
                    let copied = heap
                        .as_ref()
                        .ok_or(wings_abi::STATUS_INVALID_ROOT)
                        .and_then(|heap| {
                            IntListReply::copy(&heap.account, payload.values.as_slice())
                                .map_err(|_| wings_abi::STATUS_HEAP_EXHAUSTED)
                        });
                    match copied {
                        Ok(value) => list = Some(value),
                        Err(error) => status = error,
                    }
                }
                Err(wings_abi::STATUS_SEND_REJECTED) => status = wings_abi::STATUS_REPLY_REJECTED,
                Err(error) => status = error,
            }
            context.roots = std::ptr::null();
        }
        if status == STATUS_OK {
            if let wings_abi::ValueType::RecordRef(schema) = output_types[1] {
                let roots = [output.reply as u64, output.state as u64];
                let frame = wings_abi::ShadowFrame {
                    previous: std::ptr::null(),
                    slots: roots.as_ptr(),
                    count: 1 + usize::from(output_types[0].is_reference()),
                };
                context.roots = &frame;
                let exported = module
                    .as_ref()
                    .ok_or(wings_abi::STATUS_INVALID_ROOT)
                    .and_then(|module| unsafe {
                        crate::aot_heap::export_record_reply(
                            &mut context,
                            output.reply,
                            schema,
                            &module.manifest().records,
                        )
                    });
                match exported {
                    Ok(value) => record = Some(value),
                    Err(error) => status = error,
                }
                context.roots = std::ptr::null();
            }
        }
        if status == STATUS_OK
            && matches!(
                output_types[1],
                wings_abi::ValueType::BytesRef | wings_abi::ValueType::StringRef
            )
        {
            let roots = [output.reply as u64, output.state as u64];
            let frame = wings_abi::ShadowFrame {
                previous: std::ptr::null(),
                slots: roots.as_ptr(),
                count: 1 + usize::from(output_types[0].is_reference()),
            };
            context.roots = &frame;
            let mut arguments = [output.reply];
            let is_string = output_types[1] == wings_abi::ValueType::StringRef;
            match unsafe {
                crate::aot_heap::export_buffers(
                    &mut context,
                    &mut arguments,
                    u64::from(!is_string),
                    u64::from(is_string),
                )
            } {
                Ok(payload) => {
                    let account = heap.as_ref().map(|heap| heap.account.clone());
                    let result =
                        account
                            .ok_or(wings_abi::STATUS_INVALID_ROOT)
                            .and_then(|account| {
                                if is_string {
                                    crate::StringReply::from_payload(
                                        &account,
                                        payload,
                                        &mut context,
                                    )
                                    .map(BufferReply::String)
                                } else {
                                    BytesReply::from_payload(&account, payload)
                                        .map(BufferReply::Bytes)
                                        .map_err(|_| wings_abi::STATUS_HEAP_EXHAUSTED)
                                }
                            });
                    match result {
                        Ok(value) => bytes = Some(value),
                        Err(error) => status = error,
                    }
                }
                Err(error) => status = error,
            }
            context.roots = std::ptr::null();
        }
        let checkpoint = Checkpoint {
            remaining_steps: context.remaining_steps,
            heap: heap.as_deref().map(AotHeap::stats),
            effect: None,
            yield_requested: None,
        };
        if let Some(heap) = &mut heap {
            heap.clear_incoming();
        }
        (status, output, heap, checkpoint, list, bytes, record)
    });
    Ok(coroutine)
}

pub(super) fn resume_invocation(
    process: &mut ScalarProcess,
    invocation: &mut Option<Invocation>,
    last: &mut Option<ExecutionSnapshot>,
    max_slice_steps: u64,
    fibers: Option<&mut WorkerFibers>,
) -> Result<ExecutionStatus, String> {
    let active = invocation
        .as_mut()
        .ok_or("scalar execution has already completed or was not started")?;
    if max_slice_steps == 0 {
        return Err("execution time slice must contain at least one step".into());
    }
    let acknowledgement = active.acknowledgement.take();
    match active.backend.as_mut().expect("active invocation backend") {
        InvocationBackend::OneShot(coroutine) => {
            let command = match acknowledgement {
                Some((status, value)) => ResumeCommand::Effect(status, value, max_slice_steps),
                None => ResumeCommand::Run(max_slice_steps),
            };
            match coroutine.resume(command) {
                CoroutineResult::Yield(checkpoint) => {
                    if let Some(requested_at) = checkpoint.yield_requested {
                        active.preemption.confirm_yield(requested_at);
                    }
                    active.snapshot.phase = ExecutionPhase::Suspended;
                    active.snapshot.remaining_steps = checkpoint.remaining_steps;
                    active.snapshot.heap = checkpoint.heap;
                    active.effect = checkpoint.effect;
                    Ok(ExecutionStatus::Suspended)
                }
                CoroutineResult::Return((
                    status,
                    output,
                    heap,
                    checkpoint,
                    list,
                    bytes,
                    record,
                )) => {
                    let handler = active.handler;
                    let mut snapshot = active.snapshot;
                    snapshot.remaining_steps = checkpoint.remaining_steps;
                    snapshot.heap = checkpoint.heap;
                    active.backend.take();
                    invocation.take();
                    process.heap = heap;
                    let result =
                        process.finish_invocation(handler, status, output, list, bytes, record);
                    snapshot.phase = if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    };
                    snapshot.fault = if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    };
                    *last = Some(snapshot);
                    result
                }
            }
        }
        InvocationBackend::Reusable(reusable) => {
            let command = match acknowledgement {
                Some((status, value)) => FiberCommand::Effect(status, value, max_slice_steps),
                None => FiberCommand::Run(max_slice_steps),
            };
            let step = reusable
                .coroutine
                .as_mut()
                .expect("active reusable coroutine")
                .resume(command);
            match step {
                CoroutineResult::Yield(FiberEvent::Suspended(checkpoint)) => {
                    if let Some(requested_at) = checkpoint.yield_requested {
                        active.preemption.confirm_yield(requested_at);
                    }
                    active.snapshot.phase = ExecutionPhase::Suspended;
                    active.snapshot.remaining_steps = checkpoint.remaining_steps;
                    active.snapshot.heap = checkpoint.heap;
                    active.effect = checkpoint.effect;
                    Ok(ExecutionStatus::Suspended)
                }
                CoroutineResult::Yield(FiberEvent::Complete {
                    status,
                    output,
                    heap,
                    checkpoint,
                }) => {
                    let coroutine = reusable.coroutine.take().expect("completed coroutine");
                    let stack_charge = reusable
                        .stack_charge
                        .take()
                        .expect("completed stack charge");
                    let pool = reusable.pool.clone();
                    if let Some(fibers) = fibers {
                        fibers.recycle(coroutine, stack_charge, pool);
                    } else {
                        let mut shutdown = coroutine;
                        if shutdown.started()
                            && !shutdown.done()
                            && !matches!(
                                shutdown.resume(FiberCommand::Shutdown),
                                CoroutineResult::Return(())
                            )
                        {
                            std::process::abort();
                        }
                        pool.drop_active_fiber();
                        drop(stack_charge);
                    }
                    let handler = active.handler;
                    let mut snapshot = active.snapshot;
                    snapshot.remaining_steps = checkpoint.remaining_steps;
                    snapshot.heap = checkpoint.heap;
                    active.backend.take();
                    invocation.take();
                    process.heap = heap;
                    let result =
                        process.finish_invocation(handler, status, output, None, None, None);
                    snapshot.phase = if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    };
                    snapshot.fault = if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    };
                    *last = Some(snapshot);
                    result
                }
                CoroutineResult::Yield(FiberEvent::Primed) | CoroutineResult::Return(()) => {
                    std::process::abort()
                }
            }
        }
    }
}

pub(super) unsafe extern "C" fn suspend_reusable(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<ReusableControl<'_>>() };
    let yield_requested = control.preemption.take_request();
    let checkpoint = Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: None,
        yield_requested,
    };
    match control.yielder.suspend(FiberEvent::Suspended(checkpoint)) {
        FiberCommand::Run(steps) => {
            context.slice_remaining = steps;
            STATUS_OK
        }
        _ => STATUS_CANCELLED,
    }
}

unsafe extern "C" fn suspend_scalar(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<InvocationControl<'_>>() };
    let yield_requested = control.preemption.take_request();
    let checkpoint = Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: None,
        yield_requested,
    };
    match control.yielder.suspend(checkpoint) {
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
        if unsafe { &*context.cancellation }.load(Ordering::Acquire) != 0 {
            return Err(STATUS_CANCELLED);
        }
        if context.remaining_steps == 0 {
            return Err(wings_abi::STATUS_BUDGET_EXHAUSTED);
        }
        if context.slice_remaining == 0
            || unsafe { &*context.yield_request }.load(Ordering::Acquire) != 0
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

pub(crate) fn runtime_checkpoint_batch(
    context: &mut ScalarContext,
    max_steps: usize,
) -> Result<usize, u32> {
    if context.cancellation.is_null() || context.yield_request.is_null() {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    if unsafe { &*context.cancellation }.load(Ordering::Acquire) == 0
        && unsafe { &*context.yield_request }.load(Ordering::Acquire) == 0
        && context.remaining_steps > 0
        && context.slice_remaining > 0
    {
        let steps = max_steps
            .max(1)
            .min(usize::try_from(context.remaining_steps).unwrap_or(usize::MAX))
            .min(usize::try_from(context.slice_remaining).unwrap_or(usize::MAX));
        context.remaining_steps -= steps as u64;
        context.slice_remaining -= steps as u64;
        return Ok(steps);
    }
    runtime_checkpoint(context, true)?;
    Ok(1)
}

unsafe fn prepare_effect_checkpoint(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> Result<Checkpoint, u32> {
    let Some(request) = (unsafe { request.as_ref() }) else {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    };
    let count = request.count;
    let arguments = request.arguments;
    if output.is_null()
        || count > 64
        || (count != 0 && arguments.is_null())
        || ((request.list_mask | request.bytes_mask | request.string_mask) != 0
            && !matches!(
                request.operation,
                wings_abi::EFFECT_SEND_SELF
                    | wings_abi::EFFECT_SEND_AFTER_SELF
                    | wings_abi::EFFECT_SEND
                    | wings_abi::EFFECT_SEND_ACTOR
                    | wings_abi::EFFECT_SEND_AFTER
                    | wings_abi::EFFECT_SEND_AFTER_ACTOR
                    | wings_abi::EFFECT_REQUEST
                    | wings_abi::EFFECT_REQUEST_ACTOR
            ))
    {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    let context = unsafe { &mut *context };
    let mut copied = if count == 0 {
        MessageArguments::default()
    } else {
        MessageArguments::try_from_slice(unsafe { std::slice::from_raw_parts(arguments, count) })
            .map_err(|_| wings_abi::STATUS_HEAP_EXHAUSTED)?
    };
    let buffer_mask = request.bytes_mask | request.string_mask;
    if request.bytes_mask & request.string_mask != 0
        || request.list_mask & buffer_mask != 0
        || (count < 64 && (request.list_mask | buffer_mask) >> count != 0)
    {
        return Err(wings_abi::STATUS_INVALID_CONTROL);
    }
    let lists = unsafe {
        crate::aot_heap::export_lists(context, copied.as_mut_slice(), request.list_mask)
    }?;
    let bytes = unsafe {
        crate::aot_heap::export_buffers(
            context,
            copied.as_mut_slice(),
            request.bytes_mask,
            request.string_mask,
        )
    }?;
    let boxed_effect = {
        let layout = std::alloc::Layout::new::<ProcessEffect>();
        let pointer = unsafe { std::alloc::alloc(layout) }.cast::<ProcessEffect>();
        if pointer.is_null() {
            return Err(wings_abi::STATUS_HEAP_EXHAUSTED);
        }
        unsafe {
            pointer.write(ProcessEffect {
                operation: request.operation,
                target: request.target,
                handler: request.handler as usize,
                arguments: copied,
                callback: request.callback as usize,
                timeout_ms: request.timeout_ms,
                lists,
                bytes,
            });
            Box::from_raw(pointer)
        }
    };
    Ok(Checkpoint {
        remaining_steps: context.remaining_steps,
        heap: unsafe { context.heap_data.cast::<AotHeap>().as_ref() }.map(AotHeap::stats),
        effect: Some(boxed_effect),
        yield_requested: None,
    })
}

pub(super) unsafe extern "C" fn send_self(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> u32 {
    let checkpoint = match unsafe { prepare_effect_checkpoint(context, request, output) } {
        Ok(checkpoint) => checkpoint,
        Err(status) => return status,
    };
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<InvocationControl<'_>>() };
    match control.yielder.suspend(checkpoint) {
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

pub(super) unsafe extern "C" fn send_self_reusable(
    context: *mut ScalarContext,
    request: *const wings_abi::EffectRequest,
    output: *mut i64,
) -> u32 {
    let checkpoint = match unsafe { prepare_effect_checkpoint(context, request, output) } {
        Ok(checkpoint) => checkpoint,
        Err(status) => return status,
    };
    let context = unsafe { &mut *context };
    let control = unsafe { &*context.data.cast::<ReusableControl<'_>>() };
    match control.yielder.suspend(FiberEvent::Suspended(checkpoint)) {
        FiberCommand::Effect(status, value, steps) => {
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
