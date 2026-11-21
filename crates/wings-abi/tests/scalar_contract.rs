use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::AtomicU32;

use wings_abi::{
    DEFAULT_MAX_STEPS, HeapCall, SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED,
    STATUS_DIVISION_BY_ZERO, STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT,
    STATUS_OK, STATUS_OVERFLOW, STATUS_STACK_EXHAUSTED, ScalarContext, ScalarEntry, ScalarOutput,
    ScalarSlowPath, ShadowFrame, ValueType, host_target,
};

#[test]
fn named_record_schema_limits_and_nominal_indices_are_explicit() {
    use wings_abi::{RecordSchema, validate_records};
    let mut schemas = vec![
        RecordSchema {
            name: "A".into(),
            fields: vec![("value".into(), ValueType::Int)],
        },
        RecordSchema {
            name: "B".into(),
            fields: vec![
                ("nested".into(), ValueType::RecordRef(0)),
                ("peer".into(), ValueType::Pid),
            ],
        },
    ];
    validate_records(&schemas).unwrap();
    assert!(ValueType::RecordRef(0).is_reference());
    assert!(!ValueType::RecordRef(0).accepts(1));
    for invalid in [
        ValueType::RecordRef(1),
        ValueType::RecordRef(u16::MAX),
        ValueType::RequestId,
    ] {
        schemas[1].fields[0].1 = invalid;
        assert!(validate_records(&schemas).is_err());
    }
    let schemas: Vec<_> = (0..32)
        .map(|index| RecordSchema {
            name: format!("Record{index}"),
            fields: (0..8)
                .map(|field| (format!("field{field}"), ValueType::Int))
                .collect(),
        })
        .collect();
    validate_records(&schemas).unwrap();
    let mut excess = schemas.clone();
    excess.push(schemas[0].clone());
    assert!(validate_records(&excess).is_err());
    let mut excess = schemas;
    excess[0].fields.push(("extra".into(), ValueType::Int));
    assert!(validate_records(&excess).is_err());
}

#[test]
fn named_record_reply_expansion_is_bounded_and_preserves_flat_leaf_types() {
    use wings_abi::{RecordSchema, expand_record_schema};
    let records = vec![
        RecordSchema {
            name: "Leaf".into(),
            fields: vec![
                ("value".into(), ValueType::Int),
                ("label".into(), ValueType::StringRef),
            ],
        },
        RecordSchema {
            name: "Reply".into(),
            fields: vec![
                ("head".into(), ValueType::Bool),
                ("nested".into(), ValueType::RecordRef(0)),
                ("tail".into(), ValueType::ListRef),
            ],
        },
    ];
    let expanded = expand_record_schema(&records, 1).unwrap();
    assert_eq!(expanded.nodes, 6);
    assert_eq!(expanded.count, 4);
    assert_eq!(
        &expanded.fields[..expanded.count],
        &[
            ValueType::Bool,
            ValueType::Int,
            ValueType::StringRef,
            ValueType::ListRef,
        ]
    );

    let mut bounded = Vec::new();
    for index in 0..32 {
        bounded.push(RecordSchema {
            name: format!("R{index}"),
            fields: (0..8)
                .map(|field| (format!("f{field}"), ValueType::Int))
                .collect(),
        });
    }
    assert_eq!(expand_record_schema(&bounded, 0).unwrap().nodes, 9);
    bounded.push(RecordSchema {
        name: "Wide".into(),
        fields: (0..8)
            .map(|field| (format!("r{field}"), ValueType::RecordRef(0)))
            .collect(),
    });
    assert!(
        expand_record_schema(&bounded, 32)
            .unwrap_err()
            .contains("64 total nodes")
    );
}

#[test]
fn named_reply_type_compatibility_is_nominal_and_bounds_repeated_subtrees() {
    use wings_abi::{RecordSchema, equivalent_types};
    let mut records = vec![RecordSchema {
        name: "Leaf".into(),
        fields: vec![("value".into(), ValueType::Int)],
    }];
    assert!(equivalent_types(
        &records,
        ValueType::RecordRef(0),
        &records,
        ValueType::RecordRef(0)
    ));
    let mut foreign = records.clone();
    foreign[0].name = "Other".into();
    assert!(!equivalent_types(
        &records,
        ValueType::RecordRef(0),
        &foreign,
        ValueType::RecordRef(0)
    ));
    for index in 1..32 {
        records.push(RecordSchema {
            name: format!("R{index}"),
            fields: vec![
                ("left".into(), ValueType::RecordRef(index - 1)),
                ("right".into(), ValueType::RecordRef(index - 1)),
            ],
        });
    }
    assert!(!equivalent_types(
        &records,
        ValueType::RecordRef(31),
        &records,
        ValueType::RecordRef(31)
    ));
}

#[test]
fn integers_accept_the_entire_signed_scalar_range() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert!(ValueType::Int.accepts(value));
    }
}

#[test]
fn string_references_are_gc_roots_but_never_raw_scalar_values() {
    assert!(ValueType::StringRef.is_reference());
    assert!(!ValueType::StringRef.is_capability());
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert!(!ValueType::StringRef.accepts(value));
    }
}

#[test]
fn booleans_accept_only_zero_and_one() {
    for value in [i64::MIN, -1, 0, 1, 2, i64::MAX] {
        assert_eq!(ValueType::Bool.accepts(value), matches!(value, 0 | 1));
    }
}

#[test]
fn unit_accepts_only_zero() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(ValueType::Unit.accepts(value), value == 0);
    }
}

#[test]
fn scalar_output_is_two_contiguous_c_integers() {
    assert_eq!(offset_of!(ScalarOutput, state), 0);
    assert_eq!(offset_of!(ScalarOutput, reply), 8);
    assert_eq!(size_of::<ScalarOutput>(), 16);
    assert_eq!(align_of::<ScalarOutput>(), align_of::<i64>());
    let output = ScalarOutput::default();
    assert_eq!((output.state, output.reply), (0, 0));
}

#[test]
fn control_context_layout_matches_the_native_codegen_contract() {
    // The scalar backend is restricted to these 64-bit hosts; offsets are used
    // directly by generated code, so field reordering must change this test.
    if host_target().is_err() {
        return;
    }
    assert_eq!(offset_of!(ScalarContext, cancellation), 0);
    assert_eq!(offset_of!(ScalarContext, remaining_steps), 8);
    assert_eq!(offset_of!(ScalarContext, yield_request), 16);
    assert_eq!(offset_of!(ScalarContext, slice_remaining), 24);
    assert_eq!(offset_of!(ScalarContext, slow_path), 32);
    assert_eq!(offset_of!(ScalarContext, data), 40);
    assert_eq!(offset_of!(ScalarContext, stack_limit), 48);
    assert_eq!(offset_of!(ScalarContext, roots), 56);
    assert_eq!(offset_of!(ScalarContext, heap_call), 64);
    assert_eq!(offset_of!(ScalarContext, heap_data), 72);
    assert_eq!(offset_of!(ScalarContext, effect_call), 80);
    assert_eq!(size_of::<ScalarContext>(), 88);
    assert_eq!(align_of::<ScalarContext>(), 8);
    assert_eq!(size_of::<AtomicU32>(), 4);
}

#[test]
fn optional_slow_path_has_the_nullable_function_pointer_layout() {
    assert_eq!(size_of::<Option<ScalarSlowPath>>(), size_of::<usize>());
    assert_eq!(size_of::<ScalarEntry>(), size_of::<usize>());
    assert_eq!(size_of::<Option<HeapCall>>(), size_of::<usize>());
    assert_eq!(
        size_of::<Option<wings_abi::EffectCall>>(),
        size_of::<usize>()
    );
}

#[test]
fn process_effect_request_is_a_fixed_native_c_layout() {
    use wings_abi::EffectRequest;
    assert_eq!(offset_of!(EffectRequest, operation), 0);
    assert_eq!(offset_of!(EffectRequest, handler), 4);
    assert_eq!(offset_of!(EffectRequest, target), 8);
    assert_eq!(offset_of!(EffectRequest, arguments), 16);
    assert_eq!(offset_of!(EffectRequest, count), 24);
    assert_eq!(offset_of!(EffectRequest, callback), 32);
    assert_eq!(offset_of!(EffectRequest, timeout_ms), 40);
    assert_eq!(offset_of!(EffectRequest, list_mask), 48);
    assert_eq!(offset_of!(EffectRequest, bytes_mask), 56);
    assert_eq!(offset_of!(EffectRequest, string_mask), 64);
    assert_eq!(size_of::<EffectRequest>(), 72);
}

#[test]
fn scalar_status_codes_and_version_remain_wire_compatible() {
    assert_eq!(SCALAR_ABI_VERSION, 23);
    assert_eq!(wings_abi::MAX_RECORD_REPLY_NODES, 64);
    assert_eq!(wings_abi::HEAP_STRING_LITERAL, 18);
    assert_eq!(wings_abi::HEAP_EQUAL_STRINGS, 21);
    assert_eq!(wings_abi::HEAP_STRING_AT, 22);
    assert_eq!(wings_abi::HEAP_STRING_SLICE, 23);
    assert_eq!(wings_abi::HEAP_INT_TO_STRING, 24);
    assert_eq!(wings_abi::HEAP_BOOL_TO_STRING, 25);
    assert_eq!(wings_abi::HEAP_BYTES_LITERAL, 26);
    assert_eq!(STATUS_OK, 0);
    assert_eq!(STATUS_OVERFLOW, 1);
    assert_eq!(STATUS_DIVISION_BY_ZERO, 2);
    assert_eq!(STATUS_CANCELLED, 3);
    assert_eq!(STATUS_BUDGET_EXHAUSTED, 4);
    assert_eq!(STATUS_INVALID_CONTROL, 5);
    assert_eq!(STATUS_STACK_EXHAUSTED, 6);
    assert_eq!(STATUS_HEAP_EXHAUSTED, 7);
    assert_eq!(STATUS_INVALID_ROOT, 8);
    assert_eq!(wings_abi::STATUS_EMPTY_LIST, 9);
    assert_eq!(wings_abi::STATUS_SEND_REJECTED, 10);
    assert_eq!(wings_abi::STATUS_SPAWN_REJECTED, 11);
    assert_eq!(wings_abi::STATUS_REQUEST_REJECTED, 12);
    assert_eq!(wings_abi::STATUS_INVALID_BYTES, 13);
    assert_eq!(wings_abi::STATUS_REPLY_REJECTED, 14);
    assert_eq!(wings_abi::STATUS_INVALID_STRING, 15);
    assert_eq!(wings_abi::STATUS_INVALID_STRING_INDEX, 16);
    assert_eq!(
        [
            wings_abi::HEAP_STRING_FROM_BYTES,
            wings_abi::HEAP_STRING_BYTES,
            wings_abi::HEAP_STRING_LEN,
            wings_abi::HEAP_CONCAT_STRINGS,
        ],
        [14, 15, 16, 17]
    );
    assert_eq!(DEFAULT_MAX_STEPS, 1_000_000);
}

#[test]
fn bytes_have_no_scalar_encoding_and_operations_extend_without_layout_changes() {
    assert!(ValueType::BytesRef.is_reference());
    assert!(!ValueType::BytesRef.is_capability());
    for value in [0, 1, -1, i64::MIN, i64::MAX] {
        assert!(!ValueType::BytesRef.accepts(value));
    }
    assert_eq!(
        [
            wings_abi::HEAP_FILL_BYTES,
            wings_abi::HEAP_BYTES_LEN,
            wings_abi::HEAP_BYTE_AT,
            wings_abi::HEAP_REPLACE_BYTE,
            wings_abi::HEAP_READ_I64_BE,
            wings_abi::HEAP_REPLACE_I64_BE
        ],
        [8, 9, 10, 11, 12, 13]
    );
}

#[test]
fn references_have_no_scalar_encoding_and_shadow_frames_are_native_c_layout() {
    for value in [0, 1, -1, i64::MIN, i64::MAX] {
        assert!(!ValueType::ListRef.accepts(value));
        assert!(!ValueType::Pid.accepts(value));
        assert!(!ValueType::RequestId.accepts(value));
    }
    assert_eq!(offset_of!(ShadowFrame, previous), 0);
    assert_eq!(offset_of!(ShadowFrame, slots), size_of::<usize>());
    assert_eq!(offset_of!(ShadowFrame, count), 2 * size_of::<usize>());
    assert_eq!(size_of::<ShadowFrame>(), 3 * size_of::<usize>());
}

#[test]
fn host_target_only_accepts_the_two_supported_platforms() {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => assert_eq!(host_target().unwrap(), "aarch64-apple-darwin"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => {
            assert_eq!(host_target().unwrap(), "x86_64-unknown-linux-gnu");
        }
        _ => assert!(host_target().unwrap_err().contains("requires")),
    }
}
