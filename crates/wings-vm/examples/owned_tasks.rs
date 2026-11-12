use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ScalarModule, ScalarTask, StackPool,
};

fn load(directory: &Path) -> ScalarModule {
    unsafe { ScalarModule::load_trusted(directory) }.expect("trusted scalar artifact")
}

fn finish(task: &mut ScalarTask, slice: u64) -> Option<i64> {
    for _ in 0..4096 {
        if let ExecutionStatus::Complete(reply) = task.resume(slice).expect("resume") {
            assert!(!task.is_active());
            return reply;
        }
    }
    panic!("task did not complete within bounded resumes");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let module = load(Path::new(&arguments[1]));
    let work_module = load(Path::new(&arguments[2]));
    let increment = module.handler("Inc").expect("handler");
    let get = module.handler("Get").expect("handler");
    let work = work_module.handler("Work").expect("handler");
    let fail = work_module.handler("Fail").expect("handler");
    let pool = StackPool::new(8, 0).expect("pool");
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    let mut task = ScalarTask::new(module.instantiate().expect("instance"));
    assert!(!task.is_active());
    assert!(task.resume(1).is_err());
    assert!(std::ptr::eq(task.module().manifest(), module.manifest()));
    for (handler, values) in [(&increment, &[][..]), (&work, &[1][..])] {
        assert!(
            task.start(handler, values, 4, &cancellation, &preemption, &pool)
                .is_err()
        );
        assert!(!task.is_active());
        assert_eq!(pool.stats().active, 0);
    }
    let mut values = [3];
    task.start(&increment, &values, 4, &cancellation, &preemption, &pool)
        .expect("start");
    values[0] = 99;
    assert!(
        task.start(&get, &[], 2, &cancellation, &preemption, &pool)
            .unwrap_err()
            .contains("active")
    );
    assert!(task.resume(0).unwrap_err().contains("at least one"));
    assert!(task.is_active());
    assert_eq!(pool.stats().active, 1);
    assert_eq!(finish(&mut task, 1), Some(3));
    assert_eq!(values, [99]);
    assert_eq!(pool.stats().active, 0);
    assert!(task.resume(1).is_err());
    task.start(&get, &[], 2, &cancellation, &preemption, &pool)
        .expect("next call");
    assert_eq!(finish(&mut task, 1), Some(3));
    let mut process = task.into_process();
    assert_eq!(process.invoke("Get", &[]), Ok(Some(3)));

    let mut tasks = Vec::new();
    for seed in 0..8 {
        let mut task = ScalarTask::new(work_module.instantiate().expect("instance"));
        task.start(&work, &[seed], 147, &cancellation, &preemption, &pool)
            .expect("start work");
        assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        tasks.push((task, 48 * seed + 1128));
    }
    assert_eq!(pool.stats().active, 8);
    let mut waiting = ScalarTask::new(module.instantiate().expect("waiting instance"));
    assert!(
        waiting
            .start(&increment, &[1], 4, &cancellation, &preemption, &pool)
            .unwrap_err()
            .contains("exhausted")
    );
    assert!(!waiting.is_active());
    for _ in 0..145 {
        tasks.rotate_left(1);
        for (task, _) in &mut tasks {
            assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        }
    }
    tasks.reverse();
    for (mut task, expected) in tasks {
        assert_eq!(
            task.resume(1),
            Ok(ExecutionStatus::Complete(Some(expected)))
        );
        let mut process = task.into_process();
        assert_eq!(process.invoke("Get", &[]), Ok(Some(expected)));
    }
    assert_eq!(pool.stats().active, 0);
    waiting
        .start(&increment, &[1], 4, &cancellation, &preemption, &pool)
        .expect("pool recovery");
    assert_eq!(finish(&mut waiting, 1), Some(1));

    for started in [false, true] {
        let mut task = ScalarTask::new(module.instantiate().expect("instance"));
        task.start(&increment, &[3], 4, &cancellation, &preemption, &pool)
            .expect("start abandoned");
        if started {
            assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        }
        let mut process = task.into_process();
        assert_eq!(pool.stats().active, 0);
        assert!(
            process
                .invoke("Get", &[])
                .unwrap_err()
                .contains("terminated")
        );
        let mut task = ScalarTask::new(module.instantiate().expect("instance"));
        task.start(&increment, &[3], 4, &cancellation, &preemption, &pool)
            .expect("start dropped");
        if started {
            assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        }
        drop(task);
        assert_eq!(pool.stats().active, 0);
    }

    let mut task = ScalarTask::new(module.instantiate().expect("instance"));
    task.start(&increment, &[1], 4, &cancellation, &preemption, &pool)
        .expect("start preempted");
    let remote = preemption.clone();
    std::thread::spawn(move || remote.request())
        .join()
        .expect("preemption thread");
    assert_eq!(task.resume(100), Ok(ExecutionStatus::Suspended));
    assert_eq!(finish(&mut task, 100), Some(1));

    for cancel in [false, true] {
        let cancellation = CancellationToken::default();
        let mut task = ScalarTask::new(module.instantiate().expect("instance"));
        let budget = if cancel { 4 } else { 3 };
        task.start(&increment, &[1], budget, &cancellation, &preemption, &pool)
            .expect("start failure case");
        assert_eq!(task.resume(1), Ok(ExecutionStatus::Suspended));
        if cancel {
            let remote = cancellation.clone();
            std::thread::spawn(move || remote.cancel())
                .join()
                .expect("cancellation thread");
        }
        let error = task.resume(100).unwrap_err();
        assert!(error.contains(if cancel {
            "cancelled"
        } else {
            "budget exhausted"
        }));
        assert!(!task.is_active());
        assert_eq!(pool.stats().active, 0);
        assert!(
            task.start(&get, &[], 2, &cancellation, &preemption, &pool)
                .unwrap_err()
                .contains("terminated")
        );
    }

    let mut task = ScalarTask::new(work_module.instantiate().expect("instance"));
    task.start(&fail, &[], 100, &cancellation, &preemption, &pool)
        .expect("start arithmetic failure");
    assert!(task.resume(100).unwrap_err().contains("division by zero"));
    assert!(!task.is_active());
    assert_eq!(pool.stats().active, 0);
    let mut process = task.into_process();
    assert!(
        process
            .invoke("Get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    println!(
        "Owned tasks verified: movable live stacks, exclusive calls, exact budgets and normal cleanup"
    );
}
