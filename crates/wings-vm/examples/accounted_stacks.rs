use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    StackPool,
};

const CONTROL_BYTES: u64 = 4352;

fn limit(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn verify_direct_entries(module: &ScalarModule) {
    for level in [
        wings_vm::ResourceLevel::Process,
        wings_vm::ResourceLevel::Tenant,
        wings_vm::ResourceLevel::Vm,
    ] {
        let vm = ResourceAccount::vm_accounted(limit(4 << 20)).unwrap();
        let bootstrap = vm.used();
        let tenant = vm.tenant(limit(3 << 20)).unwrap();
        let account = tenant.process(limit(1 << 20)).unwrap();
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let mut process = module.instantiate_in_pool(&bound).unwrap();
        let owner = match level {
            wings_vm::ResourceLevel::Process => &account,
            wings_vm::ResourceLevel::Tenant => &tenant,
            wings_vm::ResourceLevel::Vm => &vm,
        };
        let fill = owner
            .reserve(Resources {
                bytes: owner.limit().bytes - owner.used().bytes - CONTROL_BYTES + 1,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        let slots = pool.stats();
        assert!(module.instantiate_in_pool(&bound).is_err());
        assert_eq!(vm.used(), before);
        assert_eq!(pool.stats(), slots);
        assert!(process.invoke("Inc", &[7]).is_err());
        assert_eq!(vm.used(), before);
        assert_eq!(pool.stats(), slots);
        drop(fill);
        assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
        assert_eq!(process.invoke("Inc", &[7]).unwrap(), Some(7));
        let baseline = vm.used();
        let stack_bytes =
            (pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack) as u64;
        let mut execution = process
            .start_controlled(
                "Inc",
                &[2],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert_eq!(account.used().bytes, stack_bytes + CONTROL_BYTES);
        assert_eq!(vm.used().bytes, baseline.bytes + CONTROL_BYTES);
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        assert_eq!(account.used().bytes, stack_bytes + CONTROL_BYTES);
        assert_eq!(
            execution.resume(128).unwrap(),
            ExecutionStatus::Complete(Some(9))
        );
        drop(execution);
        assert_eq!(vm.used(), baseline);
        drop((process, bound, pool, account, tenant));
        assert_eq!(vm.used(), bootstrap);
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.expect("module");
    verify_direct_entries(&module);
    let vm = ResourceAccount::vm(limit(4 * 1024 * 1024));
    let tenant = vm.tenant(vm.limit()).expect("tenant");
    let process_account = tenant.process(vm.limit()).expect("process");
    assert!(StackPool::new_accounted(&vm, 2, 1).is_err());
    let pool = StackPool::new_accounted(&tenant, 2, 1).expect("accounted pool");
    let charged = pool.stats().charged_bytes;
    let slot_bytes =
        (pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack) as u64;
    assert!(charged > 2 * slot_bytes);
    assert_eq!(tenant.used().bytes, charged);
    assert_eq!(vm.used().bytes, charged);
    let owner_pool = pool.for_process(&process_account).expect("bound pool");
    let outsider = vm.tenant(vm.limit()).expect("other tenant");
    assert!(
        pool.for_process(&outsider.process(vm.limit()).expect("outsider"))
            .is_err()
    );
    assert!(pool.for_process(&tenant).is_err());
    assert!(
        StackPool::new(1, 0)
            .expect("probe pool")
            .for_process(&process_account)
            .is_err()
    );
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    let mut process = module.instantiate().expect("instance");
    assert!(
        process
            .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &pool)
            .is_err()
    );
    assert_eq!(process.invoke("Get", &[]), Ok(Some(0)));
    let fill = vm
        .reserve(Resources {
            bytes: vm.limit().bytes - charged,
            ..Resources::default()
        })
        .expect("fill common root");
    let before = pool.stats();
    assert!(
        process
            .start_in_pool("Inc", &[3], 4, &cancellation, &preemption, &owner_pool)
            .is_err()
    );
    assert_eq!(pool.stats(), before);
    assert_eq!(vm.used().bytes, vm.limit().bytes);
    assert_eq!(process_account.used(), Resources::default());
    drop(fill);
    let fill = vm
        .reserve(Resources {
            bytes: vm.limit().bytes - charged - CONTROL_BYTES,
            ..Resources::default()
        })
        .expect("leave exact execution control capacity");
    {
        let mut execution = process
            .start_in_pool("Inc", &[3], 4, &cancellation, &preemption, &owner_pool)
            .expect("net transfer with full VM");
        assert_eq!(process_account.used().bytes, slot_bytes + CONTROL_BYTES);
        assert_eq!(tenant.used().bytes, charged + CONTROL_BYTES);
        assert_eq!(vm.used().bytes, vm.limit().bytes);
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        assert_eq!(
            execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(3)))
        );
        assert_eq!(process_account.used(), Resources::default());
        assert_eq!(pool.stats().cached, 1);
    }
    drop(fill);
    pool.trim().expect("trim cached pages");
    assert_eq!(tenant.used().bytes, charged);
    let denied = tenant
        .process(limit(slot_bytes - 1))
        .expect("small process");
    let denied_pool = pool.for_process(&denied).expect("bind small account");
    let before = pool.stats();
    assert!(
        process
            .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &denied_pool)
            .is_err()
    );
    assert_eq!(pool.stats(), before);
    assert_eq!(denied.used(), Resources::default());
    assert_eq!(process.invoke("Get", &[]), Ok(Some(3)));
    for started in [false, true] {
        let mut process = module.instantiate().expect("abandoned instance");
        {
            let mut execution = process
                .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &owner_pool)
                .expect("abandoned call");
            if started {
                assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
            }
            assert_eq!(process_account.used().bytes, slot_bytes + CONTROL_BYTES);
        }
        assert_eq!(process_account.used(), Resources::default());
        assert_eq!(tenant.used().bytes, charged);
    }
    let mut process = module.instantiate().expect("cancelled instance");
    let cancelled = CancellationToken::default();
    {
        let mut execution = process
            .start_in_pool("Inc", &[1], 4, &cancelled, &preemption, &owner_pool)
            .expect("cancelled call");
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        cancelled.cancel();
        assert!(execution.resume(1).unwrap_err().contains("cancelled"));
        assert_eq!(process_account.used(), Resources::default());
    }
    drop(denied_pool);
    drop(pool);
    let mut process = module.instantiate().expect("survivor");
    let mut execution = process
        .start_in_pool("Inc", &[7], 4, &cancellation, &preemption, &owner_pool)
        .expect("live ownership");
    assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
    drop(owner_pool);
    assert_eq!(tenant.used().bytes, charged + CONTROL_BYTES);
    assert_eq!(
        execution.resume(100),
        Ok(ExecutionStatus::Complete(Some(7)))
    );
    assert_eq!(process_account.used(), Resources::default());
    assert_eq!(tenant.used(), Resources::default());
    assert_eq!(vm.used(), Resources::default());
    for blocked in [false, true] {
        let vm = ResourceAccount::vm(limit(if blocked { 128 } else { 1024 * 1024 }));
        let tenant = vm
            .tenant(limit(if blocked { 1024 * 1024 } else { 128 }))
            .expect("limited tenant");
        assert!(StackPool::new_accounted(&tenant, 2, 1).is_err());
        assert_eq!(tenant.used(), Resources::default());
        assert_eq!(vm.used(), Resources::default());
    }
    println!(
        "Accounted stacks verified: net transfers, quotas, cache ownership and normal cleanup"
    );
}
