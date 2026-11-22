use std::sync::atomic::AtomicU32;

use super::abi::send_self;
use super::*;

#[test]
fn fixed_core_invocation_reservation_covers_owned_payload_and_control() {
    let maximum = std::mem::size_of::<Invocation>()
        + std::mem::size_of::<ExecutionCoroutine>()
        + std::mem::size_of::<Checkpoint>()
        + std::mem::size_of::<ProcessEffect>()
        + std::mem::size_of::<InvocationControl<'_>>()
        + std::mem::size_of::<ListPayload>()
        + std::mem::size_of::<BytePayload>()
        + 64 * std::mem::size_of::<i64>()
        + 2 * crate::control_flag::ControlFlag::UNACCOUNTED_BYTES
        + 256;
    assert!(
        maximum as u64 <= INVOCATION_OVERHEAD,
        "invocation control needs {maximum} bytes"
    );
}

#[test]
fn effect_reference_masks_reject_overlap_missing_heap_and_invalid_bits_before_yield() {
    let cancelled = AtomicU32::new(0);
    let mut context = ScalarContext {
        cancellation: &cancelled,
        remaining_steps: 1,
        yield_request: &cancelled,
        slice_remaining: 1,
        slow_path: None,
        data: std::ptr::null_mut(),
        stack_limit: 0,
        roots: std::ptr::null(),
        heap_call: None,
        heap_data: std::ptr::null_mut(),
        effect_call: None,
    };
    let arguments = [1];
    let mut request = wings_abi::EffectRequest {
        operation: wings_abi::EFFECT_SEND,
        handler: 0,
        target: 0,
        arguments: arguments.as_ptr(),
        count: 1,
        callback: 0,
        timeout_ms: 0,
        list_mask: 2,
        bytes_mask: 0,
        string_mask: 0,
    };
    let mut output = 777;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_CONTROL
    );
    request.list_mask = 0;
    request.bytes_mask = 2;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_CONTROL
    );
    request.bytes_mask = 1;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_ROOT
    );
    request.list_mask = 1;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_CONTROL
    );
    request.bytes_mask = 0;
    request.list_mask = 0;
    context.remaining_steps = 8;
    context.slice_remaining = 8;
    request.string_mask = 2;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_CONTROL
    );
    request.string_mask = 1;
    request.bytes_mask = 1;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_CONTROL
    );
    request.bytes_mask = 0;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_ROOT
    );
    request.string_mask = 0;
    context.remaining_steps = 8;
    context.slice_remaining = 8;
    request.list_mask = 1;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_ROOT
    );
    let all_arguments = [1; 64];
    request.arguments = all_arguments.as_ptr();
    request.count = 64;
    request.list_mask = u64::MAX;
    assert_eq!(
        unsafe { send_self(&mut context, &request, &mut output) },
        wings_abi::STATUS_INVALID_ROOT
    );
    for operation in [
        wings_abi::EFFECT_SELF_PID,
        wings_abi::EFFECT_SPAWN_SELF,
        wings_abi::EFFECT_CANCEL_REQUEST,
        u32::MAX,
    ] {
        request.operation = operation;
        request.list_mask = u64::MAX;
        assert_eq!(
            unsafe { send_self(&mut context, &request, &mut output) },
            wings_abi::STATUS_INVALID_CONTROL
        );
    }
    assert_eq!(output, 777);
}

unsafe extern "C" fn resume_runtime_chunk(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    unsafe { &*context.yield_request }.store(0, Ordering::SeqCst);
    context.slice_remaining = 1;
    STATUS_OK
}

#[test]
fn runtime_chunks_recheck_cancellation_budget_and_atomic_yield_before_work() {
    let cancellation = AtomicU32::new(0);
    let request = AtomicU32::new(1);
    let mut context = ScalarContext {
        cancellation: &cancellation,
        remaining_steps: 2,
        yield_request: &request,
        slice_remaining: 0,
        slow_path: Some(resume_runtime_chunk),
        data: std::ptr::null_mut(),
        stack_limit: 0,
        roots: std::ptr::null(),
        heap_call: None,
        heap_data: std::ptr::null_mut(),
        effect_call: None,
    };
    assert_eq!(runtime_checkpoint(&mut context, false), Ok(()));
    assert_eq!(
        (
            context.remaining_steps,
            context.slice_remaining,
            request.load(Ordering::SeqCst)
        ),
        (2, 1, 0)
    );
    assert_eq!(runtime_checkpoint(&mut context, true), Ok(()));
    assert_eq!((context.remaining_steps, context.slice_remaining), (1, 0));
    assert_eq!(runtime_checkpoint(&mut context, true), Ok(()));
    assert_eq!((context.remaining_steps, context.slice_remaining), (0, 0));
    assert_eq!(
        runtime_checkpoint(&mut context, true),
        Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
    );
    cancellation.store(1, Ordering::SeqCst);
    assert_eq!(
        runtime_checkpoint(&mut context, true),
        Err(STATUS_CANCELLED)
    );
    cancellation.store(0, Ordering::SeqCst);
    context.remaining_steps = 1;
    context.slow_path = None;
    assert_eq!(
        runtime_checkpoint(&mut context, true),
        Err(wings_abi::STATUS_INVALID_CONTROL)
    );
    context.cancellation = std::ptr::null();
    assert_eq!(
        runtime_checkpoint(&mut context, true),
        Err(wings_abi::STATUS_INVALID_CONTROL)
    );
}

#[test]
fn faults_classify_raw_abi_statuses_without_parsing_error_text() {
    assert_eq!(ExecutionFault::from_status(STATUS_OK), None);
    for (status, fault) in [
        (wings_abi::STATUS_OVERFLOW, ExecutionFault::Overflow),
        (
            wings_abi::STATUS_DIVISION_BY_ZERO,
            ExecutionFault::DivisionByZero,
        ),
        (STATUS_CANCELLED, ExecutionFault::Cancelled),
        (
            wings_abi::STATUS_BUDGET_EXHAUSTED,
            ExecutionFault::BudgetExhausted,
        ),
        (
            wings_abi::STATUS_INVALID_CONTROL,
            ExecutionFault::InvalidControl,
        ),
        (
            wings_abi::STATUS_STACK_EXHAUSTED,
            ExecutionFault::StackExhausted,
        ),
        (
            wings_abi::STATUS_HEAP_EXHAUSTED,
            ExecutionFault::HeapExhausted,
        ),
        (wings_abi::STATUS_INVALID_ROOT, ExecutionFault::InvalidRoot),
        (wings_abi::STATUS_EMPTY_LIST, ExecutionFault::EmptyList),
        (
            wings_abi::STATUS_SEND_REJECTED,
            ExecutionFault::SendRejected,
        ),
        (
            wings_abi::STATUS_SPAWN_REJECTED,
            ExecutionFault::SpawnRejected,
        ),
        (
            wings_abi::STATUS_REQUEST_REJECTED,
            ExecutionFault::RequestRejected,
        ),
        (u32::MAX, ExecutionFault::UnknownStatus(u32::MAX)),
    ] {
        assert_eq!(ExecutionFault::from_status(status), Some(fault));
    }
}
