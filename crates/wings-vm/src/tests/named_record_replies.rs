use super::*;

fn schemas() -> Vec<wings_abi::RecordSchema> {
    vec![
        wings_abi::RecordSchema {
            name: "Empty".into(),
            fields: vec![],
        },
        wings_abi::RecordSchema {
            name: "Reply".into(),
            fields: vec![
                ("count".into(), ValueType::Int),
                ("accepted".into(), ValueType::Bool),
                ("unit".into(), ValueType::Unit),
                ("peer".into(), ValueType::Pid),
                ("values".into(), ValueType::ListRef),
                ("bytes".into(), ValueType::BytesRef),
                ("text".into(), ValueType::StringRef),
                ("empty".into(), ValueType::RecordRef(0)),
            ],
        },
    ]
}

fn values() -> [MessageValue<'static>; 7] {
    [
        MessageValue::Int(-7),
        MessageValue::Bool(true),
        MessageValue::Unit,
        MessageValue::Pid(42),
        MessageValue::IntList(&[1, 2, 3]),
        MessageValue::Bytes(&[255, 0, 17]),
        MessageValue::String("中😀\0"),
    ]
}

fn copied(account: &ResourceAccount) -> RecordReply {
    let records = schemas();
    let lease = RecordReply::reserve(account, 1, &records, None).unwrap();
    let expansion = wings_abi::expand_record_schema(&records, 1).unwrap();
    let payload = crate::host_values::HostMessage::copy_types(
        expansion.fields[..expansion.count].iter().copied(),
        &values(),
        account,
    )
    .unwrap();
    RecordReply::from_parts(lease, payload).unwrap()
}

#[test]
fn named_reply_owns_schema_contents_and_last_clone_charge() {
    let [vm, tenant, account] = accounts();
    let reply = copied(&account);
    assert_eq!(reply.schema().name, "Reply");
    assert_eq!(reply.values().collect::<Vec<_>>(), values());
    let before = account.used();
    let retained = reply.clone();
    assert_eq!(account.used(), before);
    assert!(std::ptr::eq(reply.schema(), retained.schema()));
    drop((reply, account, tenant));
    assert_eq!(retained.values().collect::<Vec<_>>(), values());
    assert!(vm.used().bytes > 0);
    drop(retained);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_reply_schema_storage_checks_each_account_before_allocation() {
    let [vm, tenant, account] = accounts();
    let records = schemas();
    let before = account.used().bytes;
    let lease = RecordReply::reserve(&account, 1, &records, None).unwrap();
    let required = account.used().bytes - before;
    drop(lease);
    for owner in [&account, &tenant, &vm] {
        for available in [0, required - 1, required] {
            let fill = owner
                .reserve(Resources {
                    bytes: owner.limit().bytes - owner.used().bytes - available,
                    ..Resources::default()
                })
                .unwrap();
            let baseline = [vm.used(), tenant.used(), account.used()];
            let lease = RecordReply::reserve(&account, 1, &records, None);
            assert_eq!(lease.is_ok(), available == required);
            drop(lease);
            assert_eq!([vm.used(), tenant.used(), account.used()], baseline);
            drop(fill);
        }
    }
    drop((account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_reply_schema_copy_obeys_runtime_budget_and_cancellation() {
    let [vm, tenant, account] = accounts();
    let records = schemas();
    for budget in 0..=10 {
        for cancelled in [false, true] {
            let flag = std::sync::atomic::AtomicU32::new(u32::from(cancelled));
            let mut context = ScalarContext {
                cancellation: &flag,
                remaining_steps: budget,
                yield_request: &flag,
                slice_remaining: u64::MAX,
                slow_path: None,
                data: std::ptr::null_mut(),
                stack_limit: 0,
                roots: std::ptr::null(),
                heap_call: None,
                heap_data: std::ptr::null_mut(),
                effect_call: None,
            };
            let before = account.used();
            let copied = RecordReply::reserve(&account, 1, &records, Some(&mut context));
            if cancelled {
                assert_eq!(copied.err(), Some(STATUS_CANCELLED));
            } else if budget < 10 {
                assert_eq!(copied.err(), Some(STATUS_BUDGET_EXHAUSTED));
            } else {
                assert!(copied.is_ok());
                drop(copied);
            }
            assert_eq!(account.used(), before);
        }
    }
    drop((account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_reply_completion_matches_names_and_rebuilds_all_failed_leaf_defaults() {
    let [vm, tenant, account] = accounts();
    let reply = copied(&account);
    let mut records = schemas();
    let success = RecordReply::completion(Some(&reply), 1, &records, 17, 0).unwrap();
    assert_eq!(&success.arguments[..2], &[17, 0]);
    assert_eq!(success.count, 9);
    assert_eq!(success.arguments[5], 42);
    assert_eq!(success.lists.mask, 1 << 6);
    assert!(
        success
            .bytes
            .valid(&success.arguments[..success.count], 3 << 7)
    );
    records[1].name = "Other".into();
    assert!(RecordReply::completion(Some(&reply), 1, &records, 17, 0).is_err());
    for status in 1..=3 {
        let failure = RecordReply::completion(None, 1, &records, 17, status).unwrap();
        assert_eq!(&failure.arguments[..2], &[17, status]);
        assert!(failure.arguments[2..].iter().all(|value| *value == 0));
        assert_eq!(failure.lists.mask, 1 << 6);
        assert_eq!(failure.lists.len(), 0);
        assert!(
            failure
                .bytes
                .valid(&failure.arguments[..failure.count], 3 << 7)
        );
    }
    drop((success, reply, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_reply_rejects_mismatched_scalar_and_buffer_leaves_and_releases_leases() {
    let [vm, tenant, account] = accounts();
    let records = schemas();
    for invalid in [ValueType::Int, ValueType::BytesRef, ValueType::Bool] {
        let before = account.used();
        let lease = RecordReply::reserve(&account, 1, &records, None).unwrap();
        let types = [invalid];
        let value = match invalid {
            ValueType::Int => MessageValue::Int(5),
            ValueType::BytesRef => MessageValue::Bytes(&[1, 2, 3]),
            _ => MessageValue::Bool(true),
        };
        let payload =
            crate::host_values::HostMessage::copy_types(types.into_iter(), &[value], &account)
                .unwrap();
        assert!(RecordReply::from_parts(lease, payload).is_err());
        assert_eq!(account.used(), before);
    }
    drop((account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_reply_rejects_invalid_handles_and_other_layouts_before_state_commit() {
    for entry in [invalid_output as ScalarEntry, list_reply] {
        for heap_enabled in [false, true] {
            let [vm, tenant, account] = accounts();
            let pool = StackPool::new_accounted(&tenant, 1, 0)
                .unwrap()
                .for_process(&account)
                .unwrap();
            let mut module = module();
            let data = Arc::get_mut(&mut module.inner).unwrap();
            data.manifest.records = vec![wings_abi::RecordSchema {
                name: "Empty".into(),
                fields: vec![],
            }];
            data.manifest.handlers = vec![Handler {
                name: "Get".into(),
                parameters: vec![],
                reply: Some(ValueType::RecordRef(0)),
                completion: None,
            }];
            data.entries = vec![entry].into_boxed_slice();
            data.manifest.validate().unwrap();
            let mut process = if heap_enabled {
                module.instantiate_with_heap(&pool, &account, 4).unwrap()
            } else {
                module.instantiate_in_pool(&pool).unwrap()
            };
            assert!(
                process
                    .invoke_record("Get", &[], 8192, &CancellationToken::default())
                    .is_err()
            );
            assert_eq!(process.state, 0);
            assert!(process.failed);
            drop((process, pool, module, account, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}
