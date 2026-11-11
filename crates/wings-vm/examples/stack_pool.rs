use std::path::Path;

use wings_vm::{CancellationToken, ExecutionStatus, PreemptionToken, ScalarProcess, StackPool};

fn load(directory: &Path) -> ScalarProcess {
    unsafe { ScalarProcess::load_trusted(directory) }.expect("trusted Counter artifact")
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    assert!(StackPool::new(0, 0).is_err());
    assert!(StackPool::new(2049, 0).is_err());
    assert!(StackPool::new(1, 2).is_err());
    let pool = StackPool::new(2, 1).expect("pool");
    let initial = pool.stats();
    assert_eq!(
        (
            initial.capacity,
            initial.active,
            initial.cached,
            initial.available
        ),
        (2, 0, 0, 2)
    );
    assert_eq!(initial.usable_bytes_per_stack, 256 * 1024);
    assert!(initial.guard_bytes_per_stack.is_power_of_two());
    assert_eq!(
        initial.reserved_bytes,
        2 * (initial.usable_bytes_per_stack + initial.guard_bytes_per_stack)
    );
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    let mut first = load(directory);
    let mut second = load(directory);
    let mut third = load(directory);
    {
        let mut first_execution = first
            .start_in_pool("Inc", &[3], 4, &cancellation, &preemption, &pool)
            .expect("first slot");
        assert_eq!(first_execution.resume(1), Ok(ExecutionStatus::Suspended));
        let mut second_execution = second
            .start_in_pool("Inc", &[7], 4, &cancellation, &preemption, &pool)
            .expect("second slot");
        assert_eq!(second_execution.resume(1), Ok(ExecutionStatus::Suspended));
        assert_eq!(pool.stats().active, 2);
        assert!(
            third
                .start_in_pool("Inc", &[11], 4, &cancellation, &preemption, &pool)
                .is_err()
        );
        assert_eq!(third.invoke("Get", &[]), Ok(Some(0)));
        pool.trim().expect("trim with active stacks");
        assert_eq!(pool.stats().active, 2);
        assert_eq!(
            first_execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(3)))
        );
        assert_eq!(
            (
                pool.stats().active,
                pool.stats().cached,
                pool.stats().available
            ),
            (1, 1, 1)
        );
        {
            let mut replacement = third
                .start_in_pool("Inc", &[11], 4, &cancellation, &preemption, &pool)
                .expect("reuse completed slot before dropping handle");
            assert_eq!(
                replacement.resume(100),
                Ok(ExecutionStatus::Complete(Some(11)))
            );
        }
        assert_eq!(
            second_execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(7)))
        );
    }
    assert_eq!((pool.stats().active, pool.stats().cached), (0, 1));
    pool.trim().expect("release cached pages");
    assert_eq!(
        (
            pool.stats().active,
            pool.stats().cached,
            pool.stats().available
        ),
        (0, 0, 2)
    );
    for max_cached in [0, 1] {
        let pool = StackPool::new(1, max_cached).expect("single slot pool");
        let mut process = load(directory);
        for expected in 1..=256 {
            let mut execution = process
                .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &pool)
                .expect("reuse");
            assert_eq!(
                execution.resume(4),
                Ok(ExecutionStatus::Complete(Some(expected)))
            );
            assert_eq!((pool.stats().active, pool.stats().cached), (0, max_cached));
        }
        for started in [false, true] {
            let mut abandoned = load(directory);
            {
                let mut execution = abandoned
                    .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &pool)
                    .expect("abandoned");
                if started {
                    assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
                }
            }
            assert!(
                abandoned
                    .invoke("Get", &[])
                    .unwrap_err()
                    .contains("terminated")
            );
            assert_eq!(pool.stats().active, 0);
        }
        let mut process = load(directory);
        let cancellation = CancellationToken::default();
        {
            let mut execution = process
                .start_in_pool("Inc", &[1], 4, &cancellation, &preemption, &pool)
                .expect("cancelled slot");
            assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
            cancellation.cancel();
            assert!(execution.resume(1).unwrap_err().contains("cancelled"));
            assert_eq!(pool.stats().active, 0);
        }
        assert_eq!(pool.stats().reset_failures, 0);
        pool.trim().expect("trim");
    }
    let mut survivor = load(directory);
    {
        let pool = StackPool::new(1, 0).expect("owned mapping");
        let mut execution = survivor
            .start_in_pool("Inc", &[5], 4, &cancellation, &preemption, &pool)
            .expect("start");
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        drop(pool);
        assert_eq!(
            execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(5)))
        );
    }
    assert_eq!(survivor.invoke("Get", &[]), Ok(Some(5)));
    assert_eq!(pool.stats().reset_failures, 0);
    println!(
        "Stack pool verified: admission, exclusive slots, reuse, immediate release, cancellation, trim and lifetime"
    );
}
