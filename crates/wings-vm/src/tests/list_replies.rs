use super::*;

unsafe extern "C" fn return_list(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    unsafe {
        let Some(heap_call) = (*context).heap_call else {
            return STATUS_INVALID_CONTROL;
        };
        let count = *arguments;
        let mut encoded = 0;
        let status = heap_call(context, wings_abi::HEAP_EMPTY_INTS, 0, &mut encoded);
        if status != STATUS_OK {
            return status;
        }
        for value in 0..count {
            let frame = wings_abi::ShadowFrame {
                previous: (*context).roots,
                slots: &encoded,
                count: 1,
            };
            (*context).roots = &frame;
            let status = heap_call(
                context,
                wings_abi::HEAP_PREPEND_INT,
                value as u64,
                &mut encoded,
            );
            (*context).roots = frame.previous;
            if status != STATUS_OK {
                return status;
            }
        }
        *output = ScalarOutput {
            state: 77,
            reply: encoded as i64,
        };
    }
    STATUS_OK
}

fn fixture(
    count: usize,
) -> (
    ResourceAccount,
    ResourceAccount,
    ResourceAccount,
    StackPool,
    ScalarProcess,
) {
    let limits = limits(32 << 20);
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers = vec![Handler {
        name: "Get".into(),
        parameters: vec![("count".into(), ValueType::Int)],
        reply: Some(ValueType::ListRef),
    }];
    data.entries = vec![return_list as ScalarEntry].into_boxed_slice();
    data.manifest.validate().unwrap();
    let process = module
        .instantiate_with_heap(&pool, &account, count + 2)
        .unwrap();
    (vm, tenant, account, pool, process)
}

#[test]
fn list_reply_limits_invalid_scalar_call_and_export_failures_do_not_commit_candidates() {
    for count in [0, 1, 64, 65] {
        let (vm, tenant, account, pool, mut process) = fixture(count);
        assert!(
            process
                .invoke("Get", &[count as i64])
                .unwrap_err()
                .contains("invoke_list")
        );
        assert!(!process.failed);
        assert_eq!(process.state, 0);
        let result =
            process.invoke_list("Get", &[count as i64], 1024, &CancellationToken::default());
        if count <= 64 {
            let list = result.unwrap();
            assert_eq!(list.as_slice(), (0..count as i64).rev().collect::<Vec<_>>());
            assert_eq!(process.state, 77);
            let retained = list.clone();
            drop((list, process, pool, account, tenant));
            assert!(vm.used().bytes > 0);
            assert_eq!(retained.as_slice().len(), count);
            drop(retained);
        } else {
            assert!(result.unwrap_err().contains("exceeds 64"));
            assert_eq!(process.state, 0);
            assert!(process.failed);
            drop((process, pool, account, tenant));
        }
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn list_reply_export_remains_on_live_stack_and_cancellation_keeps_old_state() {
    for cancel_at in [0, 1, 16, 64] {
        let (vm, tenant, account, pool, mut process) = fixture(64);
        let cancellation = CancellationToken::default();
        let mut execution = process
            .start_controlled(
                "Get",
                &[64],
                1024,
                &cancellation,
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..cancel_at {
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
}

#[test]
fn list_reply_total_budget_includes_every_exported_node_and_empty_tail() {
    for budget in [63, 64, 65] {
        let (vm, tenant, account, pool, mut process) = fixture(64);
        let result = process.invoke_list("Get", &[64], budget, &CancellationToken::default());
        if budget == 65 {
            assert_eq!(result.unwrap().as_slice().len(), 64);
            assert_eq!(process.state, 77);
        } else {
            assert!(result.unwrap_err().contains("budget exhausted"));
            assert_eq!(process.state, 0);
        }
        drop((process, pool, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn dropping_suspended_list_export_normally_cancels_and_releases_roots_and_result_storage() {
    let (vm, tenant, account, pool, mut process) = fixture(64);
    let baseline = vm.used();
    {
        let mut execution = process
            .start_controlled(
                "Get",
                &[64],
                1024,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    }
    assert!(process.failed);
    assert_eq!(process.state, 0);
    assert!(process.heap.is_none());
    assert!(vm.used().bytes < baseline.bytes);
    drop((process, pool, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn list_result_storage_rejection_after_export_preserves_old_candidate_and_returns_its_reservations()
{
    let (vm, tenant, account, pool, mut process) = fixture(64);
    let mut execution = process
        .start_controlled(
            "Get",
            &[64],
            1024,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let available = [
        account.limit().bytes - account.used().bytes,
        tenant.limit().bytes - tenant.used().bytes,
        vm.limit().bytes - vm.used().bytes,
    ]
    .into_iter()
    .min()
    .unwrap();
    let blocker = account
        .reserve(Resources {
            bytes: available,
            ..Resources::default()
        })
        .unwrap();
    assert!(
        execution
            .resume(u64::MAX)
            .unwrap_err()
            .contains("heap exhausted")
    );
    assert_eq!(
        execution.snapshot().fault,
        Some(ExecutionFault::HeapExhausted)
    );
    drop((execution, blocker));
    assert_eq!(process.state, 0);
    assert!(process.failed);
    drop((process, pool, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}
