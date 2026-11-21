use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 8,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let account = tenant.process_accounted(limits()).unwrap();
    let control = vm
        .reserve(Resources {
            bytes: (2 << 20) + 4096,
            ..Resources::default()
        })
        .unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let mut process = module.instantiate_with_heap(&bound, &account, 16).unwrap();
    assert_eq!(process.invoke("build", &[3]).unwrap(), Some(3));
    let original_collections = process.heap_stats().unwrap().collections;
    let mut image = process.detach();
    drop((bound, pool));
    let headroom = vm.limit().bytes - vm.used().bytes;
    let blocked = vm
        .reserve(Resources {
            bytes: headroom,
            ..Resources::default()
        })
        .unwrap();
    assert!(StackPool::new_accounted(&tenant, 1, 0).is_err());
    drop(blocked);
    for expected in 4..=6 {
        let tenant = tenant.clone();
        let account = account.clone();
        image = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
                let bound = pool.for_process(&account).unwrap();
                let wrong_account = tenant.process_accounted(limits()).unwrap();
                let wrong = pool.for_process(&wrong_account).unwrap();
                let unaccounted = StackPool::new(1, 0).unwrap();
                let foreign_vm = ResourceAccount::vm(limits());
                let foreign_tenant = foreign_vm.tenant_accounted(limits()).unwrap();
                let foreign_pool = StackPool::new_accounted(&foreign_tenant, 1, 0).unwrap();
                let baseline = account.used();
                assert!(image.attach(&wrong).is_err());
                assert!(image.attach(&unaccounted).is_err());
                assert!(
                    image
                        .attach(&foreign_pool)
                        .err()
                        .unwrap()
                        .contains("different VM")
                );
                assert_eq!(account.used(), baseline);
                let mut process = image.attach(&bound).unwrap();
                assert!(image.attach(&bound).is_err());
                assert_eq!(process.invoke("head", &[]).unwrap(), Some(expected - 1));
                assert_eq!(process.invoke("push", &[expected]).unwrap(), Some(expected));
                assert!(process.heap_stats().unwrap().collections > original_collections);
                process.detach()
            })
            .unwrap()
            .join()
            .unwrap();
    }
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let mut process = image.attach(&bound).unwrap();
    assert_eq!(process.invoke("head", &[]).unwrap(), Some(6));
    let cancellation = CancellationToken::default();
    let mut execution = process
        .start_in_pool(
            "build",
            &[10],
            4096,
            &cancellation,
            &PreemptionToken::default(),
            &bound,
        )
        .unwrap();
    assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    cancellation.cancel();
    assert!(execution.resume(1).is_err());
    drop(execution);
    let mut failed = process.detach();
    let mut process = failed.attach(&bound).unwrap();
    assert!(
        process
            .invoke("head", &[])
            .unwrap_err()
            .contains("terminated")
    );
    drop(process);
    let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
    assert_eq!(process.invoke("push", &[9]).unwrap(), Some(9));
    let discarded = process.detach();
    drop((bound, pool, module, tenant, account));
    assert!(vm.used().bytes > control.amount().bytes);
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || drop(discarded))
        .unwrap()
        .join()
        .unwrap();
    drop(control);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Detached processes verified: exclusive heap handoff, same-account rebinding, precise roots, failed-state retention and final cleanup"
    );
}
