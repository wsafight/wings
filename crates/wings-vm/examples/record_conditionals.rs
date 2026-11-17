use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionPhase, ExecutionStatus, PreemptionToken, ResourceAccount,
    Resources, ScalarModule, ScalarTask, StackPool,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
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
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
    assert_eq!(process.invoke("Replace", &[0, 99]).unwrap(), Some(7));
    assert_eq!(process.heap_stats().unwrap().allocated, 1);
    assert_eq!(process.invoke("Replace", &[1, 11]).unwrap(), Some(11));
    assert_eq!(process.invoke("Nested", &[1, 0, 19]).unwrap(), Some(11));
    assert_eq!(process.invoke("Nested", &[0, 1, 19]).unwrap(), Some(11));
    assert_eq!(process.invoke("Nested", &[0, 0, 19]).unwrap(), Some(20));
    assert_eq!(process.invoke("Local", &[0, 23]).unwrap(), Some(44));
    assert_eq!(process.invoke("Discard", &[1]).unwrap(), Some(20));
    drop(process);
    assert_eq!(vm.used(), baseline);
    let mut task = ScalarTask::new(module.instantiate_with_heap(&bound, &account, 2).unwrap());
    let handler = module.handler("Nested").unwrap();
    let mut expected = 7;
    for (first, second) in [(1, 1), (1, 0), (0, 1), (0, 0)]
        .into_iter()
        .cycle()
        .take(40)
    {
        task.start(
            &handler,
            &[first, second, 19],
            128,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &bound,
        )
        .unwrap();
        let mut done = false;
        for _ in 0..128 {
            match task.resume(1).unwrap() {
                ExecutionStatus::Suspended => {
                    assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Suspended);
                }
                ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
                ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
                ExecutionStatus::CompleteString(value) => {
                    panic!("unexpected string reply: {value:?}")
                }
                ExecutionStatus::Complete(reply) => {
                    expected = if first == 1 && second == 1 {
                        19
                    } else if first == 0 && second == 0 {
                        20
                    } else {
                        expected
                    };
                    assert_eq!(reply, Some(expected));
                    done = true;
                    break;
                }
            }
        }
        assert!(done);
    }
    assert!(task.heap_stats().unwrap().collections >= 40);
    drop(task);
    assert_eq!(vm.used(), baseline);
    for (name, values) in [("Replace", vec![1, 99]), ("Fail", vec![1])] {
        for budget in 0..40 {
            let mut task =
                ScalarTask::new(module.instantiate_with_heap(&bound, &account, 2).unwrap());
            task.start(
                &module.handler(name).unwrap(),
                &values,
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
                &bound,
            )
            .unwrap();
            let result = task.resume(128);
            let snapshot = task.snapshot().unwrap();
            let mut process = task.into_process();
            match result {
                Err(_) => {
                    assert_eq!(snapshot.phase, ExecutionPhase::Failed);
                    assert!(process.invoke("Get", &[]).is_err());
                }
                Ok(status) => {
                    assert_ne!(name, "Fail");
                    assert_eq!(status, ExecutionStatus::Complete(Some(99)));
                    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(99));
                }
            }
            drop(process);
            assert_eq!(vm.used(), baseline);
        }
    }
    for slice in 1..24 {
        let mut task = ScalarTask::new(module.instantiate_with_heap(&bound, &account, 2).unwrap());
        let cancellation = CancellationToken::default();
        task.start(
            &handler,
            &[0, 0, 99],
            128,
            &cancellation,
            &PreemptionToken::default(),
            &bound,
        )
        .unwrap();
        if task.resume(slice).unwrap() == ExecutionStatus::Suspended {
            cancellation.cancel();
            assert!(task.resume(128).unwrap_err().contains("cancelled"));
            assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Failed);
        }
        drop(task);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Record conditionals verified: lazy merges, persistent roots, slices, failure and cleanup"
    );
}
