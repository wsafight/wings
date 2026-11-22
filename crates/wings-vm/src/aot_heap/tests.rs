use super::*;
use crate::Resources;

fn named_fixture(records: &[wings_abi::RecordSchema]) -> (ResourceAccount, Box<AotHeap>) {
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let heap = AotHeap::new_with_records(&account, 16, records).unwrap();
    (vm, heap)
}

#[test]
fn named_records_reject_same_shape_wrong_tags_and_stale_references() {
    use wings_abi::{RecordSchema, ValueType};
    let records = [
        RecordSchema {
            name: "First".into(),
            fields: vec![("value".into(), ValueType::Int)],
        },
        RecordSchema {
            name: "Second".into(),
            fields: vec![("value".into(), ValueType::Int)],
        },
        RecordSchema {
            name: "Holder".into(),
            fields: vec![("value".into(), ValueType::RecordRef(0))],
        },
        RecordSchema {
            name: "Flag".into(),
            fields: vec![("value".into(), ValueType::Bool)],
        },
    ];
    let (vm, mut heap) = named_fixture(&records);
    let first = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(7)])
        .unwrap();
    let second = heap
        .heap
        .allocate(heap.named[1].layout, &[RecordValue::Int(8)])
        .unwrap();
    let scalar = heap
        .heap
        .allocate(heap.empty_list, &[RecordValue::Unit])
        .unwrap();
    heap.commit_typed(ValueType::RecordRef(0), first.encoded() as i64)
        .unwrap();
    for ty in [
        ValueType::RecordRef(1),
        ValueType::ListRef,
        ValueType::StringRef,
        ValueType::RecordRef(32),
    ] {
        assert!(heap.commit_typed(ty, first.encoded() as i64).is_err());
        assert_eq!(heap.committed, Some(first));
    }
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    for arguments in [
        [2, second.encoded() as i64, 0],
        [2, scalar.encoded() as i64, 0],
        [3, 2, 0],
        [32, 0, 0],
    ] {
        let before = heap.stats();
        assert_eq!(
            unsafe { bytes(&mut context, wings_abi::HEAP_RECORD_NEW, arguments) },
            (STATUS_INVALID_ROOT, 79)
        );
        assert_eq!(heap.stats(), before);
    }
    for arguments in [
        [1, first.encoded() as i64, 0],
        [0, second.encoded() as i64, 0],
        [0, first.encoded() as i64, 1],
    ] {
        assert_eq!(
            unsafe { bytes(&mut context, wings_abi::HEAP_RECORD_FIELD, arguments) },
            (STATUS_INVALID_ROOT, 79)
        );
    }
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert!(
        heap.typed_reference(ValueType::RecordRef(1), second.encoded() as i64)
            .is_err()
    );
    assert_eq!(heap.committed, Some(first));
    let (status, holder) = unsafe {
        bytes(
            &mut context,
            wings_abi::HEAP_RECORD_NEW,
            [2, first.encoded() as i64, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    heap.commit_typed(ValueType::RecordRef(2), holder as i64)
        .unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.heap.get(first, 0).unwrap(), RecordValue::Int(7));
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn named_layout_capacity_and_failed_initialization_preserve_accounting() {
    use wings_abi::{RecordSchema, ValueType};
    let records: Vec<_> = (0..32)
        .map(|index| RecordSchema {
            name: format!("Record{index}"),
            fields: (0..8)
                .map(|field| (format!("field{field}"), ValueType::Int))
                .collect(),
        })
        .collect();
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let baseline = vm.used();
    let heap = AotHeap::new_with_records(&account, 1, &records).unwrap();
    let charge = account.used().bytes;
    assert_eq!(heap.named.len(), 32);
    assert_ne!(heap.named[0].layout, heap.named[31].layout);
    drop(heap);
    assert_eq!(vm.used(), baseline);
    let filler = account
        .reserve(Resources {
            bytes: limits.bytes - charge + 1,
            ..Resources::default()
        })
        .unwrap();
    let filled = vm.used();
    assert!(AotHeap::new_with_records(&account, 1, &records).is_err());
    assert_eq!(vm.used(), filled);
    drop(filler);
    let mut invalid = records.clone();
    invalid[0].fields[0].1 = ValueType::RecordRef(0);
    assert!(AotHeap::new_with_records(&account, 1, &invalid).is_err());
    assert_eq!(vm.used(), baseline);
    drop((account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

fn raw_bytes(heap: &mut AotHeap, data: &[u8]) -> u64 {
    let mut buffer = heap.heap.prepare_bytes(data.len()).unwrap();
    buffer.data.extend_from_slice(data);
    heap.heap
        .allocate_bytes_chunk(&mut Some(buffer), &mut 0)
        .unwrap()
        .unwrap()
        .encoded()
}

#[test]
fn string_literals_copy_static_data_in_chunks_with_gc_and_distinct_tags() {
    for value in [
        Vec::new(),
        b"literal\0\n".to_vec(),
        [vec![b'a'; 255], "😀中文".as_bytes().to_vec()].concat(),
        "😀".repeat(16384).into_bytes(),
    ] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let before = vm.used();
        let arguments = [value.as_ptr() as usize as i64, value.len() as i64, 0];
        let (status, reference) =
            unsafe { bytes(&mut context, wings_abi::HEAP_STRING_LITERAL, arguments) };
        assert_eq!(status, STATUS_OK);
        assert_eq!(vm.used().bytes, before.bytes + value.len() as u64);
        let reference = heap.heap.import_reference(reference).unwrap();
        assert_eq!(heap.heap.string_bytes(reference).unwrap(), value);
        assert!(heap.heap.bytes(reference).is_err());
        heap.commit_typed(wings_abi::ValueType::StringRef, reference.encoded() as i64)
            .unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.heap.string_bytes(reference).unwrap(), value);
        heap.committed = None;
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert!(heap.heap.import_reference(reference.encoded()).is_err());
        assert_eq!(vm.used(), before);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn string_literal_invalid_input_budget_and_cancel_leave_output_and_charges_unchanged() {
    for value in [
        vec![0xff],
        vec![0xc0, 0xaf],
        vec![0xed, 0xa0, 0x80],
        [vec![b'a'; 255], vec![0xf0, 0x9f]].concat(),
    ] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let before = vm.used();
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [value.as_ptr() as usize as i64, value.len() as i64, 0],
                )
            },
            (wings_abi::STATUS_INVALID_STRING, 79)
        );
        assert_eq!(vm.used(), before);
    }
    let value = vec![b'a'; 65536];
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let before = vm.used();
    for length in [-1, 65537, i64::MAX] {
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [value.as_ptr() as usize as i64, length, 0],
                )
            },
            (wings_abi::STATUS_INVALID_STRING, 79)
        );
    }
    assert_eq!(
        unsafe { bytes(&mut context, wings_abi::HEAP_STRING_LITERAL, [0, 0, 0]) },
        (wings_abi::STATUS_INVALID_STRING, 79)
    );
    for steps in [0, 1, 64, 128, 255, 256] {
        context.remaining_steps = steps;
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [value.as_ptr() as usize as i64, 65536, 0],
                )
            },
            (wings_abi::STATUS_BUDGET_EXHAUSTED, 79)
        );
        assert_eq!(vm.used(), before);
    }
    context.remaining_steps = 4096;
    flag.store(1, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        unsafe {
            bytes(
                &mut context,
                wings_abi::HEAP_STRING_LITERAL,
                [value.as_ptr() as usize as i64, 65536, 0],
            )
        },
        (wings_abi::STATUS_CANCELLED, 79)
    );
    assert_eq!(vm.used(), before);
}

#[test]
fn string_literal_storage_admission_rolls_back_at_all_three_levels() {
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    for level in 0..3 {
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        let mut heap = AotHeap::new(&account, 4).unwrap();
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let levels = [&vm, &tenant, &account];
        let charge = levels[level]
            .reserve(Resources {
                bytes: limits.bytes - levels[level].used().bytes - 4,
                ..Resources::default()
            })
            .unwrap();
        let before = levels.map(ResourceAccount::used);
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [b"hello".as_ptr() as usize as i64, 5, 0],
                )
            },
            (STATUS_HEAP_EXHAUSTED, 79)
        );
        assert_eq!(levels.map(ResourceAccount::used), before);
        drop(charge);
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [b"hello".as_ptr() as usize as i64, 5, 0],
                )
            }
            .0,
            STATUS_OK
        );
        drop((heap, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn string_values_validate_utf8_boundaries_and_copy_independent_storage() {
    use wings_abi::{HEAP_STRING_BYTES, HEAP_STRING_FROM_BYTES, HEAP_STRING_LEN};
    for data in [
        Vec::new(),
        b"hello\0world".to_vec(),
        "中文😀e\u{301}".as_bytes().to_vec(),
        [vec![b'a'; 255], "😀中文".as_bytes().to_vec()].concat(),
        "😀".repeat(16384).into_bytes(),
    ] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let original = raw_bytes(&mut heap, &data);
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let baseline = vm.used();
        let (status, string) = unsafe {
            bytes(
                &mut context,
                HEAP_STRING_FROM_BYTES,
                [original as i64, 0, 0],
            )
        };
        assert_eq!(status, STATUS_OK);
        assert_eq!(vm.used().bytes, baseline.bytes + data.len() as u64);
        let reference = heap.heap.import_reference(string).unwrap();
        assert_eq!(heap.heap.string_bytes(reference).unwrap(), data);
        assert!(heap.heap.bytes(reference).is_err());
        assert!(heap.heap.get(reference, 0).is_err());
        assert!(
            heap.commit_typed(wings_abi::ValueType::BytesRef, string as i64)
                .is_err()
        );
        heap.commit_typed(wings_abi::ValueType::StringRef, string as i64)
            .unwrap();
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_STRING_LEN, [string as i64, 0, 0]) },
            (
                STATUS_OK,
                std::str::from_utf8(&data).unwrap().chars().count() as u64
            )
        );
        let (status, copy) =
            unsafe { bytes(&mut context, HEAP_STRING_BYTES, [string as i64, 0, 0]) };
        assert_eq!(status, STATUS_OK);
        let copy = heap.heap.import_reference(copy).unwrap();
        assert_eq!(heap.heap.bytes(copy).unwrap(), data);
        assert!(heap.heap.string_bytes(copy).is_err());
        assert!(
            heap.commit_typed(wings_abi::ValueType::StringRef, copy.encoded() as i64)
                .is_err()
        );
        assert!(heap.stats().collections > 0);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn string_values_reject_invalid_utf8_wrong_tags_and_stale_references() {
    use wings_abi::{
        HEAP_STRING_BYTES, HEAP_STRING_FROM_BYTES, HEAP_STRING_LEN, STATUS_INVALID_STRING,
    };
    for data in [
        vec![0x80],
        vec![0xc0, 0xaf],
        vec![0xed, 0xa0, 0x80],
        vec![0xf4, 0x90, 0x80, 0x80],
        vec![0xe4, 0xb8],
        [vec![b'a'; 255], vec![0xe4, 0xb8]].concat(),
        [vec![b'a'; 256], vec![0xff]].concat(),
    ] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let original = raw_bytes(&mut heap, &data);
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let baseline = vm.used();
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    HEAP_STRING_FROM_BYTES,
                    [original as i64, 0, 0],
                )
            },
            (STATUS_INVALID_STRING, 79)
        );
        for operation in [HEAP_STRING_BYTES, HEAP_STRING_LEN] {
            assert_eq!(
                unsafe { bytes(&mut context, operation, [original as i64, 0, 0]) },
                (STATUS_INVALID_ROOT, 79)
            );
        }
        assert_eq!(vm.used(), baseline);
        heap.committed = None;
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    HEAP_STRING_FROM_BYTES,
                    [original as i64, 0, 0],
                )
            },
            (STATUS_INVALID_ROOT, 79)
        );
    }
}

#[test]
fn string_concatenation_keeps_both_roots_at_gc_checkpoints_and_enforces_limit() {
    use wings_abi::{
        HEAP_CONCAT_STRINGS, HEAP_STRING_FROM_BYTES, HEAP_STRING_LEN, STATUS_INVALID_STRING,
    };
    let (vm, mut heap) = fixture(8);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let original = raw_bytes(&mut heap, &vec![b'a'; 32768]);
    heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
        .unwrap();
    let (status, string) = unsafe {
        bytes(
            &mut context,
            HEAP_STRING_FROM_BYTES,
            [original as i64, 0, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    heap.commit_typed(wings_abi::ValueType::StringRef, string as i64)
        .unwrap();
    let (status, doubled) = unsafe {
        bytes(
            &mut context,
            HEAP_CONCAT_STRINGS,
            [string as i64, string as i64, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    let roots = [string, doubled];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: roots.as_ptr(),
        count: roots.len(),
    };
    context.roots = &frame;
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_STRING_LEN, [doubled as i64, 0, 0]) },
        (STATUS_OK, 65536)
    );
    let before = vm.used();
    assert_eq!(
        unsafe {
            bytes(
                &mut context,
                HEAP_CONCAT_STRINGS,
                [doubled as i64, string as i64, 0],
            )
        },
        (STATUS_INVALID_STRING, 79)
    );
    assert_eq!(vm.used(), before);
    assert_eq!(
        heap.heap
            .string_bytes(heap.heap.import_reference(doubled).unwrap())
            .unwrap(),
        vec![b'a'; 65536]
    );
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn string_copy_budget_and_cancellation_release_unpublished_reservations() {
    use wings_abi::{
        HEAP_CONCAT_STRINGS, HEAP_STRING_BYTES, HEAP_STRING_FROM_BYTES, HEAP_STRING_LEN,
    };
    for operation in [
        HEAP_STRING_FROM_BYTES,
        HEAP_STRING_BYTES,
        HEAP_STRING_LEN,
        HEAP_CONCAT_STRINGS,
    ] {
        let budgets: &[u64] = if operation == HEAP_STRING_FROM_BYTES {
            &[0, 1, 2, 64, 128, 256, 258, 300, 512]
        } else {
            &[0, 1, 2, 64, 128, 256]
        };
        for &steps in budgets {
            let (vm, mut heap) = fixture(4);
            let flag = std::sync::atomic::AtomicU32::new(0);
            let mut context = context(&mut heap, &flag);
            context.slow_path = Some(resume_one_step);
            let original = raw_bytes(&mut heap, &vec![b'a'; 65536]);
            heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
                .unwrap();
            let (status, string) = unsafe {
                bytes(
                    &mut context,
                    HEAP_STRING_FROM_BYTES,
                    [original as i64, 0, 0],
                )
            };
            assert_eq!(status, STATUS_OK);
            let roots = [original, string];
            let frame = ShadowFrame {
                previous: std::ptr::null(),
                slots: roots.as_ptr(),
                count: roots.len(),
            };
            context.roots = &frame;
            let source = if operation == HEAP_STRING_FROM_BYTES {
                original
            } else {
                string
            };
            let other = if operation == HEAP_CONCAT_STRINGS {
                let empty = raw_bytes(&mut heap, &[]);
                let (status, empty) =
                    unsafe { bytes(&mut context, HEAP_STRING_FROM_BYTES, [empty as i64, 0, 0]) };
                assert_eq!(status, STATUS_OK);
                heap.commit_typed(wings_abi::ValueType::StringRef, empty as i64)
                    .unwrap();
                empty
            } else {
                0
            };
            let before = vm.used();
            context.remaining_steps = steps;
            assert_eq!(
                unsafe { bytes(&mut context, operation, [source as i64, other as i64, 0]) },
                (wings_abi::STATUS_BUDGET_EXHAUSTED, 79)
            );
            assert_eq!(vm.used(), before);
            context.remaining_steps = 4096;
            flag.store(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                unsafe { bytes(&mut context, operation, [source as i64, other as i64, 0]) },
                (wings_abi::STATUS_CANCELLED, 79)
            );
            assert_eq!(vm.used(), before);
            drop(heap);
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

#[test]
fn string_copy_admission_rejects_at_each_resource_level_without_leaking() {
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    for level in 0..3 {
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        let mut heap = AotHeap::new(&account, 4).unwrap();
        let original = raw_bytes(&mut heap, b"hello");
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let levels = [&vm, &tenant, &account];
        let guard = levels[level]
            .reserve(Resources {
                bytes: limits.bytes - levels[level].used().bytes - 4,
                ..Resources::default()
            })
            .unwrap();
        let before = levels.map(ResourceAccount::used);
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_FROM_BYTES,
                    [original as i64, 0, 0],
                )
            },
            (STATUS_HEAP_EXHAUSTED, 79)
        );
        assert_eq!(levels.map(ResourceAccount::used), before);
        drop(guard);
        let (status, _) = unsafe {
            bytes(
                &mut context,
                wings_abi::HEAP_STRING_FROM_BYTES,
                [original as i64, 0, 0],
            )
        };
        assert_eq!(status, STATUS_OK);
        drop((heap, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

fn fixture(capacity: usize) -> (ResourceAccount, Box<AotHeap>) {
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let records = [wings_abi::RecordSchema {
        name: "Box".into(),
        fields: vec![("value".into(), wings_abi::ValueType::Int)],
    }];
    (
        vm,
        AotHeap::new_with_records(&account, capacity, &records).unwrap(),
    )
}

fn context(heap: &mut AotHeap, flag: &std::sync::atomic::AtomicU32) -> ScalarContext {
    ScalarContext {
        cancellation: flag,
        remaining_steps: 4096,
        yield_request: flag,
        slice_remaining: 1,
        slow_path: Some(collect_on_resume),
        data: std::ptr::null_mut(),
        stack_limit: 0,
        roots: std::ptr::null(),
        heap_call: None,
        heap_data: std::ptr::from_mut(heap).cast(),
        effect_call: None,
    }
}

unsafe extern "C" fn collect_on_resume(pointer: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *pointer };
    let heap = unsafe { &mut *context.heap_data.cast::<AotHeap>() };
    if unsafe { heap.collect(context.roots) }.is_err() {
        return STATUS_INVALID_ROOT;
    }
    context.slice_remaining = 1;
    STATUS_OK
}

unsafe extern "C" fn resume_one_step(pointer: *mut ScalarContext) -> u32 {
    unsafe { (*pointer).slice_remaining = 1 };
    STATUS_OK
}

#[test]
fn string_index_and_slice_keep_sources_live_through_every_gc_checkpoint() {
    for text in [
        String::new(),
        "Aé中😀Z\0e\u{301}".into(),
        "a".repeat(255) + "é中😀z",
        "a".repeat(65536),
    ] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let (status, source) = unsafe {
            bytes(
                &mut context,
                wings_abi::HEAP_STRING_LITERAL,
                [text.as_ptr() as usize as i64, text.len() as i64, 0],
            )
        };
        assert_eq!(status, STATUS_OK);
        let roots = [source];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: roots.as_ptr(),
            count: 1,
        };
        context.roots = &frame;
        context.slow_path = Some(collect_on_resume);
        let count = text.chars().count();
        let baseline = vm.used();
        for index in [0, 1, 255, 256, 257, count.saturating_sub(1)] {
            let Some(expected) = text.chars().nth(index) else {
                continue;
            };
            context.remaining_steps = 4096;
            assert_eq!(
                unsafe {
                    bytes(
                        &mut context,
                        wings_abi::HEAP_STRING_AT,
                        [source as i64, index as i64, 0],
                    )
                },
                (STATUS_OK, u64::from(expected))
            );
            assert_eq!(vm.used(), baseline);
        }
        for (start, length) in [
            (0, 0),
            (0, count),
            (count, 0),
            (count.saturating_sub(1), usize::from(count > 0)),
            (1.min(count), count.saturating_sub(2)),
            (255.min(count), 3.min(count.saturating_sub(255))),
        ] {
            context.remaining_steps = 4096;
            let (status, result) = unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_SLICE,
                    [source as i64, start as i64, length as i64],
                )
            };
            assert_eq!(status, STATUS_OK);
            assert_ne!(result, source);
            let reference = heap.heap.import_reference(result).unwrap();
            let expected: String = text.chars().skip(start).take(length).collect();
            assert_eq!(
                heap.heap.string_bytes(reference).unwrap(),
                expected.as_bytes()
            );
            unsafe { heap.collect(context.roots) }.unwrap();
            assert_eq!(vm.used(), baseline);
            assert_eq!(heap.stats().allocated, 1);
        }
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn string_index_and_slice_reject_bounds_budget_cancel_and_stale_types_without_output() {
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let text = "a".repeat(65536);
    let (status, source) = unsafe {
        bytes(
            &mut context,
            wings_abi::HEAP_STRING_LITERAL,
            [text.as_ptr() as usize as i64, text.len() as i64, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    let baseline = vm.used();
    for (operation, start, length) in [
        (wings_abi::HEAP_STRING_AT, -1, 0),
        (wings_abi::HEAP_STRING_AT, 65536, 0),
        (wings_abi::HEAP_STRING_AT, i64::MAX, 0),
        (wings_abi::HEAP_STRING_SLICE, -1, 0),
        (wings_abi::HEAP_STRING_SLICE, 0, -1),
        (wings_abi::HEAP_STRING_SLICE, 65536, 1),
        (wings_abi::HEAP_STRING_SLICE, 65537, 0),
        (wings_abi::HEAP_STRING_SLICE, i64::MAX, i64::MAX),
    ] {
        context.remaining_steps = 4096;
        assert_eq!(
            unsafe { bytes(&mut context, operation, [source as i64, start, length]) },
            (wings_abi::STATUS_INVALID_STRING_INDEX, 79)
        );
        assert_eq!(vm.used(), baseline);
    }
    for (operation, start, length, budgets) in [
        (wings_abi::HEAP_STRING_AT, 65535, 0, &[0, 1, 128, 255][..]),
        (
            wings_abi::HEAP_STRING_SLICE,
            0,
            65536,
            &[0, 1, 256, 300][..],
        ),
    ] {
        for steps in budgets {
            context.remaining_steps = *steps;
            assert_eq!(
                unsafe { bytes(&mut context, operation, [source as i64, start, length]) },
                (wings_abi::STATUS_BUDGET_EXHAUSTED, 79)
            );
            assert_eq!(vm.used(), baseline);
        }
        context.remaining_steps = 4096;
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            unsafe { bytes(&mut context, operation, [source as i64, start, length]) },
            (wings_abi::STATUS_CANCELLED, 79)
        );
        flag.store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(vm.used(), baseline);
    }
    let (status, wrong_type) =
        unsafe { bytes(&mut context, wings_abi::HEAP_FILL_BYTES, [0, 0, 0]) };
    assert_eq!(status, STATUS_OK);
    for operation in [wings_abi::HEAP_STRING_AT, wings_abi::HEAP_STRING_SLICE] {
        assert_eq!(
            unsafe { bytes(&mut context, operation, [wrong_type as i64, 0, 0]) },
            (STATUS_INVALID_ROOT, 79)
        );
    }
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    for operation in [wings_abi::HEAP_STRING_AT, wings_abi::HEAP_STRING_SLICE] {
        assert_eq!(
            unsafe { bytes(&mut context, operation, [source as i64, 0, 0]) },
            (STATUS_INVALID_ROOT, 79)
        );
    }
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn string_equality_compares_contents_without_allocating_and_survives_every_gc_checkpoint() {
    let cases = [
        (String::new(), String::new(), true),
        ("中😀\0".into(), "中😀\0".into(), true),
        ("é".into(), "e\u{301}".into(), false),
        ("a".repeat(65536), "a".repeat(65536), true),
        ("a".repeat(255), "a".repeat(255), true),
        ("a".repeat(256), "a".repeat(256), true),
        ("a".repeat(257), "a".repeat(257), true),
        ("b".to_string() + &"a".repeat(256), "a".repeat(257), false),
        ("a".repeat(65535) + "b", "a".repeat(65536), false),
        ("a".repeat(256), "a".repeat(257), false),
    ];
    for (first, second, expected) in cases {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let mut sources = [0; 2];
        for (index, value) in [first, second].iter().enumerate() {
            let (status, reference) = unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_STRING_LITERAL,
                    [value.as_ptr() as usize as i64, value.len() as i64, 0],
                )
            };
            assert_eq!(status, STATUS_OK);
            sources[index] = reference;
        }
        assert_ne!(sources[0], sources[1]);
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: sources.as_ptr(),
            count: 2,
        };
        context.roots = &frame;
        context.slow_path = Some(collect_on_resume);
        let before = vm.used();
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_EQUAL_STRINGS,
                    [sources[0] as i64, sources[1] as i64, 0],
                )
            },
            (STATUS_OK, u64::from(expected))
        );
        assert_eq!(vm.used(), before);
        assert!(heap.stats().collections > 0);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn string_equality_checks_budget_cancel_invalid_types_and_stale_roots_without_output_commit() {
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let value = "a".repeat(65536);
    let (status, source) = unsafe {
        bytes(
            &mut context,
            wings_abi::HEAP_STRING_LITERAL,
            [value.as_ptr() as usize as i64, value.len() as i64, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    let before = vm.used();
    for steps in [0, 1, 128, 256] {
        context.remaining_steps = steps;
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    wings_abi::HEAP_EQUAL_STRINGS,
                    [source as i64, source as i64, 0],
                )
            },
            (wings_abi::STATUS_BUDGET_EXHAUSTED, 79)
        );
        assert_eq!(vm.used(), before);
    }
    context.remaining_steps = 4096;
    flag.store(1, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        unsafe {
            bytes(
                &mut context,
                wings_abi::HEAP_EQUAL_STRINGS,
                [source as i64, source as i64, 0],
            )
        },
        (wings_abi::STATUS_CANCELLED, 79)
    );
    flag.store(0, std::sync::atomic::Ordering::SeqCst);
    let (status, wrong_type) =
        unsafe { bytes(&mut context, wings_abi::HEAP_FILL_BYTES, [0, 0, 0]) };
    assert_eq!(status, STATUS_OK);
    for operands in [
        [wrong_type as i64, source as i64, 0],
        [source as i64, wrong_type as i64, 0],
        [0, source as i64, 0],
    ] {
        assert_eq!(
            unsafe { bytes(&mut context, wings_abi::HEAP_EQUAL_STRINGS, operands) },
            (STATUS_INVALID_ROOT, 79)
        );
    }
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(
        unsafe {
            bytes(
                &mut context,
                wings_abi::HEAP_EQUAL_STRINGS,
                [source as i64, source as i64, 0],
            )
        },
        (STATUS_INVALID_ROOT, 79)
    );
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn string_exports_retain_types_and_enforce_combined_limits_and_cleanup() {
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let text = "😀".repeat(16384);
    let (status, source) = unsafe {
        bytes(
            &mut context,
            wings_abi::HEAP_STRING_LITERAL,
            [text.as_ptr() as usize as i64, text.len() as i64, 0],
        )
    };
    assert_eq!(status, STATUS_OK);
    heap.commit_typed(wings_abi::ValueType::StringRef, source as i64)
        .unwrap();
    let baseline = vm.used();
    assert!(matches!(
        unsafe { export_buffers(&mut context, &mut [source as i64], 1, 0) },
        Err(STATUS_INVALID_ROOT)
    ));
    assert!(matches!(
        unsafe { export_buffers(&mut context, &mut [source as i64], 1, 1) },
        Err(STATUS_INVALID_CONTROL)
    ));
    assert!(matches!(
        unsafe { export_buffers(&mut context, &mut [source as i64], 0, 2) },
        Err(STATUS_INVALID_CONTROL)
    ));
    assert!(matches!(
        unsafe { export_buffers(&mut context, &mut [source as i64; 2], 0, 3) },
        Err(wings_abi::STATUS_SEND_REJECTED)
    ));
    for steps in [0, 1, 128, 256] {
        context.remaining_steps = steps;
        assert!(matches!(
            unsafe { export_buffers(&mut context, &mut [source as i64], 0, 1) },
            Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
        ));
        assert_eq!(vm.used(), baseline);
    }
    context.remaining_steps = 4096;
    flag.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        unsafe { export_buffers(&mut context, &mut [source as i64], 0, 1) },
        Err(wings_abi::STATUS_CANCELLED)
    ));
    assert_eq!(vm.used(), baseline);
    flag.store(0, std::sync::atomic::Ordering::SeqCst);
    let mut arguments = [source as i64];
    let payload = unsafe { export_buffers(&mut context, &mut arguments, 0, 1) }.unwrap();
    assert_eq!(payload.string_mask, 1);
    assert!(payload.valid(&arguments, 1));
    assert_eq!(payload.data, text.as_bytes());
    assert_eq!(vm.used().bytes, baseline.bytes + 65536);
    drop(heap);
    assert_eq!(vm.used().bytes, 65536);
    drop(payload);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn mixed_string_imports_validate_utf8_and_preserve_incoming_gc_roots() {
    let (vm, mut heap) = fixture(4);
    for _ in 0..4 {
        heap.heap
            .allocate(heap.named[0].layout, &[RecordValue::Int(0)])
            .unwrap();
    }
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let text = "a".repeat(255) + "中😀\0";
    let mut payload = BytePayload::new(&heap.account, 7, text.len() + 2).unwrap();
    payload.string_mask = 5;
    payload.data.extend_from_slice(text.as_bytes());
    payload.data.extend_from_slice(&[255, 0]);
    let mut arguments = [
        ListPayload::encode(0, text.len()),
        ListPayload::encode(text.len(), 2),
        ListPayload::encode(text.len() + 2, 0),
    ];
    unsafe {
        import_arguments(
            &mut context,
            &mut arguments,
            &ListPayload::default(),
            &payload,
        )
    }
    .unwrap();
    for (index, expected) in [(0, text.as_bytes()), (2, &[][..])] {
        let reference = heap.heap.import_reference(arguments[index] as u64).unwrap();
        assert_eq!(heap.heap.string_bytes(reference).unwrap(), expected);
        assert!(heap.heap.bytes(reference).is_err());
    }
    let reference = heap.heap.import_reference(arguments[1] as u64).unwrap();
    assert_eq!(heap.heap.bytes(reference).unwrap(), &[255, 0]);
    assert!(heap.heap.string_bytes(reference).is_err());
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    let baseline = vm.used();
    payload.string_mask = 7;
    let mut invalid = [
        ListPayload::encode(0, text.len()),
        ListPayload::encode(text.len(), 2),
        ListPayload::encode(text.len() + 2, 0),
    ];
    assert_eq!(
        unsafe {
            import_arguments(
                &mut context,
                &mut invalid,
                &ListPayload::default(),
                &payload,
            )
        },
        Err(wings_abi::STATUS_INVALID_STRING)
    );
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(vm.used(), baseline);
    payload.string_mask = 8;
    assert!(!payload.valid(&invalid, 7));
    drop((payload, heap));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn string_import_budget_cancel_and_three_level_admission_roll_back() {
    let limits = Resources {
        bytes: 1 << 20,
        ..Resources::default()
    };
    for level in 0..3 {
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        let mut heap = AotHeap::new(&account, 4).unwrap();
        let mut payload = BytePayload::new(&account, 1, 65536).unwrap();
        payload.string_mask = 1;
        payload.data.resize(65536, 97);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let levels = [&vm, &tenant, &account];
        let guard = levels[level]
            .reserve(Resources {
                bytes: limits.bytes - levels[level].used().bytes - 4,
                ..Resources::default()
            })
            .unwrap();
        let baseline = levels.map(ResourceAccount::used);
        let encoded = ListPayload::encode(0, 65536);
        assert_eq!(
            unsafe {
                import_arguments(
                    &mut context,
                    &mut [encoded],
                    &ListPayload::default(),
                    &payload,
                )
            },
            Err(STATUS_HEAP_EXHAUSTED)
        );
        assert_eq!(levels.map(ResourceAccount::used), baseline);
        drop(guard);
        let baseline = levels.map(ResourceAccount::used);
        for steps in [0, 1, 64, 128, 256, 257] {
            context.remaining_steps = steps;
            assert_eq!(
                unsafe {
                    import_arguments(
                        &mut context,
                        &mut [encoded],
                        &ListPayload::default(),
                        &payload,
                    )
                },
                Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
            );
            assert_eq!(levels.map(ResourceAccount::used), baseline);
        }
        context.remaining_steps = 4096;
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            unsafe {
                import_arguments(
                    &mut context,
                    &mut [encoded],
                    &ListPayload::default(),
                    &payload,
                )
            },
            Err(wings_abi::STATUS_CANCELLED)
        );
        assert_eq!(levels.map(ResourceAccount::used), baseline);
        flag.store(0, std::sync::atomic::Ordering::SeqCst);
        unsafe {
            import_arguments(
                &mut context,
                &mut [encoded],
                &ListPayload::default(),
                &payload,
            )
        }
        .unwrap();
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(levels.map(ResourceAccount::used), baseline);
        drop((payload, heap, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn byte_exports_are_independent_accounted_and_bounded_before_publication() {
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let (status, source) =
        unsafe { bytes(&mut context, wings_abi::HEAP_FILL_BYTES, [65536, 201, 0]) };
    assert_eq!(status, STATUS_OK);
    heap.commit_typed(wings_abi::ValueType::BytesRef, source as i64)
        .unwrap();
    let baseline = vm.used();
    let mut arguments = [source as i64];
    let payload = unsafe { export_bytes(&mut context, &mut arguments, 1) }.unwrap();
    assert!(payload.valid(&arguments, 1));
    assert_eq!(payload.data, vec![201; MAX_MESSAGE_BYTES]);
    assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
    let mut arguments = [source as i64, source as i64];
    assert!(matches!(
        unsafe { export_bytes(&mut context, &mut arguments, 3) },
        Err(wings_abi::STATUS_SEND_REJECTED)
    ));
    assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
    for steps in [0, 2, 32, 128, 256] {
        context.remaining_steps = steps;
        let mut arguments = [source as i64];
        assert!(matches!(
            unsafe { export_bytes(&mut context, &mut arguments, 1) },
            Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
        ));
        assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
    }
    context.remaining_steps = 4096;
    flag.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        unsafe { export_bytes(&mut context, &mut [source as i64], 1) },
        Err(wings_abi::STATUS_CANCELLED)
    ));
    flag.store(0, std::sync::atomic::Ordering::SeqCst);
    heap.committed = None;
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    assert_eq!(payload.data, vec![201; MAX_MESSAGE_BYTES]);
    drop(heap);
    assert_eq!(vm.used().bytes, MAX_MESSAGE_BYTES as u64);
    drop(payload);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn byte_imports_root_mixed_arguments_at_every_copy_and_collection_checkpoint() {
    let (vm, mut heap) = fixture(6);
    for _ in 0..6 {
        heap.heap
            .allocate(heap.named[0].layout, &[RecordValue::Int(0)])
            .unwrap();
    }
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let mut bytes = BytePayload::new(&heap.account, 0b11001, MAX_MESSAGE_BYTES).unwrap();
    bytes
        .data
        .extend((0..MAX_MESSAGE_BYTES).map(|index| (index % 251) as u8));
    let lists = ListPayload {
        mask: 4,
        count: 2,
        values: std::array::from_fn(|index| if index == 0 { i64::MIN } else { i64::MAX }),
    };
    let mut arguments = [
        ListPayload::encode(0, 32768),
        17,
        ListPayload::encode(0, 2),
        ListPayload::encode(32768, 32768),
        ListPayload::encode(65536, 0),
    ];
    unsafe { import_arguments(&mut context, &mut arguments, &lists, &bytes) }.unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 6);
    assert_eq!(arguments[1], 17);
    for (index, expected) in [
        (0, &bytes.data[..32768]),
        (3, &bytes.data[32768..]),
        (4, &bytes.data[0..0]),
    ] {
        let reference = heap.heap.import_reference(arguments[index] as u64).unwrap();
        assert_eq!(heap.heap.bytes(reference).unwrap(), expected);
    }
    let Some((first, tail)) = heap.list_node(arguments[2]).unwrap() else {
        panic!("missing imported list")
    };
    assert_eq!(first, i64::MIN);
    assert_eq!(heap.list_node(tail).unwrap().unwrap().0, i64::MAX);
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    assert_eq!(bytes.data.len(), MAX_MESSAGE_BYTES);
    drop((heap, bytes));
    assert_eq!(vm.used(), Resources::default());
}

unsafe fn bytes(context: &mut ScalarContext, operation: u32, arguments: [i64; 3]) -> (u32, u64) {
    let mut output = 79;
    let status = unsafe {
        heap_call(
            context,
            operation,
            arguments.as_ptr() as usize as u64,
            &mut output,
        )
    };
    (status, output)
}

#[test]
fn byte_calls_keep_source_live_through_every_copy_checkpoint_and_validate_shape() {
    use wings_abi::{
        HEAP_BYTE_AT, HEAP_BYTES_LEN, HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, STATUS_INVALID_BYTES,
    };
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let (status, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 255, 0]) };
    assert_eq!(status, STATUS_OK);
    heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
        .unwrap();
    let (status, replaced) =
        unsafe { bytes(&mut context, HEAP_REPLACE_BYTE, [original as i64, 32768, 7]) };
    assert_eq!(status, STATUS_OK);
    assert!(heap.stats().collections > 500);
    let slots = [replaced];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: 1,
    };
    context.roots = &frame;
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_BYTES_LEN, [replaced as i64, 0, 0]) },
        (STATUS_OK, 65536)
    );
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_BYTE_AT, [replaced as i64, 32768, 0]) },
        (STATUS_OK, 7)
    );
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_BYTE_AT, [original as i64, 32768, 0]) },
        (STATUS_OK, 255)
    );
    let baseline = vm.used();
    for (operation, arguments) in [
        (HEAP_FILL_BYTES, [-1, 0, 0]),
        (HEAP_FILL_BYTES, [65537, 0, 0]),
        (HEAP_FILL_BYTES, [0, -1, 0]),
        (HEAP_FILL_BYTES, [1, 256, 0]),
        (HEAP_BYTE_AT, [original as i64, -1, 0]),
        (HEAP_BYTE_AT, [original as i64, 65536, 0]),
        (HEAP_REPLACE_BYTE, [original as i64, 0, -1]),
        (HEAP_REPLACE_BYTE, [original as i64, 0, 256]),
    ] {
        assert_eq!(
            unsafe { bytes(&mut context, operation, arguments) },
            (STATUS_INVALID_BYTES, 79)
        );
        assert_eq!(vm.used(), baseline);
    }
    assert!(
        heap.commit_typed(wings_abi::ValueType::RecordRef(0), original as i64)
            .is_err()
    );
    assert!(heap.commit_list(original as i64).is_err());
    let record = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(3)])
        .unwrap();
    assert_eq!(
        unsafe {
            bytes(
                &mut context,
                HEAP_BYTES_LEN,
                [record.encoded() as i64, 0, 0],
            )
        },
        (STATUS_INVALID_ROOT, 79)
    );
    assert!(
        heap.commit_typed(wings_abi::ValueType::BytesRef, record.encoded() as i64)
            .is_err()
    );
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn byte_builder_budget_and_cancellation_release_unpublished_data_at_every_chunk() {
    use wings_abi::{
        HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, HEAP_REPLACE_I64_BE, STATUS_BUDGET_EXHAUSTED,
        STATUS_CANCELLED,
    };
    for budget in [0, 1, 2, 16, 64, 128, 255, 256] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let (_, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 9, 0]) };
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let baseline = vm.used();
        for operation in [HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, HEAP_REPLACE_I64_BE] {
            context.remaining_steps = budget;
            context.slice_remaining = 1;
            let arguments = if operation == HEAP_FILL_BYTES {
                [65536, 7, 0]
            } else {
                [original as i64, 42, 7]
            };
            assert_eq!(
                unsafe { bytes(&mut context, operation, arguments) },
                (STATUS_BUDGET_EXHAUSTED, 79)
            );
            assert_eq!(vm.used(), baseline);
            assert_eq!(heap.stats().allocated, 1);
        }
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        context.remaining_steps = 4096;
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 1, 0]) },
            (STATUS_CANCELLED, 79)
        );
        assert_eq!(vm.used(), baseline);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn byte_i64_access_is_big_endian_signed_unaligned_immutable_and_rooted() {
    use wings_abi::{HEAP_FILL_BYTES, HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE};
    for value in [i64::MIN, -65537, -1, 0, 1, 65537, i64::MAX] {
        for offset in [0, 1, 8] {
            let (vm, mut heap) = fixture(4);
            let flag = std::sync::atomic::AtomicU32::new(0);
            let mut context = context(&mut heap, &flag);
            let (status, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [16, 0, 0]) };
            assert_eq!(status, STATUS_OK);
            heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
                .unwrap();
            let (status, replacement) = unsafe {
                bytes(
                    &mut context,
                    HEAP_REPLACE_I64_BE,
                    [original as i64, offset, value],
                )
            };
            assert_eq!(status, STATUS_OK);
            let original_ref = heap.heap.import_reference(original).unwrap();
            let replacement_ref = heap.heap.import_reference(replacement).unwrap();
            assert_eq!(heap.heap.bytes(original_ref).unwrap(), [0; 16]);
            let mut expected = [0; 16];
            expected[offset as usize..offset as usize + 8].copy_from_slice(&value.to_be_bytes());
            assert_eq!(heap.heap.bytes(replacement_ref).unwrap(), expected);
            assert_eq!(
                unsafe {
                    bytes(
                        &mut context,
                        HEAP_READ_I64_BE,
                        [replacement as i64, offset, 0],
                    )
                },
                (STATUS_OK, value as u64)
            );
            heap.commit_typed(wings_abi::ValueType::BytesRef, replacement as i64)
                .unwrap();
            unsafe { heap.collect(std::ptr::null()) }.unwrap();
            assert_eq!(heap.stats().allocated, 1);
            assert_eq!(heap.heap.bytes(replacement_ref).unwrap(), expected);
            drop(heap);
            assert_eq!(vm.used(), Resources::default());
        }
    }
}

#[test]
fn byte_i64_range_rejections_do_not_allocate_or_change_output() {
    use wings_abi::{HEAP_FILL_BYTES, HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE, STATUS_INVALID_BYTES};
    for length in [0, 1, 7, 8, 16] {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let (status, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [length, 255, 0]) };
        assert_eq!(status, STATUS_OK);
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let baseline = vm.used();
        for offset in [i64::MIN, -1, length.saturating_sub(7), length, i64::MAX] {
            for operation in [HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE] {
                assert_eq!(
                    unsafe { bytes(&mut context, operation, [original as i64, offset, i64::MIN]) },
                    (STATUS_INVALID_BYTES, 79)
                );
                assert_eq!(vm.used(), baseline);
                assert_eq!(heap.stats().allocated, 1);
            }
        }
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn zero_bytes_and_slot_gc_retry_keep_control_and_data_charges_separate() {
    use wings_abi::{HEAP_BYTE_AT, HEAP_BYTES_LEN, HEAP_FILL_BYTES, STATUS_INVALID_BYTES};
    let (vm, mut heap) = fixture(1);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let baseline = vm.used();
    let (status, empty) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [0, 0, 0]) };
    assert_eq!(status, STATUS_OK);
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_BYTES_LEN, [empty as i64, 0, 0]) },
        (STATUS_OK, 0)
    );
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_BYTE_AT, [empty as i64, 0, 0]) },
        (STATUS_INVALID_BYTES, 79)
    );
    assert_eq!(vm.used(), baseline);
    let (status, next) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 9, 0]) };
    assert_eq!(status, STATUS_OK);
    assert_ne!(empty, next);
    assert_eq!(heap.stats().allocated, 1);
    assert_eq!(heap.stats().collections, 1);
    assert_eq!(vm.used().bytes, baseline.bytes + 512);
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn byte_quota_retry_reclaims_only_unreachable_data_and_rejects_live_overflow() {
    use wings_abi::{HEAP_FILL_BYTES, HEAP_REPLACE_BYTE};
    let (vm, mut heap) = fixture(4);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slow_path = Some(resume_one_step);
    let baseline = vm.used();
    let guard = vm
        .reserve(Resources {
            bytes: (1 << 20) - baseline.bytes - 512,
            ..Resources::default()
        })
        .unwrap();
    let (status, stale) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 1, 0]) };
    assert_eq!(status, STATUS_OK);
    let (status, current) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 2, 0]) };
    assert_eq!(status, STATUS_OK);
    assert_ne!(stale, current);
    assert_eq!(heap.stats().collections, 1);
    assert_eq!(heap.stats().allocated, 1);
    heap.commit_typed(wings_abi::ValueType::BytesRef, current as i64)
        .unwrap();
    let full = vm.used();
    assert_eq!(
        unsafe { bytes(&mut context, HEAP_REPLACE_BYTE, [current as i64, 0, 3]) },
        (STATUS_HEAP_EXHAUSTED, 79)
    );
    assert_eq!(vm.used(), full);
    assert_eq!(heap.stats().collections, 2);
    assert_eq!(heap.stats().allocated, 1);
    let reference = heap.heap.import_reference(current).unwrap();
    assert_eq!(heap.heap.bytes(reference).unwrap(), [2; 512]);
    drop(guard);
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn list_exports_poll_each_node_keep_source_roots_and_reject_stale_layouts() {
    let (vm, mut heap) = fixture(68);
    let mut tail = heap
        .heap
        .allocate(heap.empty_list, &[RecordValue::Unit])
        .unwrap();
    for value in 0..65 {
        tail = heap
            .heap
            .allocate(
                heap.cons_list,
                &[RecordValue::Int(value), RecordValue::Ref(tail)],
            )
            .unwrap();
    }
    let nodes = [tail.encoded()];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: nodes.as_ptr(),
        count: 1,
    };
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.roots = &frame;
    let mut arguments = [tail.encoded() as i64];
    assert!(matches!(
        unsafe { export_lists(&mut context, &mut arguments, 1) },
        Err(wings_abi::STATUS_SEND_REJECTED)
    ));
    assert_eq!(arguments[0], tail.encoded() as i64);
    let Some((_, shorter)) = heap.list_node(tail.encoded() as i64).unwrap() else {
        panic!("expected nonempty list")
    };
    let mut arguments = [shorter];
    context.remaining_steps = 65;
    let payload = unsafe { export_lists(&mut context, &mut arguments, 1) }.unwrap();
    assert!(payload.valid(&arguments, 1));
    assert_eq!(context.remaining_steps, 0);
    assert_eq!(payload.count, 64);
    assert_eq!(
        payload.values,
        std::array::from_fn(|index| 63 - index as i64)
    );
    let mut arguments = [shorter];
    context.remaining_steps = 2;
    assert!(matches!(
        unsafe { export_lists(&mut context, &mut arguments, 1) },
        Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
    ));
    flag.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        unsafe { export_lists(&mut context, &mut arguments, 1) },
        Err(wings_abi::STATUS_CANCELLED)
    ));
    flag.store(0, std::sync::atomic::Ordering::SeqCst);
    context.remaining_steps = 4096;
    let record = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(9)])
        .unwrap();
    let mut arguments = [record.encoded() as i64];
    context.slice_remaining = 1;
    assert!(matches!(
        unsafe { export_lists(&mut context, &mut arguments, 1) },
        Err(STATUS_INVALID_ROOT)
    ));
    context.roots = std::ptr::null();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    let mut arguments = [shorter];
    assert!(matches!(
        unsafe { export_lists(&mut context, &mut arguments, 1) },
        Err(STATUS_INVALID_ROOT)
    ));
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn list_imports_root_each_tail_and_prior_mixed_arguments_at_every_checkpoint() {
    let (vm, mut heap) = fixture(8);
    for _ in 0..8 {
        heap.heap
            .allocate(heap.named[0].layout, &[RecordValue::Int(0)])
            .unwrap();
    }
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    context.slice_remaining = 0;
    let payload = ListPayload {
        mask: 0b10101,
        count: 4,
        values: std::array::from_fn(|index| [i64::MIN, i64::MAX, -7, 9][index % 4]),
    };
    let mut arguments = [
        ListPayload::encode(0, 2),
        17,
        ListPayload::encode(2, 2),
        1,
        ListPayload::encode(4, 0),
    ];
    unsafe {
        import_arguments(
            &mut context,
            &mut arguments,
            &payload,
            &BytePayload::default(),
        )
    }
    .unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(arguments[1], 17);
    assert_eq!(heap.stats().allocated, 7);
    for (index, expected) in [(0, vec![i64::MIN, i64::MAX]), (2, vec![-7, 9]), (4, vec![])] {
        let mut current = arguments[index];
        for value in expected {
            let Some((actual, tail)) = heap.list_node(current).unwrap() else {
                panic!("short list")
            };
            assert_eq!(actual, value);
            current = tail;
        }
        assert_eq!(heap.list_node(current).unwrap(), None);
    }
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    let mut arguments = [0; 64];
    let empty_payload = ListPayload {
        mask: u64::MAX,
        ..ListPayload::default()
    };
    context.remaining_steps = 4096;
    context.slow_path = Some(resume_one_step);
    assert_eq!(
        unsafe {
            import_arguments(
                &mut context,
                &mut arguments,
                &empty_payload,
                &BytePayload::default(),
            )
        },
        Err(STATUS_HEAP_EXHAUSTED)
    );
    assert_eq!(heap.stats().allocated, 8);
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn all_64_empty_list_parameters_have_independent_incoming_roots() {
    let (vm, mut heap) = fixture(64);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = context(&mut heap, &flag);
    let mut arguments = [0; 64];
    let payload = ListPayload {
        mask: u64::MAX,
        ..ListPayload::default()
    };
    unsafe {
        import_arguments(
            &mut context,
            &mut arguments,
            &payload,
            &BytePayload::default(),
        )
    }
    .unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 64);
    for argument in arguments {
        assert_eq!(heap.list_node(argument).unwrap(), None);
    }
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn incoming_parameters_survive_gc_before_entry_and_later_parameter_allocation() {
    let (vm, mut heap) = fixture(3);
    let committed = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(7)])
        .unwrap();
    heap.commit_typed(
        wings_abi::ValueType::RecordRef(0),
        committed.encoded() as i64,
    )
    .unwrap();
    heap.heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(8)])
        .unwrap();
    heap.heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(9)])
        .unwrap();
    let cancellation = std::sync::atomic::AtomicU32::new(0);
    let yielding = std::sync::atomic::AtomicU32::new(0);
    let mut context = ScalarContext {
        cancellation: &cancellation,
        remaining_steps: 128,
        yield_request: &yielding,
        slice_remaining: 128,
        slow_path: None,
        data: std::ptr::null_mut(),
        stack_limit: 0,
        roots: std::ptr::null(),
        heap_call: None,
        heap_data: std::ptr::from_mut(&mut *heap).cast(),
        effect_call: None,
    };
    let mut arguments = [0, 0];
    let lists = ListPayload {
        mask: 3,
        ..ListPayload::default()
    };
    unsafe {
        import_arguments(
            &mut context,
            &mut arguments,
            &lists,
            &BytePayload::default(),
        )
    }
    .unwrap();
    assert_eq!(heap.list_node(arguments[0]), Ok(None));
    assert_eq!(heap.list_node(arguments[1]), Ok(None));
    let first = heap.heap.import_reference(arguments[0] as u64).unwrap();
    let second = heap.heap.import_reference(arguments[1] as u64).unwrap();
    heap.incoming = std::array::from_fn(|index| Some(if index % 2 == 0 { first } else { second }));
    let frame_slots = [arguments[1] as u64; MAX_SHADOW_ROOTS];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: frame_slots.as_ptr(),
        count: frame_slots.len(),
    };
    unsafe { heap.collect(&frame) }.unwrap();
    assert_eq!(heap.stats().allocated, 3);
    assert_eq!(heap.list_node(arguments[0]), Ok(None));
    heap.clear_incoming();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 1);
    assert_eq!(heap.list_node(arguments[0]), Err(STATUS_INVALID_ROOT));
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn typed_state_roots_reject_other_shapes_and_trace_list_tails() {
    let (vm, mut heap) = fixture(5);
    let empty = heap
        .heap
        .allocate(heap.empty_list, &[RecordValue::Unit])
        .unwrap();
    let tail = heap
        .heap
        .allocate(
            heap.cons_list,
            &[RecordValue::Int(7), RecordValue::Ref(empty)],
        )
        .unwrap();
    let head = heap
        .heap
        .allocate(
            heap.cons_list,
            &[RecordValue::Int(11), RecordValue::Ref(tail)],
        )
        .unwrap();
    let record = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(99)])
        .unwrap();
    assert!(heap.commit_list(record.encoded() as i64).is_err());
    assert!(
        heap.commit_typed(wings_abi::ValueType::RecordRef(0), head.encoded() as i64)
            .is_err()
    );
    heap.commit_list(head.encoded() as i64).unwrap();
    let used = vm.used();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 3);
    assert!(heap.heap.get(record, 0).is_err());
    assert_eq!(heap.heap.get(tail, 0).unwrap(), RecordValue::Int(7));
    heap.commit_list(empty.encoded() as i64).unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 1);
    assert!(heap.commit_list(head.encoded() as i64).is_err());
    assert_eq!(vm.used(), used);
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn committed_root_is_independent_from_sixty_four_compiler_slots() {
    let (vm, mut heap) = fixture(2);
    let committed = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(7)])
        .unwrap();
    let candidate = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(9)])
        .unwrap();
    heap.commit_typed(
        wings_abi::ValueType::RecordRef(0),
        committed.encoded() as i64,
    )
    .unwrap();
    let slots = [candidate.encoded(); MAX_SHADOW_ROOTS];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: slots.len(),
    };
    unsafe { heap.collect(&frame) }.unwrap();
    assert_eq!(heap.stats().allocated, 2);
    let invalid_slots = [0];
    let invalid = ShadowFrame {
        previous: &frame,
        slots: invalid_slots.as_ptr(),
        count: 1,
    };
    let before = heap.stats();
    assert!(unsafe { heap.collect(&invalid) }.is_err());
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.heap.get(committed, 0).unwrap(), RecordValue::Int(7));
    heap.commit_typed(
        wings_abi::ValueType::RecordRef(0),
        candidate.encoded() as i64,
    )
    .unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 1);
    assert!(heap.heap.get(committed, 0).is_err());
    assert!(
        heap.commit_typed(
            wings_abi::ValueType::RecordRef(0),
            committed.encoded() as i64
        )
        .is_err()
    );
    assert_eq!(heap.committed, Some(candidate));
    assert!(
        heap.commit_typed(wings_abi::ValueType::RecordRef(0), 0)
            .is_err()
    );
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.heap.get(candidate, 0).unwrap(), RecordValue::Int(9));
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn nested_compiler_frames_keep_only_explicit_references() {
    let (vm, mut heap) = fixture(3);
    let first = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(11)])
        .unwrap();
    let second = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(22)])
        .unwrap();
    let dead = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(33)])
        .unwrap();
    let parent_slots = [first.encoded()];
    let parent = ShadowFrame {
        previous: std::ptr::null(),
        slots: parent_slots.as_ptr(),
        count: 1,
    };
    let child_slots = [second.encoded(), first.encoded()];
    let child = ShadowFrame {
        previous: &parent,
        slots: child_slots.as_ptr(),
        count: 2,
    };
    let charged = vm.used();
    unsafe { heap.collect(&child) }.unwrap();
    assert_eq!(heap.stats().allocated, 2);
    assert!(heap.heap.get(dead, 0).is_err());
    unsafe { heap.collect(&parent) }.unwrap();
    assert_eq!(heap.stats().allocated, 1);
    assert_eq!(heap.heap.get(first, 0).unwrap(), RecordValue::Int(11));
    assert!(heap.heap.get(second, 0).is_err());
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    assert_eq!(heap.stats().allocated, 0);
    assert_eq!(vm.used(), charged);
    drop(heap);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn invalid_roots_and_frame_limits_never_start_collection() {
    let (_, mut heap) = fixture(2);
    let live = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(1)])
        .unwrap();
    let slots = [live.encoded(), 0];
    let invalid_handle = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: 2,
    };
    let null_slots = ShadowFrame {
        previous: std::ptr::null(),
        slots: std::ptr::null(),
        count: 1,
    };
    let oversized = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: 65,
    };
    let mut cycle = ShadowFrame {
        previous: std::ptr::null(),
        slots: std::ptr::null(),
        count: 0,
    };
    cycle.previous = &cycle;
    for frame in [&invalid_handle, &null_slots, &oversized, &cycle] {
        let before = heap.stats();
        assert!(unsafe { heap.collect(frame) }.is_err());
        assert_eq!(heap.stats(), before);
        assert_eq!(heap.heap.get(live, 0).unwrap(), RecordValue::Int(1));
    }
    let slots = [live.encoded(); 64];
    let parent = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: 64,
    };
    let child = ShadowFrame {
        previous: &parent,
        slots: slots.as_ptr(),
        count: 1,
    };
    let before = heap.stats();
    assert!(unsafe { heap.collect(&child) }.is_err());
    assert_eq!(heap.stats(), before);
    unsafe { heap.collect(&parent) }.unwrap();
    assert_eq!(heap.stats().allocated, 1);
}

#[test]
fn stale_roots_fail_before_reclaiming_valid_objects() {
    let (_, mut heap) = fixture(1);
    let stale = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(1)])
        .unwrap();
    unsafe { heap.collect(std::ptr::null()) }.unwrap();
    let current = heap
        .heap
        .allocate(heap.named[0].layout, &[RecordValue::Int(2)])
        .unwrap();
    let slots = [stale.encoded()];
    let frame = ShadowFrame {
        previous: std::ptr::null(),
        slots: slots.as_ptr(),
        count: 1,
    };
    let before = heap.stats();
    assert!(unsafe { heap.collect(&frame) }.is_err());
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.heap.get(current, 0).unwrap(), RecordValue::Int(2));
}
