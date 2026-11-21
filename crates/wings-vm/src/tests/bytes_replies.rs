use super::*;

unsafe extern "C" fn return_bytes(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let scratch = [unsafe { *arguments }, 255, 0];
    let mut reference = 0;
    let Some(call) = (unsafe { (*context).heap_call }) else {
        return STATUS_INVALID_CONTROL;
    };
    let status = unsafe {
        call(
            context,
            wings_abi::HEAP_FILL_BYTES,
            scratch.as_ptr() as usize as u64,
            &mut reference,
        )
    };
    if status == STATUS_OK {
        unsafe {
            *output = ScalarOutput {
                state: 77,
                reply: reference as i64,
            }
        };
    }
    status
}

fn fixture() -> (
    ResourceAccount,
    ResourceAccount,
    ResourceAccount,
    StackPool,
    ScalarProcess,
) {
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(24 << 20)).unwrap();
    let account = tenant.process(limits(16 << 20)).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers = vec![Handler {
        name: "Get".into(),
        parameters: vec![("length".into(), ValueType::Int)],
        reply: Some(ValueType::BytesRef),
        completion: None,
    }];
    data.entries = vec![return_bytes as ScalarEntry].into_boxed_slice();
    data.manifest.validate().unwrap();
    let process = module.instantiate_with_heap(&pool, &account, 4).unwrap();
    (vm, tenant, account, pool, process)
}

#[test]
fn byte_replies_copy_exact_content_and_outlive_process_without_releasing_storage() {
    for length in [0, 1, 255, 256, 257, 65536] {
        let (vm, tenant, account, pool, mut process) = fixture();
        assert!(
            process
                .invoke("Get", &[length])
                .unwrap_err()
                .contains("invoke_bytes")
        );
        assert!(!process.failed);
        let reply = process
            .invoke_bytes("Get", &[length], 4096, &CancellationToken::default())
            .unwrap();
        assert_eq!(reply.as_slice(), vec![255; length as usize]);
        assert_eq!(process.state, 77);
        let retained = reply.clone();
        assert_eq!(retained.as_slice().as_ptr(), reply.as_slice().as_ptr());
        drop((reply, process, pool, account, tenant));
        assert!(vm.used().bytes >= length as u64);
        assert_eq!(retained.as_slice(), vec![255; length as usize]);
        drop(retained);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn byte_export_checks_total_budget_cancel_and_drop_without_committing_candidates() {
    for boundary in [0, 1, 16, 255, 256, 257, 300, 511] {
        let (vm, tenant, account, pool, mut process) = fixture();
        let cancellation = CancellationToken::default();
        let mut execution = process
            .start_controlled(
                "Get",
                &[65536],
                4096,
                &cancellation,
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..boundary {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        cancellation.cancel();
        assert!(execution.resume(1).unwrap_err().contains("cancelled"));
        assert_eq!(execution.snapshot().fault, Some(ExecutionFault::Cancelled));
        drop(execution);
        assert_eq!(process.state, 0);
        drop((process, pool, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
    for budget in [255, 256, 257, 512, 513, 514] {
        let (vm, tenant, account, pool, mut process) = fixture();
        let result = process.invoke_bytes("Get", &[65536], budget, &CancellationToken::default());
        if budget == 514 {
            assert_eq!(result.unwrap().as_slice(), vec![255; 65536]);
            assert_eq!(process.state, 77);
        } else {
            assert!(result.unwrap_err().contains("budget exhausted"));
            assert_eq!(process.state, 0);
        }
        drop((process, pool, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
    let (vm, tenant, account, pool, mut process) = fixture();
    {
        let mut execution = process
            .start_controlled(
                "Get",
                &[65536],
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..300 {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
    }
    assert!(process.failed);
    assert_eq!(process.state, 0);
    drop((process, pool, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn byte_reply_control_rejection_after_copy_rolls_back_and_preserves_old_state() {
    for level in 0..3 {
        let (vm, tenant, account, pool, mut process) = fixture();
        let mut execution = process
            .start_controlled(
                "Get",
                &[65536],
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..300 {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        let denied = [&vm, &tenant, &account][level];
        let blocker = denied
            .reserve(Resources {
                bytes: denied.limit().bytes - denied.used().bytes,
                ..Resources::default()
            })
            .unwrap();
        assert!(
            execution
                .resume(u64::MAX)
                .unwrap_err()
                .contains("heap exhausted")
        );
        drop((execution, blocker));
        assert_eq!(process.state, 0);
        drop((process, pool, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}
