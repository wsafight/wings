use super::*;

unsafe extern "C" fn string_candidate(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let Some(call) = (unsafe { (*context).heap_call }) else {
        return STATUS_INVALID_CONTROL;
    };
    let value = unsafe { *arguments };
    let mut scratch = [8, 97, 0];
    let mut bytes = 0;
    let mut status = unsafe {
        call(
            context,
            wings_abi::HEAP_FILL_BYTES,
            scratch.as_ptr() as usize as u64,
            &mut bytes,
        )
    };
    if status != STATUS_OK {
        return status;
    }
    let roots = [bytes];
    let frame = wings_abi::ShadowFrame {
        previous: unsafe { (*context).roots },
        slots: roots.as_ptr(),
        count: 1,
    };
    unsafe { (*context).roots = &frame };
    scratch[0] = bytes as i64;
    let mut string = 0;
    status = unsafe {
        call(
            context,
            wings_abi::HEAP_STRING_FROM_BYTES,
            scratch.as_ptr() as usize as u64,
            &mut string,
        )
    };
    unsafe { (*context).roots = frame.previous };
    if status != STATUS_OK {
        return status;
    }
    unsafe {
        *output = ScalarOutput {
            state: string as i64,
            reply: value,
        }
    };
    if value == 99 {
        wings_abi::STATUS_INVALID_STRING
    } else {
        STATUS_OK
    }
}

unsafe extern "C" fn string_initialize(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    unsafe { string_candidate(state, [0].as_ptr(), output, context) }
}

#[test]
fn string_state_failure_keeps_old_commit_and_final_owners_release_all_charges() {
    let [vm, tenant, account] = accounts();
    let baseline = vm.used();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::StringRef;
    data.initializer = string_initialize;
    data.entries[1] = string_candidate;
    let mut process = module.instantiate_with_heap(&pool, &account, 8).unwrap();
    assert_eq!(process.invoke("Bump", &[7]).unwrap(), Some(7));
    let committed = process.state;
    assert!(
        process
            .invoke("Bump", &[99])
            .unwrap_err()
            .contains("invalid UTF-8 string")
    );
    assert_eq!(process.state, committed);
    assert!(process.failed);
    process
        .heap
        .as_mut()
        .unwrap()
        .commit_typed(ValueType::StringRef, committed)
        .unwrap();
    assert!(process.invoke("Bump", &[0]).is_err());
    drop((process, module, pool));
    assert_eq!(vm.used(), baseline);
}
