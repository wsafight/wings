use wings_vm::{
    CollectionStats, FieldType, RecordHeap, RecordValue, ResourceAccount, Resources, ShadowRoots,
};

fn limit(bytes: u64) -> Resources {
    Resources {
        bytes,
        ..Resources::default()
    }
}

fn accounts(bytes: u64) -> [ResourceAccount; 3] {
    let vm = ResourceAccount::vm(limit(bytes));
    let tenant = vm.tenant(limit(bytes)).unwrap();
    let process = tenant.process(limit(bytes)).unwrap();
    [vm, tenant, process]
}

fn heap(capacity: usize, layouts: usize) -> RecordHeap {
    RecordHeap::new(&accounts(1 << 20)[2], capacity, layouts).unwrap()
}

#[test]
fn constructor_validates_account_level_and_capacity_before_charging() {
    let accounts = accounts(1 << 20);
    for account in &accounts[..2] {
        assert!(RecordHeap::new(account, 1, 1).is_err());
    }
    for (capacity, layouts) in [(0, 1), (1025, 1), (1, 0), (1, 129), (usize::MAX, 1)] {
        assert!(RecordHeap::new(&accounts[2], capacity, layouts).is_err());
    }
    assert!(
        accounts
            .iter()
            .all(|account| account.used() == Resources::default())
    );
}

#[test]
fn maximum_declared_capacity_is_bounded_and_charged_until_drop() {
    let accounts = accounts(1 << 20);
    let heap = RecordHeap::new(&accounts[2], 1024, 128).unwrap();
    let stats = heap.stats();
    assert_eq!(
        (
            stats.capacity,
            stats.allocated,
            stats.layouts,
            stats.retired,
            stats.collections
        ),
        (1024, 0, 0, 0, 0)
    );
    assert!(stats.charged_bytes > 0);
    assert!(
        accounts
            .iter()
            .all(|account| account.used() == limit(stats.charged_bytes))
    );
    drop(heap);
    assert!(
        accounts
            .iter()
            .all(|account| account.used() == Resources::default())
    );
}

#[test]
fn heap_quota_failure_at_each_level_rolls_back_all_charges() {
    for level in 0..3 {
        let mut limits = [limit(1 << 20); 3];
        limits[level] = limit(1);
        let vm = ResourceAccount::vm(limits[0]);
        let tenant = vm.tenant(limits[1]).unwrap();
        let process = tenant.process(limits[2]).unwrap();
        assert!(RecordHeap::new(&process, 4, 2).is_err());
        assert!(
            [vm, tenant, process]
                .iter()
                .all(|account| account.used() == Resources::default())
        );
    }
}

#[test]
fn layouts_are_deduplicated_even_when_the_table_is_full() {
    let mut heap = heap(1, 1);
    let first = heap.register_layout(&[FieldType::Int]).unwrap();
    assert_eq!(heap.register_layout(&[FieldType::Int]).unwrap(), first);
    let before = heap.stats();
    assert!(heap.register_layout(&[FieldType::Bool]).is_err());
    assert!(heap.register_layout(&[FieldType::Int; 9]).is_err());
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.register_layout(&[FieldType::Int]).unwrap(), first);
}

#[test]
fn empty_and_eight_field_layouts_are_supported() {
    let mut heap = heap(2, 2);
    let empty = heap.register_layout(&[]).unwrap();
    let reference = heap.allocate(empty, &[]).unwrap();
    assert!(heap.get(reference, 0).is_err());
    let full = heap.register_layout(&[FieldType::Int; 8]).unwrap();
    let values = [RecordValue::Int(i64::MIN); 8];
    let reference = heap.allocate(full, &values).unwrap();
    for (index, value) in values.iter().enumerate() {
        assert_eq!(heap.get(reference, index).unwrap(), *value);
    }
    assert!(heap.get(reference, 8).is_err());
    assert!(heap.get(reference, usize::MAX).is_err());
}

#[test]
fn all_field_types_roundtrip_without_losing_integer_bits() {
    let mut heap = heap(3, 2);
    let integers = heap
        .register_layout(&[FieldType::Int, FieldType::Int])
        .unwrap();
    let leaf = heap
        .allocate(
            integers,
            &[RecordValue::Int(i64::MIN), RecordValue::Int(i64::MAX)],
        )
        .unwrap();
    let mixed = heap
        .register_layout(&[
            FieldType::Bool,
            FieldType::Unit,
            FieldType::Ref,
            FieldType::Bool,
        ])
        .unwrap();
    let values = [
        RecordValue::Bool(false),
        RecordValue::Unit,
        RecordValue::Ref(leaf),
        RecordValue::Bool(true),
    ];
    let parent = heap.allocate(mixed, &values).unwrap();
    for (index, value) in values.iter().enumerate() {
        assert_eq!(heap.get(parent, index).unwrap(), *value);
    }
    assert_eq!(heap.get(leaf, 0).unwrap(), RecordValue::Int(i64::MIN));
    assert_eq!(heap.get(leaf, 1).unwrap(), RecordValue::Int(i64::MAX));
}

#[test]
fn invalid_field_count_or_type_never_consumes_a_slot() {
    let mut heap = heap(1, 1);
    let layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let before = heap.stats();
    for values in [
        vec![],
        vec![RecordValue::Bool(true)],
        vec![RecordValue::Unit],
        vec![RecordValue::Int(1), RecordValue::Int(2)],
    ] {
        assert!(heap.allocate(layout, &values).is_err());
        assert_eq!(heap.stats(), before);
    }
    heap.allocate(layout, &[RecordValue::Int(1)]).unwrap();
}

#[test]
fn foreign_layouts_references_and_roots_are_rejected_without_mutation() {
    let mut first = heap(2, 2);
    let mut second = heap(2, 2);
    let first_layout = first.register_layout(&[FieldType::Int]).unwrap();
    let leaf = first
        .allocate(first_layout, &[RecordValue::Int(4)])
        .unwrap();
    let second_layout = second.register_layout(&[FieldType::Ref]).unwrap();
    let before = second.stats();
    assert!(
        second
            .allocate(first_layout, &[RecordValue::Int(3)])
            .is_err()
    );
    assert!(
        second
            .allocate(second_layout, &[RecordValue::Ref(leaf)])
            .is_err()
    );
    assert!(second.get(leaf, 0).is_err());
    let roots = ShadowRoots::new(&first, 1).unwrap();
    assert!(second.collect(&roots).is_err());
    assert_eq!(second.stats(), before);
    let mut roots = ShadowRoots::new(&second, 1).unwrap();
    assert!(roots.frame().push(&first, leaf).is_err());
    assert!(roots.frame().push(&second, leaf).is_err());
}

#[test]
fn full_heap_requires_explicit_collection_and_rejects_stale_handles_after_reuse() {
    let mut heap = heap(1, 1);
    let layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let first = heap.allocate(layout, &[RecordValue::Int(7)]).unwrap();
    let before = heap.stats();
    assert!(heap.allocate(layout, &[RecordValue::Int(8)]).is_err());
    assert_eq!(heap.stats(), before);
    let roots = ShadowRoots::new(&heap, 1).unwrap();
    assert_eq!(
        heap.collect(&roots).unwrap(),
        CollectionStats {
            before: 1,
            retained: 0,
            reclaimed: 1
        }
    );
    let second = heap.allocate(layout, &[RecordValue::Int(9)]).unwrap();
    assert_ne!(first, second);
    assert!(heap.get(first, 0).unwrap_err().contains("stale"));
    assert_eq!(heap.get(second, 0).unwrap(), RecordValue::Int(9));
}

#[test]
fn graph_collection_preserves_shared_descendants_and_stable_handles() {
    let mut heap = heap(6, 2);
    let leaf_layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let edges = heap
        .register_layout(&[FieldType::Ref, FieldType::Ref])
        .unwrap();
    let leaf = heap.allocate(leaf_layout, &[RecordValue::Int(42)]).unwrap();
    let left = heap.allocate(edges, &[RecordValue::Ref(leaf); 2]).unwrap();
    let right = heap.allocate(edges, &[RecordValue::Ref(leaf); 2]).unwrap();
    let top = heap
        .allocate(edges, &[RecordValue::Ref(left), RecordValue::Ref(right)])
        .unwrap();
    let garbage = heap.allocate(leaf_layout, &[RecordValue::Int(99)]).unwrap();
    let mut roots = ShadowRoots::new(&heap, 2).unwrap();
    {
        let mut frame = roots.frame();
        frame.push(&heap, top).unwrap();
        frame.push(&heap, top).unwrap();
        assert_eq!(
            frame.collect(&mut heap).unwrap(),
            CollectionStats {
                before: 5,
                retained: 4,
                reclaimed: 1
            }
        );
        assert_eq!(heap.get(top, 0).unwrap(), RecordValue::Ref(left));
        assert_eq!(heap.get(leaf, 0).unwrap(), RecordValue::Int(42));
        assert!(heap.get(garbage, 0).is_err());
        assert_eq!(
            frame.collect(&mut heap).unwrap(),
            CollectionStats {
                before: 4,
                retained: 4,
                reclaimed: 0
            }
        );
    }
    assert_eq!(heap.collect(&roots).unwrap().reclaimed, 4);
}

#[test]
fn maximum_depth_graph_is_collected_iteratively() {
    let mut heap = heap(1024, 2);
    let leaf_layout = heap.register_layout(&[]).unwrap();
    let edge = heap.register_layout(&[FieldType::Ref; 8]).unwrap();
    let mut top = heap.allocate(leaf_layout, &[]).unwrap();
    for _ in 1..1024 {
        top = heap.allocate(edge, &[RecordValue::Ref(top); 8]).unwrap();
    }
    let mut roots = ShadowRoots::new(&heap, 1).unwrap();
    {
        let mut frame = roots.frame();
        frame.push(&heap, top).unwrap();
        assert_eq!(frame.collect(&mut heap).unwrap().retained, 1024);
    }
    assert_eq!(heap.collect(&roots).unwrap().reclaimed, 1024);
}

#[test]
fn integer_payload_resembling_an_encoded_reference_does_not_keep_an_object_alive() {
    let mut heap = heap(2, 1);
    let layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let unrooted = heap.allocate(layout, &[RecordValue::Int(7)]).unwrap();
    let rooted = heap
        .allocate(layout, &[RecordValue::Int(1_i64 << 32)])
        .unwrap();
    let mut roots = ShadowRoots::new(&heap, 1).unwrap();
    let mut frame = roots.frame();
    frame.push(&heap, rooted).unwrap();
    assert_eq!(frame.collect(&mut heap).unwrap().reclaimed, 1);
    assert!(heap.get(unrooted, 0).is_err());
    assert_eq!(heap.get(rooted, 0).unwrap(), RecordValue::Int(1_i64 << 32));
}

#[test]
fn nested_frames_cannot_modify_parent_roots_and_drop_only_their_own_roots() {
    let mut heap = heap(2, 1);
    let layout = heap.register_layout(&[]).unwrap();
    let parent_value = heap.allocate(layout, &[]).unwrap();
    let child_value = heap.allocate(layout, &[]).unwrap();
    let mut roots = ShadowRoots::new(&heap, 2).unwrap();
    {
        let mut parent = roots.frame();
        assert_eq!(parent.push(&heap, parent_value).unwrap(), 0);
        {
            let mut child = parent.frame();
            assert!(child.set(&heap, 0, child_value).is_err());
            assert_eq!(child.push(&heap, child_value).unwrap(), 0);
            assert_eq!(child.collect(&mut heap).unwrap().retained, 2);
        }
        assert_eq!(parent.collect(&mut heap).unwrap().reclaimed, 1);
        assert!(parent.set(&heap, 1, parent_value).is_err());
        assert!(parent.set(&heap, 0, child_value).is_err());
        assert_eq!(parent.collect(&mut heap).unwrap().retained, 1);
    }
    assert_eq!(heap.collect(&roots).unwrap().reclaimed, 1);
}

#[test]
fn replacing_root_releases_the_previous_value_and_does_not_grow_capacity() {
    let mut heap = heap(2, 1);
    let layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let first = heap.allocate(layout, &[RecordValue::Int(1)]).unwrap();
    let second = heap.allocate(layout, &[RecordValue::Int(2)]).unwrap();
    let mut roots = ShadowRoots::new(&heap, 1).unwrap();
    let mut frame = roots.frame();
    frame.push(&heap, first).unwrap();
    assert!(frame.push(&heap, second).is_err());
    frame.set(&heap, 0, second).unwrap();
    assert_eq!(frame.collect(&mut heap).unwrap().reclaimed, 1);
    assert!(heap.get(first, 0).is_err());
    assert_eq!(heap.get(second, 0).unwrap(), RecordValue::Int(2));
}

#[test]
fn stale_root_validation_is_atomic_and_collection_count_does_not_change() {
    let mut heap = heap(1, 1);
    let layout = heap.register_layout(&[]).unwrap();
    let value = heap.allocate(layout, &[]).unwrap();
    let mut published = ShadowRoots::new(&heap, 1).unwrap();
    let empty = ShadowRoots::new(&heap, 1).unwrap();
    let mut frame = published.frame();
    frame.push(&heap, value).unwrap();
    heap.collect(&empty).unwrap();
    let new = heap.allocate(layout, &[]).unwrap();
    let before = heap.stats();
    assert!(frame.collect(&mut heap).unwrap_err().contains("stale"));
    assert_eq!(heap.stats(), before);
    frame.set(&heap, 0, new).unwrap();
    assert_eq!(frame.collect(&mut heap).unwrap().retained, 1);
}

#[test]
fn root_capacity_bounds_and_accounting_survive_heap_drop() {
    let accounts = accounts(1 << 20);
    let heap = RecordHeap::new(&accounts[2], 1, 1).unwrap();
    let before = accounts[2].used();
    for capacity in [0, 65, usize::MAX] {
        assert!(ShadowRoots::new(&heap, capacity).is_err());
        assert_eq!(accounts[2].used(), before);
    }
    let roots = ShadowRoots::new(&heap, 64).unwrap();
    assert_eq!(
        accounts[2].used().bytes,
        before.bytes + roots.charged_bytes()
    );
    drop(heap);
    assert_eq!(accounts[2].used(), limit(roots.charged_bytes()));
    drop(roots);
    assert!(
        accounts
            .iter()
            .all(|account| account.used() == Resources::default())
    );
}

#[test]
fn rejected_root_allocation_keeps_the_existing_heap_charge() {
    let generous = accounts(1 << 20);
    let measured = RecordHeap::new(&generous[2], 1, 1)
        .unwrap()
        .stats()
        .charged_bytes;
    for level in 0..3 {
        let mut limits = [limit(1 << 20); 3];
        limits[level] = limit(measured);
        let vm = ResourceAccount::vm(limits[0]);
        let tenant = vm.tenant(limits[1]).unwrap();
        let process = tenant.process(limits[2]).unwrap();
        let heap = RecordHeap::new(&process, 1, 1).unwrap();
        assert!(ShadowRoots::new(&heap, 1).is_err());
        assert_eq!(process.used(), limit(measured));
        assert_eq!(tenant.used(), limit(measured));
        assert_eq!(vm.used(), limit(measured));
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }
}
