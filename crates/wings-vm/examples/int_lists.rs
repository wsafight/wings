use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, PreemptionToken, RequestOutcome,
    ResourceAccount, Resources, ScalarModule, ScalarWorker, StackPool, WorkerEvent,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let limits = Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 4,
        requests: 4,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    assert!(module.instantiate_in_pool(&bound).is_err());
    let mut process = module.instantiate_with_heap(&bound, &account, 16).unwrap();
    assert_eq!(process.invoke("Empty", &[]).unwrap(), Some(1));
    for value in [7, 11, 19] {
        assert_eq!(process.invoke("Push", &[value]).unwrap(), Some(value));
    }
    assert_eq!(process.invoke("Shared", &[1]).unwrap(), Some(18));
    assert_eq!(process.invoke("Shared", &[0]).unwrap(), Some(106));
    for value in [19, 11, 7] {
        assert_eq!(process.invoke("Pop", &[]).unwrap(), Some(value));
    }
    assert_eq!(process.invoke("Empty", &[]).unwrap(), Some(1));
    assert_eq!(process.invoke("Build", &[7]).unwrap(), Some(7));
    let mut execution = process
        .start_controlled(
            "Drain",
            &[],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut done = false;
    for _ in 0..4096 {
        if let ExecutionStatus::Complete(reply) = execution.resume(1).unwrap() {
            assert_eq!(reply, Some(0));
            done = true;
            break;
        }
    }
    assert!(done);
    drop(execution);
    assert_eq!(process.invoke("Empty", &[]).unwrap(), Some(1));
    drop(process);
    assert_eq!(vm.used(), baseline);
    for (name, arguments, capacity, budget, fault) in [
        ("Head", vec![], 4, 128, ExecutionFault::EmptyList),
        ("Pop", vec![], 4, 128, ExecutionFault::EmptyList),
        ("Build", vec![30], 4, 4096, ExecutionFault::HeapExhausted),
        ("Build", vec![30], 64, 16, ExecutionFault::BudgetExhausted),
        ("Fail", vec![], 4, 128, ExecutionFault::DivisionByZero),
    ] {
        let mut process = module
            .instantiate_with_heap(&bound, &account, capacity)
            .unwrap();
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
    let mut process = module.instantiate_with_heap(&bound, &account, 64).unwrap();
    let cancellation = CancellationToken::default();
    let mut execution = process
        .start_controlled(
            "Build",
            &[30],
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
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn_with_heap(&module, limits, 1, 4).unwrap();
    assert_eq!(worker.snapshot(pid).unwrap().committed_state, None);
    assert!(worker.committed_state(pid).is_err());
    let request = worker
        .request(
            pid,
            &module.handler("Pop").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(matches!(
        worker.tick(128).unwrap(),
        WorkerEvent::Exited { .. }
    ));
    let exit = worker.last_exit().unwrap();
    assert_eq!(exit.committed_state, None);
    assert_eq!(exit.execution.fault, Some(ExecutionFault::EmptyList));
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Failed)
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "IntList verified: shared tails, state roots, loops, calls, empty failures and cleanup"
    );
}
