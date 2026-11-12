use wings_vm::{
    CollectionStats, FieldType, RecordHeap, RecordValue, ResourceAccount, ResourceLevel, Resources,
    ShadowRoots,
};

fn bytes(amount: u64) -> Resources {
    Resources {
        bytes: amount,
        ..Resources::default()
    }
}

fn accounts(vm_limit: u64, tenant_limit: u64, process_limit: u64) -> [ResourceAccount; 3] {
    let vm = ResourceAccount::vm(bytes(vm_limit));
    let tenant = vm.tenant(bytes(tenant_limit)).expect("tenant account");
    let process = tenant
        .process(bytes(process_limit))
        .expect("process account");
    [vm, tenant, process]
}

fn assert_usage(accounts: &[ResourceAccount; 3], expected: u64) {
    for account in accounts {
        assert_eq!(account.used(), bytes(expected), "{:?}", account.level());
    }
}

fn verify_validation() {
    let accounts = accounts(1 << 20, 1 << 20, 1 << 20);
    for (slots, layouts) in [(0, 1), (1025, 1), (1, 0), (1, 129)] {
        assert!(RecordHeap::new(&accounts[2], slots, layouts).is_err());
        assert_usage(&accounts, 0);
    }
    for account in &accounts[..2] {
        assert!(RecordHeap::new(account, 4, 4).is_err());
        assert_usage(&accounts, 0);
    }
    let mut heap = RecordHeap::new(&accounts[2], 8, 3).expect("validation heap");
    let scalar = heap
        .register_layout(&[FieldType::Int])
        .expect("scalar layout");
    let leaf = heap
        .allocate(scalar, &[RecordValue::Int(i64::MIN)])
        .expect("leaf");
    let fields = [
        FieldType::Int,
        FieldType::Bool,
        FieldType::Unit,
        FieldType::Ref,
    ];
    let layout = heap.register_layout(&fields).expect("four field types");
    for boolean in [false, true] {
        let values = [
            RecordValue::Int(i64::MAX),
            RecordValue::Bool(boolean),
            RecordValue::Unit,
            RecordValue::Ref(leaf),
        ];
        let reference = heap.allocate(layout, &values).expect("typed record");
        for (index, value) in values.into_iter().enumerate() {
            assert_eq!(heap.get(reference, index).expect("typed field"), value);
        }
        let before = heap.stats();
        for index in [4, usize::MAX] {
            assert!(heap.get(reference, index).is_err());
            assert_eq!(heap.stats(), before);
        }
        for invalid in [&values[..3], &[RecordValue::Unit; 5][..]] {
            assert!(heap.allocate(layout, invalid).is_err());
            assert_eq!(heap.stats(), before);
        }
        for index in 0..values.len() {
            let mut invalid = values;
            invalid[index] = match values[index] {
                RecordValue::Unit => RecordValue::Bool(false),
                _ => RecordValue::Unit,
            };
            assert!(heap.allocate(layout, &invalid).is_err());
            assert_eq!(heap.stats(), before);
        }
    }
    let empty = heap.register_layout(&[]).expect("empty layout");
    let empty_record = heap.allocate(empty, &[]).expect("empty record");
    let before = heap.stats();
    assert!(heap.get(empty_record, 0).is_err());
    assert!(heap.register_layout(&[FieldType::Unit; 9]).is_err());
    assert!(heap.register_layout(&[FieldType::Bool]).is_err());
    assert_eq!(
        heap.register_layout(&fields).expect("duplicate layout"),
        layout
    );
    assert_eq!(heap.stats(), before);

    let mut foreign = RecordHeap::new(&accounts[2], 2, 2).expect("foreign heap");
    let foreign_layout = foreign.register_layout(&fields).expect("foreign schema");
    let foreign_scalar = foreign
        .register_layout(&[FieldType::Int])
        .expect("foreign scalar");
    let foreign_leaf = foreign
        .allocate(foreign_scalar, &[RecordValue::Int(7)])
        .expect("foreign leaf");
    let values = [
        RecordValue::Int(1),
        RecordValue::Bool(true),
        RecordValue::Unit,
        RecordValue::Ref(leaf),
    ];
    let before = heap.stats();
    let foreign_before = foreign.stats();
    assert!(heap.allocate(foreign_layout, &values).is_err());
    let mut invalid = values;
    invalid[3] = RecordValue::Ref(foreign_leaf);
    assert!(heap.allocate(layout, &invalid).is_err());
    assert!(heap.get(foreign_leaf, 0).is_err());
    assert!(foreign.get(leaf, 0).is_err());
    let foreign_roots = ShadowRoots::new(&foreign, 1).expect("foreign roots");
    assert!(heap.collect(&foreign_roots).is_err());
    assert_eq!(heap.stats(), before);
    assert_eq!(foreign.stats(), foreign_before);
    assert_eq!(
        heap.get(leaf, 0).expect("unchanged leaf"),
        RecordValue::Int(i64::MIN)
    );
    drop(foreign_roots);
    drop(foreign);
    assert_usage(&accounts, heap.stats().charged_bytes);
    drop(heap);
    assert_usage(&accounts, 0);
}

fn verify_graph() {
    let accounts = accounts(1 << 20, 1 << 20, 1 << 20);
    let mut heap = RecordHeap::new(&accounts[2], 128, 2).expect("bounded graph heap");
    let scalar = heap
        .register_layout(&[FieldType::Int])
        .expect("leaf layout");
    let branch = heap
        .register_layout(&[
            FieldType::Int,
            FieldType::Ref,
            FieldType::Ref,
            FieldType::Ref,
            FieldType::Ref,
            FieldType::Ref,
            FieldType::Ref,
            FieldType::Ref,
        ])
        .expect("shared branch layout");
    let leaf = heap
        .allocate(scalar, &[RecordValue::Int(-1)])
        .expect("graph leaf");
    let mut chain = Vec::with_capacity(127);
    chain.push(leaf);
    for index in 1..127 {
        let previous = chain[index - 1];
        let reference = heap
            .allocate(
                branch,
                &[
                    RecordValue::Int(index as i64),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(previous),
                    RecordValue::Ref(leaf),
                ],
            )
            .expect("long shared chain");
        chain.push(reference);
    }
    let garbage = heap
        .allocate(scalar, &[RecordValue::Int(999)])
        .expect("unreachable graph leaf");
    let charge = heap.stats().charged_bytes;
    let mut roots = ShadowRoots::new(&heap, 64).expect("graph roots");
    assert_usage(&accounts, charge + roots.charged_bytes());
    {
        let mut frame = roots.frame();
        for index in 0..64 {
            assert_eq!(
                frame.push(&heap, chain[126]).expect("duplicate graph root"),
                index
            );
        }
        assert!(frame.push(&heap, leaf).is_err());
        for (before, reclaimed) in [(128, 1), (127, 0)] {
            assert_eq!(
                frame
                    .collect(&mut heap)
                    .expect("bounded worklist collection"),
                CollectionStats {
                    before,
                    retained: 127,
                    reclaimed,
                }
            );
            for index in 1..chain.len() {
                assert_eq!(
                    heap.get(chain[index], 0).expect("unmoved node"),
                    RecordValue::Int(index as i64)
                );
                for field in 1..7 {
                    assert_eq!(
                        heap.get(chain[index], field).expect("shared predecessor"),
                        RecordValue::Ref(chain[index - 1])
                    );
                }
                assert_eq!(
                    heap.get(chain[index], 7).expect("shared leaf"),
                    RecordValue::Ref(leaf)
                );
            }
            assert_eq!(
                heap.get(leaf, 0).expect("retained leaf"),
                RecordValue::Int(-1)
            );
            assert!(heap.get(garbage, 0).is_err());
        }
    }
    assert_eq!(
        heap.collect(&roots).expect("dropped graph roots"),
        CollectionStats {
            before: 127,
            retained: 0,
            reclaimed: 127,
        }
    );
    for reference in chain {
        assert!(heap.get(reference, 0).is_err());
    }
    drop(roots);
    assert_usage(&accounts, charge);
    drop(heap);
    assert_usage(&accounts, 0);
}

fn verify_roots_and_stale() {
    let accounts = accounts(1 << 20, 1 << 20, 1 << 20);
    let mut heap = RecordHeap::new(&accounts[2], 4, 2).expect("root heap");
    let scalar = heap
        .register_layout(&[FieldType::Int])
        .expect("root scalar");
    let reference_layout = heap
        .register_layout(&[FieldType::Ref])
        .expect("reference layout");
    let first = heap
        .allocate(scalar, &[RecordValue::Int(1)])
        .expect("first root");
    let second = heap
        .allocate(scalar, &[RecordValue::Int(2)])
        .expect("second root");
    let third = heap
        .allocate(scalar, &[RecordValue::Int(3)])
        .expect("third root");
    let mut roots = ShadowRoots::new(&heap, 2).expect("nested roots");
    {
        let mut outer = roots.frame();
        assert_eq!(outer.push(&heap, first).expect("outer root"), 0);
        {
            let mut inner = outer.frame();
            assert!(inner.set(&heap, 0, second).is_err());
            assert_eq!(inner.push(&heap, second).expect("relative inner index"), 0);
            assert!(inner.push(&heap, third).is_err());
            assert!(inner.set(&heap, 1, third).is_err());
            assert!(inner.set(&heap, usize::MAX, third).is_err());
            inner.set(&heap, 0, third).expect("replace inner root");
            {
                let mut nested = inner.frame();
                assert!(nested.set(&heap, 0, second).is_err());
                assert!(nested.push(&heap, second).is_err());
                assert_eq!(
                    nested.collect(&mut heap).expect("three nested frames"),
                    CollectionStats {
                        before: 3,
                        retained: 2,
                        reclaimed: 1,
                    }
                );
            }
            assert_eq!(
                heap.get(first, 0).expect("parent preserved"),
                RecordValue::Int(1)
            );
            assert_eq!(
                heap.get(third, 0).expect("replacement preserved"),
                RecordValue::Int(3)
            );
            assert!(heap.get(second, 0).is_err());
        }
        assert_eq!(
            outer.collect(&mut heap).expect("inner frame dropped"),
            CollectionStats {
                before: 2,
                retained: 1,
                reclaimed: 1,
            }
        );
        assert!(heap.get(third, 0).is_err());
        let before = heap.stats();
        assert!(outer.push(&heap, second).is_err());
        assert!(outer.set(&heap, 0, third).is_err());
        assert!(
            heap.allocate(reference_layout, &[RecordValue::Ref(second)])
                .is_err()
        );
        assert_eq!(heap.stats(), before);
        let replacement = heap
            .allocate(scalar, &[RecordValue::Int(4)])
            .expect("reused slot");
        assert_ne!(replacement, second);
        assert!(heap.get(second, 0).is_err());
        assert_eq!(
            outer
                .push(&heap, replacement)
                .expect("reused root capacity"),
            1
        );
        assert_eq!(
            outer.collect(&mut heap).expect("failed set kept parent"),
            CollectionStats {
                before: 2,
                retained: 2,
                reclaimed: 0,
            }
        );
    }
    assert_eq!(
        heap.collect(&roots).expect("outer dropped"),
        CollectionStats {
            before: 2,
            retained: 0,
            reclaimed: 2,
        }
    );

    let stale = heap
        .allocate(scalar, &[RecordValue::Int(5)])
        .expect("soon stale root");
    let empty_roots = ShadowRoots::new(&heap, 1).expect("independent root set");
    {
        let mut frame = roots.frame();
        frame.push(&heap, stale).expect("publish soon stale root");
        heap.collect(&empty_roots)
            .expect("invalidate other root set");
        let live = heap
            .allocate(scalar, &[RecordValue::Int(6)])
            .expect("same slot new generation");
        assert_ne!(stale, live);
        frame
            .push(&heap, live)
            .expect("valid root alongside stale root");
        let before = heap.stats();
        let used = accounts[2].used();
        assert!(frame.collect(&mut heap).is_err());
        assert_eq!(heap.stats(), before);
        assert_eq!(accounts[2].used(), used);
        assert_eq!(
            heap.get(live, 0).expect("invalid roots change nothing"),
            RecordValue::Int(6)
        );
        assert!(heap.get(stale, 0).is_err());
        frame.set(&heap, 0, live).expect("repair stale root");
        assert_eq!(
            frame.collect(&mut heap).expect("repaired roots"),
            CollectionStats {
                before: 1,
                retained: 1,
                reclaimed: 0,
            }
        );
    }
    heap.collect(&roots).expect("clear repaired roots");
    drop(empty_roots);
    drop(roots);
    assert_usage(&accounts, heap.stats().charged_bytes);
    drop(heap);
    assert_usage(&accounts, 0);
}

fn verify_capacity_and_integer_precision() {
    let accounts = accounts(1 << 20, 1 << 20, 1 << 20);
    let mut heap = RecordHeap::new(&accounts[2], 1024, 1).expect("maximum bounded heap");
    let layout = heap
        .register_layout(&[FieldType::Int])
        .expect("integer layout");
    let victim = heap
        .allocate(layout, &[RecordValue::Int(0)])
        .expect("slot zero generation one");
    let integer = heap
        .allocate(layout, &[RecordValue::Int(1_i64 << 32)])
        .expect("reference-shaped integer");
    for value in 2..1024 {
        heap.allocate(layout, &[RecordValue::Int(value)])
            .expect("fill fixed heap");
    }
    let mut roots = ShadowRoots::new(&heap, 1).expect("integer roots");
    {
        let mut frame = roots.frame();
        frame
            .push(&heap, integer)
            .expect("only integer record rooted");
        let before = heap.stats();
        assert!(heap.allocate(layout, &[RecordValue::Int(1024)]).is_err());
        assert_eq!(heap.stats(), before);
        assert_eq!(
            frame.collect(&mut heap).expect("precise integer marking"),
            CollectionStats {
                before: 1024,
                retained: 1,
                reclaimed: 1023,
            }
        );
        assert!(heap.get(victim, 0).is_err());
        assert_eq!(
            heap.get(integer, 0).expect("integer bits unchanged"),
            RecordValue::Int(1_i64 << 32)
        );
        let replacement = heap
            .allocate(layout, &[RecordValue::Int(42)])
            .expect("allocate after full heap GC");
        assert_ne!(replacement, victim);
        assert!(heap.get(victim, 0).is_err());
        assert_eq!(
            heap.get(replacement, 0).expect("new generation"),
            RecordValue::Int(42)
        );
        for value in 2..1024 {
            heap.allocate(layout, &[RecordValue::Int(value)])
                .expect("refill fixed capacity");
        }
        assert_eq!(heap.stats().allocated, 1024);
        assert_eq!(heap.stats().retired, 0);
        let before = heap.stats();
        assert!(heap.allocate(layout, &[RecordValue::Int(0)]).is_err());
        assert_eq!(heap.stats(), before);
    }
    assert_eq!(
        heap.collect(&roots).expect("release full heap"),
        CollectionStats {
            before: 1024,
            retained: 0,
            reclaimed: 1024,
        }
    );
    drop(roots);
    drop(heap);
    assert_usage(&accounts, 0);
}

fn verify_accounting() {
    let generous = 1 << 20;
    let probe = accounts(generous, generous, generous);
    let heap = RecordHeap::new(&probe[2], 8, 2).expect("measure heap charge");
    let heap_bytes = heap.stats().charged_bytes;
    assert!(heap_bytes > 0);
    assert_usage(&probe, heap_bytes);
    for capacity in [0, 65] {
        assert!(ShadowRoots::new(&heap, capacity).is_err());
        assert_usage(&probe, heap_bytes);
    }
    let roots = ShadowRoots::new(&heap, 4).expect("measure root charge");
    let root_bytes = roots.charged_bytes();
    assert!(root_bytes > 0);
    assert_usage(&probe, heap_bytes + root_bytes);
    drop(heap);
    assert_usage(&probe, root_bytes);
    drop(roots);
    assert_usage(&probe, 0);

    for (level, position) in [
        (ResourceLevel::Process, 2),
        (ResourceLevel::Tenant, 1),
        (ResourceLevel::Vm, 0),
    ] {
        for limit in [
            heap_bytes - 1,
            heap_bytes + root_bytes - 1,
            heap_bytes + root_bytes,
        ] {
            let mut limits = [generous; 3];
            limits[position] = limit;
            let accounts = accounts(limits[0], limits[1], limits[2]);
            if limit < heap_bytes {
                let error = RecordHeap::new(&accounts[2], 8, 2)
                    .err()
                    .expect("heap admission rejected");
                assert!(error.contains(&format!("{level:?}")), "{error}");
                assert_usage(&accounts, 0);
                continue;
            }
            let heap = RecordHeap::new(&accounts[2], 8, 2).expect("admitted heap");
            assert_usage(&accounts, heap_bytes);
            if limit < heap_bytes + root_bytes {
                let before = heap.stats();
                let error = ShadowRoots::new(&heap, 4)
                    .err()
                    .expect("root admission rejected");
                assert!(error.contains(&format!("{level:?}")), "{error}");
                assert_eq!(heap.stats(), before);
                assert_usage(&accounts, heap_bytes);
            } else {
                let roots = ShadowRoots::new(&heap, 4).expect("exact budget accepted");
                assert_usage(&accounts, heap_bytes + root_bytes);
                assert!(ShadowRoots::new(&heap, 1).is_err());
                assert_usage(&accounts, heap_bytes + root_bytes);
                drop(roots);
                assert_usage(&accounts, heap_bytes);
            }
            drop(heap);
            assert_usage(&accounts, 0);
        }
    }
}

fn main() {
    verify_validation();
    verify_graph();
    verify_roots_and_stale();
    verify_capacity_and_integer_precision();
    verify_accounting();
    println!(
        "Record heap verified: schema rejection, bounded shared graph, precise GC, nested roots, stale generations, fixed capacity and three-level accounting"
    );
}
