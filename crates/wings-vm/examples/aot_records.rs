use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    ScalarProcess, StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 8 << 20,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn process(
    module: &ScalarModule,
    pool: &StackPool,
    account: &ResourceAccount,
    capacity: usize,
) -> ScalarProcess {
    module
        .instantiate_with_heap(pool, account, capacity)
        .expect("accounted heap instance")
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    let module = unsafe { ScalarModule::load_trusted(&directory.join("records")) }
        .expect("trusted record fixture");
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).expect("tenant");
    let account = tenant.process(limits()).expect("process account");
    let pool = StackPool::new_accounted(&tenant, 2, 0).expect("pool");
    let bound = pool.for_process(&account).expect("bound pool");
    let baseline = vm.used();
    for (name, capacity, expected) in [
        ("Live", 2, 22),
        ("Reuse", 1, 11),
        ("Many", 64, 715),
        ("Integer", 1, 1_i64 << 32),
    ] {
        let mut actor = process(&module, &bound, &account, capacity);
        for _ in 0..3 {
            assert_eq!(actor.invoke(name, &[]), Ok(Some(expected)));
            assert_eq!(actor.heap_stats().expect("heap stats").allocated, 0);
            assert!(actor.heap_stats().expect("heap stats").collections > 0);
        }
        let mut task = actor
            .start_in_pool(
                name,
                &[],
                4096,
                &CancellationToken::default(),
                &PreemptionToken::default(),
                &bound,
            )
            .expect("start");
        let mut completed = false;
        for _ in 0..1024 {
            match task.resume(1).expect("resume") {
                ExecutionStatus::Suspended => {}
                ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
                ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
                ExecutionStatus::CompleteString(value) => {
                    panic!("unexpected string reply: {value:?}")
                }
                ExecutionStatus::Complete(reply) => {
                    assert_eq!(reply, Some(expected));
                    completed = true;
                    break;
                }
            }
        }
        assert!(completed);
        drop(task);
        assert_eq!(actor.heap_stats().expect("heap returned").allocated, 0);
        drop(actor);
        assert_eq!(vm.used(), baseline);
    }
    for (name, capacity, error) in [
        ("Full", 2, "heap exhausted"),
        ("Fail", 2, "division by zero"),
    ] {
        let mut actor = process(&module, &bound, &account, capacity);
        assert!(actor.invoke(name, &[]).unwrap_err().contains(error));
        assert!(actor.invoke("Get", &[]).unwrap_err().contains("terminated"));
        drop(actor);
        assert_eq!(vm.used(), baseline);
    }
    let mut actor = process(&module, &bound, &account, 2);
    let mut sibling = process(&module, &bound, &account, 2);
    let mut task = actor
        .start_in_pool(
            "Live",
            &[],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &bound,
        )
        .expect("first task");
    let mut other = sibling
        .start_in_pool(
            "Live",
            &[],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &bound,
        )
        .expect("second task");
    let mut completed_steps = None;
    for steps in 1..1024 {
        let result = task.resume(1).unwrap();
        assert_eq!(other.resume(1).unwrap(), result);
        if result == ExecutionStatus::Complete(Some(22)) {
            completed_steps = Some(steps);
            break;
        }
    }
    let completed_steps = completed_steps.expect("bounded GC completion");
    assert!(completed_steps > 13);
    drop((task, other));
    drop((actor, sibling));
    assert_eq!(vm.used(), baseline);
    for budget in 0..completed_steps {
        let mut actor = process(&module, &bound, &account, 2);
        assert!(
            actor
                .invoke_controlled("Live", &[], budget, &CancellationToken::default())
                .unwrap_err()
                .contains("budget")
        );
        drop(actor);
        assert_eq!(vm.used(), baseline);
    }
    for abandoned in [false, true] {
        let mut actor = process(&module, &bound, &account, 2);
        let cancellation = CancellationToken::default();
        let mut task = actor
            .start_in_pool(
                "Live",
                &[],
                4096,
                &cancellation,
                &PreemptionToken::default(),
                &bound,
            )
            .expect("start");
        for _ in 0..3 {
            assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        }
        if !abandoned {
            cancellation.cancel();
            assert!(task.resume(1).unwrap_err().contains("cancelled"));
        }
        drop(task);
        drop(actor);
        assert_eq!(vm.used(), baseline);
    }
    let initializer = unsafe { ScalarModule::load_trusted(&directory.join("initializer")) }
        .expect("record initializer");
    assert!(initializer.instantiate().is_err());
    let mut actor = process(&initializer, &bound, &account, 1);
    assert_eq!(actor.invoke("Get", &[]), Ok(Some(7)));
    drop(actor);
    let mut no_heap = module.instantiate().expect("scalar initializer");
    assert!(
        no_heap
            .invoke("Live", &[])
            .unwrap_err()
            .contains("callback")
    );
    drop(no_heap);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "AOT roots verified: live/dead roots, bounded reuse, initializer, suspension, failures and accounted cleanup"
    );
}
