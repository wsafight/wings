use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        ..Resources::default()
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let account = tenant.process(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    for (flag, expected) in [(1, 18), (0, 29), (1, 18), (0, 29)] {
        let mut execution = process
            .start_controlled(
                "Heap",
                &[flag],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        let mut completed = false;
        for _ in 0..128 {
            match execution.resume(1).unwrap() {
                ExecutionStatus::Suspended => {}
                ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
                ExecutionStatus::Complete(reply) => {
                    assert_eq!(reply, Some(expected));
                    completed = true;
                    break;
                }
            }
        }
        assert!(completed);
        drop(execution);
        assert_eq!(process.heap_stats().unwrap().allocated, 1);
    }
    let cancellation = CancellationToken::default();
    let mut execution = process
        .start_controlled(
            "Heap",
            &[0],
            128,
            &cancellation,
            &PreemptionToken::default(),
        )
        .unwrap();
    for _ in 0..6 {
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    }
    cancellation.cancel();
    assert!(execution.resume(1).unwrap_err().contains("cancelled"));
    drop(execution);
    drop(process);
    assert_eq!(vm.used(), baseline);
    for flag in [0, 1] {
        let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
        let mut execution = process
            .start_controlled(
                "Heap",
                &[flag],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        for _ in 0..6 {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        drop(execution);
        assert!(
            process
                .invoke("Guard", &[1])
                .unwrap_err()
                .contains("terminated")
        );
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Conditional execution verified: chosen paths, precise heap roots, suspension, cancellation and cleanup"
    );
}
