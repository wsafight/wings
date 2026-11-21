use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionPhase, ExecutionStatus, PreemptionToken,
    ResourceAccount, Resources, ScalarModule, StackPool,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    assert!(module.handler("fresh").is_err());
    let library = unsafe {
        libloading::Library::new(Path::new(&arguments[1]).join(wings_artifact::library_name()))
    }
    .unwrap();
    assert!(unsafe { library.get::<wings_abi::ScalarEntry>(b"wings_managed_function_0") }.is_err());
    drop(library);
    let limits = Resources {
        bytes: 16 << 20,
        ..Resources::default()
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
    assert_eq!(process.invoke("order", &[]).unwrap(), Some(12));
    assert_eq!(process.invoke("count", &[30]).unwrap(), Some(30));
    assert_eq!(process.invoke("choose", &[0]).unwrap(), Some(30));
    assert_eq!(process.invoke("choose", &[1]).unwrap(), Some(31));
    assert_eq!(process.invoke("once", &[]).unwrap(), Some(61));
    let mut execution = process
        .start_controlled(
            "count",
            &[60],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut done = false;
    for _ in 0..4096 {
        if let ExecutionStatus::Complete(reply) = execution.resume(1).unwrap() {
            assert_eq!(reply, Some(60));
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(execution.snapshot().phase, ExecutionPhase::Complete);
    drop(execution);
    assert_eq!(process.invoke("get", &[]).unwrap(), Some(60));
    drop(process);
    assert_eq!(vm.used(), baseline);
    for checkpoints in 0..48 {
        let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
        let mut execution = process
            .start_controlled(
                "order",
                &[],
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..checkpoints {
            if matches!(execution.resume(1).unwrap(), ExecutionStatus::Complete(_)) {
                break;
            }
        }
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    for (name, arguments, budget, fault) in [
        ("count", vec![30], 16, ExecutionFault::BudgetExhausted),
        ("fail", vec![], 128, ExecutionFault::DivisionByZero),
        ("first_failure", vec![], 128, ExecutionFault::DivisionByZero),
        ("second_failure", vec![], 128, ExecutionFault::Overflow),
    ] {
        let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
        let mut execution = process
            .start_controlled(
                name,
                &arguments,
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(execution.resume(4096).is_err());
        assert_eq!(execution.snapshot().fault, Some(fault));
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
    let cancellation = CancellationToken::default();
    let mut execution = process
        .start_controlled(
            "count",
            &[100],
            4096,
            &cancellation,
            &PreemptionToken::default(),
        )
        .unwrap();
    for _ in 0..50 {
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    }
    cancellation.cancel();
    assert!(execution.resume(1).is_err());
    assert_eq!(execution.snapshot().fault, Some(ExecutionFault::Cancelled));
    drop(execution);
    drop(process);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Managed functions verified: argument order, cross-frame roots, loop calls, slices and failures"
    );
}
