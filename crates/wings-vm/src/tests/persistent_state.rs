use super::*;
use wings_abi::{HEAP_COLLECT, HEAP_READ_INT, HEAP_RECORD_INT};

pub(super) unsafe extern "C" fn record_initializer(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let mut reference = 0;
    let status =
        unsafe { ((*context).heap_call.unwrap())(context, HEAP_RECORD_INT, 7, &mut reference) };
    if status == STATUS_OK {
        unsafe { (*output).state = reference as i64 };
    }
    status
}

unsafe extern "C" fn invalid_record_initializer(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe { (*output).state = 0 };
    STATUS_OK
}

unsafe extern "C" fn candidate(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let mut reference = 0;
    let status =
        unsafe { ((*context).heap_call.unwrap())(context, HEAP_RECORD_INT, 99, &mut reference) };
    if status == STATUS_OK {
        unsafe {
            *output = ScalarOutput {
                state: reference as i64,
                reply: *arguments,
            }
        };
    }
    status
}

unsafe extern "C" fn stale_candidate(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let status = unsafe { candidate(state, arguments, output, context) };
    if status != STATUS_OK {
        return status;
    }
    let mut ignored = 0;
    unsafe { ((*context).heap_call.unwrap())(context, HEAP_COLLECT, 0, &mut ignored) }
}

unsafe extern "C" fn failed_candidate(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let status = unsafe { candidate(state, arguments, output, context) };
    if status == STATUS_OK {
        STATUS_DIVISION_BY_ZERO
    } else {
        status
    }
}

fn record_module(entry: ScalarEntry) -> ScalarModule {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::Ref;
    data.manifest.handlers[1].reply = Some(ValueType::Bool);
    data.initializer = record_initializer;
    data.entries[1] = entry;
    module
}

#[test]
fn record_state_without_heap_is_rejected_before_entering_initializer() {
    let module = record_module(candidate);
    assert!(
        module
            .instantiate()
            .err()
            .unwrap()
            .contains("accounted heap")
    );
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    assert!(
        module
            .instantiate_in_pool(&bound)
            .err()
            .unwrap()
            .contains("accounted heap")
    );
    assert_eq!(pool.stats().active, 0);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn invalid_initializer_record_releases_heap_and_stack_reservations() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut module = record_module(candidate);
    Arc::get_mut(&mut module.inner).unwrap().initializer = invalid_record_initializer;
    assert!(
        module
            .instantiate_with_heap(&bound, &account, 2)
            .err()
            .unwrap()
            .contains("invalid record state")
    );
    assert_eq!(vm.used(), baseline);
    assert_eq!(pool.stats().active, 0);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn invalid_reply_stale_state_and_managed_failures_never_replace_committed_root() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    for (entry, reply, expected) in [
        (candidate as ScalarEntry, 2, "invalid scalar state or reply"),
        (stale_candidate as ScalarEntry, 0, "invalid record state"),
        (failed_candidate as ScalarEntry, 0, "division by zero"),
    ] {
        let module = record_module(entry);
        let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
        let committed = process.state;
        assert!(
            process
                .invoke("Bump", &[reply])
                .unwrap_err()
                .contains(expected)
        );
        assert!(process.failed);
        assert_eq!(process.state, committed);
        let heap = process.heap.as_mut().unwrap();
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = ScalarContext {
            effect_call: None,
            cancellation: &flag,
            remaining_steps: 128,
            yield_request: &flag,
            slice_remaining: 128,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: Some(crate::aot_heap::heap_call),
            heap_data: std::ptr::from_mut(&mut **heap).cast(),
        };
        let mut ignored = 0;
        assert_eq!(
            unsafe { crate::aot_heap::heap_call(&mut context, HEAP_COLLECT, 0, &mut ignored) },
            STATUS_OK
        );
        assert_eq!(process.heap_stats().unwrap().allocated, 1);
        assert_eq!(
            unsafe {
                crate::aot_heap::heap_call(
                    &mut context,
                    HEAP_READ_INT,
                    committed as u64,
                    &mut ignored,
                )
            },
            STATUS_OK
        );
        assert_eq!(ignored, 7);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

unsafe extern "C" fn named_initializer(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let arguments = [0u64, 7];
    let mut reference = 0;
    let status = unsafe {
        ((*context).heap_call.unwrap())(
            context,
            wings_abi::HEAP_RECORD_NEW,
            arguments.as_ptr() as usize as u64,
            &mut reference,
        )
    };
    if status == STATUS_OK {
        unsafe {
            *output = ScalarOutput {
                state: reference as i64,
                reply: 0,
            }
        };
    }
    status
}

unsafe extern "C" fn named_candidate(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let mode = unsafe { *arguments };
    let values = [u64::from(mode == 0), 99];
    let mut reference = 0;
    let status = unsafe {
        ((*context).heap_call.unwrap())(
            context,
            wings_abi::HEAP_RECORD_NEW,
            values.as_ptr() as usize as u64,
            &mut reference,
        )
    };
    if status != STATUS_OK {
        return status;
    }
    unsafe {
        *output = ScalarOutput {
            state: reference as i64,
            reply: i64::from(mode == 2) * 2,
        }
    };
    if mode == 1 {
        STATUS_DIVISION_BY_ZERO
    } else {
        STATUS_OK
    }
}

#[test]
fn named_state_rejects_nominal_mismatch_and_failed_candidates_without_losing_old_root() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut module = record_module(named_candidate);
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.records = ["First", "Second"]
        .map(|name| wings_abi::RecordSchema {
            name: name.into(),
            fields: vec![("value".into(), ValueType::Int)],
        })
        .into();
    data.manifest.state = ValueType::RecordRef(0);
    data.initializer = named_initializer;
    data.manifest.validate().unwrap();
    for (mode, expected) in [
        (0, "invalid named record state"),
        (1, "division by zero"),
        (2, "invalid scalar state or reply"),
    ] {
        let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
        let committed = process.state;
        assert!(
            process
                .invoke("Bump", &[mode])
                .unwrap_err()
                .contains(expected)
        );
        assert_eq!(process.state, committed);
        let heap = process.heap.as_mut().unwrap();
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = ScalarContext {
            effect_call: None,
            cancellation: &flag,
            remaining_steps: 128,
            yield_request: &flag,
            slice_remaining: 128,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: Some(crate::aot_heap::heap_call),
            heap_data: std::ptr::from_mut(&mut **heap).cast(),
        };
        let arguments = [0u64, committed as u64, 0];
        let mut value = 0;
        assert_eq!(
            unsafe { crate::aot_heap::heap_call(&mut context, HEAP_COLLECT, 0, &mut value) },
            STATUS_OK
        );
        assert_eq!(process.heap_stats().unwrap().allocated, 1);
        assert_eq!(
            unsafe {
                crate::aot_heap::heap_call(
                    &mut context,
                    wings_abi::HEAP_RECORD_FIELD,
                    arguments.as_ptr() as usize as u64,
                    &mut value,
                )
            },
            STATUS_OK
        );
        assert_eq!(value, 7);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
}
