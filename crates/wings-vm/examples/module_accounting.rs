use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    ScalarTask, ScalarWorker, StackPool,
};

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 4,
        messages: 4,
        requests: 4,
    }
}

fn verify_execution_binding(directory: &Path) {
    let vm = ResourceAccount::vm_accounted(limits(16 << 20)).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant(limits(12 << 20)).unwrap();
    let account = tenant.process(limits(8 << 20)).unwrap();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    let tools = StackPool::new(1, 0).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let other = tenant.process(limits(8 << 20)).unwrap();
    let foreign = pool.for_process(&other).unwrap();
    let baseline = vm.used();
    assert!(module.instantiate().is_err());
    for invalid in [&tools, &pool] {
        let slots = invalid.stats();
        assert!(module.instantiate_in_pool(invalid).is_err());
        assert_eq!(vm.used(), baseline);
        assert_eq!(invalid.stats(), slots);
    }
    let mut process = module.instantiate_in_pool(&bound).unwrap();
    assert_eq!(process.invoke("Inc", &[7]).unwrap(), Some(7));
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    for owner in [&account, &tenant, &vm] {
        let fill = owner
            .reserve(Resources {
                bytes: owner.limit().bytes - owner.used().bytes,
                ..Resources::default()
            })
            .unwrap();
        let full = vm.used();
        assert!(process.invoke("Inc", &[99]).is_err());
        for invalid in [&tools, &pool] {
            let slots = invalid.stats();
            assert!(module.instantiate_in_pool(invalid).is_err());
            assert!(
                process
                    .start_in_pool("Inc", &[99], 128, &cancellation, &preemption, invalid)
                    .is_err()
            );
            assert_eq!(vm.used(), full);
            assert_eq!(invalid.stats(), slots);
        }
        let slots = foreign.stats();
        assert!(
            process
                .start_in_pool("Inc", &[99], 128, &cancellation, &preemption, &foreign)
                .is_err()
        );
        assert_eq!(vm.used(), full);
        assert_eq!(foreign.stats(), slots);
        drop(fill);
        assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
    }
    let mut task = ScalarTask::new(process);
    assert!(
        task.start(
            &module.handler("Inc").unwrap(),
            &[99],
            128,
            &cancellation,
            &preemption,
            &tools
        )
        .is_err()
    );
    assert!(!task.is_active());
    let mut image = task.into_process().detach();
    for invalid in [&tools, &pool, &foreign] {
        assert!(image.attach(invalid).is_err());
        assert_eq!(vm.used(), baseline);
    }
    let mut process = image.attach(&bound).unwrap();
    assert!(image.attach(&bound).is_err());
    assert_eq!(process.invoke("Inc", &[2]).unwrap(), Some(9));
    drop((
        process, image, module, bound, pool, tools, foreign, other, account, tenant,
    ));
    assert_eq!(vm.used(), bootstrap);
}

fn verify_loading_order(directory: &Path) {
    let control = 2 << 20;
    for accounted in [false, true] {
        let bootstrap = if accounted {
            ResourceAccount::vm_accounted(limits(control))
                .unwrap()
                .used()
                .bytes
        } else {
            0
        };
        for remaining in [0, control - 1, control] {
            let limit = limits(bootstrap + remaining);
            let vm = if accounted {
                ResourceAccount::vm_accounted(limit).unwrap()
            } else {
                ResourceAccount::vm(limit)
            };
            let baseline = vm.used();
            for _ in 0..2 {
                for path in [Path::new(""), directory] {
                    let error = unsafe { ScalarModule::load_accounted(path, &vm) }
                        .err()
                        .unwrap();
                    assert_eq!(
                        error.contains("resource_exhausted at Vm account"),
                        remaining < control || path == directory
                    );
                    assert_eq!(vm.used(), baseline);
                }
            }
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() >= 6 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    verify_loading_order(directory);
    verify_execution_binding(directory);
    let file_bytes = std::fs::metadata(directory.join(wings_artifact::library_name()))
        .unwrap()
        .len()
        .div_ceil(4096)
        * 4096
        + (2 << 20);
    let bytes: u64 = arguments.last().unwrap().parse().unwrap();
    assert!(bytes >= file_bytes);
    let vm = ResourceAccount::vm(limits(32 << 20));
    let tenant = vm.tenant(limits(16 << 20)).unwrap();
    let account = tenant.process(limits(8 << 20)).unwrap();
    for invalid in [&tenant, &account] {
        assert!(unsafe { ScalarModule::load_accounted(directory, invalid) }.is_err());
        assert_eq!(vm.used(), Resources::default());
    }
    let small = ResourceAccount::vm(limits(bytes - 1));
    assert!(
        unsafe { ScalarModule::load_accounted(directory, &small) }
            .err()
            .unwrap()
            .contains("resource_exhausted at Vm account")
    );
    assert_eq!(small.used(), Resources::default());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    assert_eq!(
        vm.used(),
        Resources {
            bytes,
            ..Resources::default()
        }
    );
    assert_eq!(tenant.used(), Resources::default());
    let duplicate = module.clone();
    let handler = module.handler("Inc").unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut process = module.instantiate_in_pool(&pool).unwrap();
    let baseline = vm.used();
    drop((module, duplicate));
    assert_eq!(vm.used(), baseline);
    assert_eq!(process.invoke("Inc", &[7]).unwrap(), Some(7));
    drop((process, pool));
    assert_eq!(vm.used().bytes, bytes);
    drop(handler);
    assert_eq!(vm.used(), Resources::default());
    let exact = ResourceAccount::vm(limits(bytes));
    let first = unsafe { ScalarModule::load_accounted(directory, &exact) }.unwrap();
    assert!(unsafe { ScalarModule::load_accounted(directory, &exact) }.is_err());
    assert_eq!(exact.used().bytes, bytes);
    drop(first);
    assert_eq!(exact.used(), Resources::default());
    for _ in 0..32 {
        let pool = StackPool::new_accounted(&tenant, 1, 0)
            .unwrap()
            .for_process(&account)
            .unwrap();
        let pool_baseline = vm.used();
        let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
        let mut task = ScalarTask::new(module.instantiate_in_pool(&pool).unwrap());
        task.start(
            &module.handler("Inc").unwrap(),
            &[1],
            128,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool,
        )
        .unwrap();
        assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Suspended);
        drop(module);
        assert!(vm.used().bytes > bytes);
        drop(task);
        assert_eq!(vm.used(), pool_baseline);
        drop(pool);
        assert_eq!(vm.used(), Resources::default());
    }
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    let foreign_vm = ResourceAccount::vm(limits(16 << 20));
    let foreign_tenant = foreign_vm.tenant(limits(16 << 20)).unwrap();
    let foreign_account = foreign_tenant.process(limits(8 << 20)).unwrap();
    let mut foreign_worker = ScalarWorker::new(&foreign_tenant, 1).unwrap();
    let baseline = foreign_vm.used();
    assert!(
        foreign_worker
            .spawn(&module, limits(8 << 20), 1)
            .unwrap_err()
            .contains("different VM")
    );
    assert_eq!(foreign_vm.used(), baseline);
    let foreign_pool = StackPool::new_accounted(&foreign_tenant, 1, 0)
        .unwrap()
        .for_process(&foreign_account)
        .unwrap();
    let baseline = foreign_vm.used();
    assert!(module.instantiate_in_pool(&foreign_pool).is_err());
    assert!(
        module
            .instantiate_with_heap(&foreign_pool, &foreign_account, 2)
            .is_err()
    );
    assert_eq!(foreign_vm.used(), baseline);
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut process = module.instantiate_in_pool(&pool).unwrap();
    assert!(
        process
            .start_in_pool(
                "Inc",
                &[1],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
                &foreign_pool
            )
            .is_err()
    );
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
    drop((foreign_pool, foreign_worker));
    assert_eq!(foreign_vm.used(), Resources::default());
    drop((process, pool));
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    worker
        .request(
            pid,
            &module.handler("Inc").unwrap(),
            &[1],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    worker.tick(1).unwrap();
    drop(module);
    assert!(vm.used().bytes > bytes);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let mut process = module.instantiate_in_pool(&pool).unwrap();
    let baseline = vm.used();
    drop(module);
    let cancellation = CancellationToken::default();
    let mut execution = process
        .start_controlled("Inc", &[7], 128, &cancellation, &PreemptionToken::default())
        .unwrap();
    assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    assert_eq!(vm.used().bytes, baseline.bytes + 4352);
    cancellation.cancel();
    assert!(execution.resume(1).is_err());
    drop(execution);
    assert_eq!(vm.used(), baseline);
    drop((process, pool));
    assert_eq!(vm.used(), Resources::default());
    for artifact in &arguments[2..arguments.len() - 1] {
        assert!(unsafe { ScalarModule::load_accounted(Path::new(artifact), &vm) }.is_err());
        assert_eq!(vm.used(), Resources::default());
    }
    if arguments.len() > 6 {
        let error = unsafe { ScalarModule::load_accounted(Path::new(&arguments[5]), &vm) }
            .err()
            .unwrap();
        assert!(
            error.contains("module image span exceeds 64 MiB"),
            "{error}"
        );
        assert_eq!(vm.used(), Resources::default());
    }
    println!(
        "Module accounting verified: path/control/file/image admission order, exact capacity, failure rollback, shared leases, suspended cleanup, VM ownership and no unaccounted execution fallback"
    );
}
