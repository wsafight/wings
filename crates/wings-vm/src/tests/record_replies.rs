use super::*;

fn record_limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 4,
        requests: 4,
    }
}

unsafe extern "C" fn raw_record(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    unsafe {
        let Some(heap_call) = (*context).heap_call else {
            return STATUS_INVALID_CONTROL;
        };
        let mut encoded = 0;
        let operation = if *arguments.add(1) == 0 {
            wings_abi::HEAP_RECORD_INT
        } else {
            wings_abi::HEAP_EMPTY_INTS
        };
        let status = heap_call(context, operation, *arguments as u64, &mut encoded);
        if status != STATUS_OK {
            return status;
        }
        *output = ScalarOutput {
            state,
            reply: encoded as i64,
        };
    }
    STATUS_OK
}

unsafe extern "C" fn take_record(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe {
        *output = ScalarOutput {
            state: state + 77,
            reply: *arguments,
        };
    }
    STATUS_OK
}

unsafe extern "C" fn collect_records(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    unsafe {
        let Some(heap_call) = (*context).heap_call else {
            return STATUS_INVALID_CONTROL;
        };
        let mut ignored = 0;
        let status = heap_call(context, wings_abi::HEAP_COLLECT, 0, &mut ignored);
        if status != STATUS_OK {
            return status;
        }
        *output = ScalarOutput { state, reply: 0 };
    }
    STATUS_OK
}

fn record_module() -> ScalarModule {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers = vec![
        Handler {
            name: "Raw".into(),
            parameters: vec![
                ("value".into(), ValueType::Int),
                ("list".into(), ValueType::Bool),
            ],
            reply: Some(ValueType::Int),
            completion: None,
        },
        Handler {
            name: "Take".into(),
            parameters: vec![("value".into(), ValueType::Int)],
            reply: Some(ValueType::Ref),
            completion: None,
        },
        Handler {
            name: "Collect".into(),
            parameters: vec![],
            reply: None,
            completion: None,
        },
    ];
    data.entries = vec![raw_record as ScalarEntry, take_record, collect_records].into_boxed_slice();
    data.manifest.validate().unwrap();
    module
}

fn record_instance(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    heap: bool,
) -> (ResourceAccount, StackPool, ScalarProcess) {
    let account = tenant.process_accounted(record_limits()).unwrap();
    let shared = StackPool::new_accounted(tenant, 1, 0).unwrap();
    let pool = shared.for_process(&account).unwrap();
    let process = if heap {
        module.instantiate_with_heap(&pool, &account, 4).unwrap()
    } else {
        module.instantiate_in_pool(&pool).unwrap()
    };
    (account, pool, process)
}

#[test]
fn record_reply_copies_local_contents_and_rejects_stale_results_before_commit() {
    let vm = ResourceAccount::vm(record_limits());
    let tenant = vm.tenant_accounted(record_limits()).unwrap();
    let module = record_module();
    let (account, pool, mut process) = record_instance(&module, &tenant, true);
    let encoded = process.invoke("Raw", &[37, 0]).unwrap().unwrap();
    assert_ne!(encoded, 37);
    let copied = process.invoke("Take", &[encoded]).unwrap();
    assert_eq!(copied, Some(37));
    assert_eq!(process.state, 77);
    process.invoke("Collect", &[]).unwrap();
    assert!(
        process
            .invoke("Take", &[encoded])
            .unwrap_err()
            .contains("invalid record reply")
    );
    assert_eq!(process.state, 77);
    assert_eq!(copied, Some(37));
    drop((process, pool, account, module, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn record_reply_rejects_missing_heap_and_invalid_handles_without_committing() {
    for heap in [false, true] {
        for encoded in [0, 1, i64::MIN, i64::MAX] {
            let vm = ResourceAccount::vm(record_limits());
            let tenant = vm.tenant_accounted(record_limits()).unwrap();
            let module = record_module();
            let (account, pool, mut process) = record_instance(&module, &tenant, heap);
            assert!(
                process
                    .invoke("Take", &[encoded])
                    .unwrap_err()
                    .contains("invalid record reply")
            );
            assert_eq!(process.state, 0);
            assert!(process.failed);
            drop((process, pool, account, module, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

#[test]
fn record_reply_cannot_read_foreign_content_when_current_heap_has_no_matching_object() {
    let vm = ResourceAccount::vm(record_limits());
    let tenant = vm.tenant_accounted(record_limits()).unwrap();
    let module = record_module();
    let (account, pool, mut process) = record_instance(&module, &tenant, true);
    let (foreign_account, foreign_pool, mut foreign) = record_instance(&module, &tenant, true);
    let encoded = foreign.invoke("Raw", &[37, 0]).unwrap().unwrap();
    assert!(
        process
            .invoke("Take", &[encoded])
            .unwrap_err()
            .contains("invalid record reply")
    );
    assert_eq!(process.state, 0);
    assert_eq!(foreign.state, 0);
    assert_eq!(foreign.invoke("Take", &[encoded]).unwrap(), Some(37));
    drop((
        process,
        pool,
        account,
        foreign,
        foreign_pool,
        foreign_account,
        module,
        tenant,
    ));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn record_reply_rejects_other_layout_without_committing_candidate() {
    let vm = ResourceAccount::vm(record_limits());
    let tenant = vm.tenant_accounted(record_limits()).unwrap();
    let module = record_module();
    let (account, pool, mut process) = record_instance(&module, &tenant, true);
    let encoded = process.invoke("Raw", &[37, 1]).unwrap().unwrap();
    assert!(
        process
            .invoke("Take", &[encoded])
            .unwrap_err()
            .contains("invalid record reply")
    );
    assert_eq!(process.state, 0);
    drop((process, pool, account, module, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn record_reply_resolves_matching_native_bits_in_current_heap_only() {
    let vm = ResourceAccount::vm(record_limits());
    let tenant = vm.tenant_accounted(record_limits()).unwrap();
    let module = record_module();
    let (account, pool, mut process) = record_instance(&module, &tenant, true);
    let (foreign_account, foreign_pool, mut foreign) = record_instance(&module, &tenant, true);
    let local = process.invoke("Raw", &[91, 0]).unwrap().unwrap();
    let encoded = foreign.invoke("Raw", &[37, 0]).unwrap().unwrap();
    assert_eq!(local, encoded);
    assert_eq!(process.invoke("Take", &[encoded]).unwrap(), Some(91));
    assert_eq!(foreign.state, 0);
    assert_eq!(foreign.invoke("Take", &[encoded]).unwrap(), Some(37));
    drop((
        process,
        pool,
        account,
        foreign,
        foreign_pool,
        foreign_account,
        module,
        tenant,
    ));
    assert_eq!(vm.used(), Resources::default());
}
