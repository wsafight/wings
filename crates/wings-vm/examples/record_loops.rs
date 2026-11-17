use std::path::Path;
use std::time::Duration;

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionPhase, ExecutionStatus, PreemptionToken,
    ProcessPhase, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarTask,
    ScalarWorker, StackPool, WorkerEvent,
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
    let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
    assert_eq!(process.invoke("Count", &[0]).unwrap(), Some(0));
    assert_eq!(process.invoke("Count", &[30]).unwrap(), Some(30));
    assert!(process.heap_stats().unwrap().collections >= 30);
    assert_eq!(process.invoke("Advance", &[12, 8]).unwrap(), Some(12));
    assert_eq!(process.invoke("Advance", &[5, 9]).unwrap(), Some(9));
    assert_eq!(process.invoke("Preserve", &[0, 10]).unwrap(), Some(10));
    assert_eq!(process.invoke("Nested", &[15]).unwrap(), Some(27));
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(17));
    assert_eq!(process.invoke("Retain", &[20]).unwrap(), Some(20));
    drop(process);
    assert_eq!(vm.used(), baseline);
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    assert_eq!(process.invoke("Count", &[30]).unwrap(), Some(30));
    drop(process);
    let mut process = module.instantiate_with_heap(&bound, &account, 2).unwrap();
    assert!(
        process
            .invoke("Retain", &[30])
            .unwrap_err()
            .contains("heap exhausted")
    );
    drop(process);
    assert_eq!(vm.used(), baseline);
    for cancel in [false, true] {
        let mut task = ScalarTask::new(module.instantiate_with_heap(&bound, &account, 3).unwrap());
        let cancellation = CancellationToken::default();
        task.start(
            &module.handler("Count").unwrap(),
            &[30],
            2048,
            &cancellation,
            &PreemptionToken::default(),
            &bound,
        )
        .unwrap();
        let mut done = false;
        for slice in 0..2048 {
            match task.resume(1).unwrap() {
                ExecutionStatus::Suspended => {
                    assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Suspended);
                    if cancel && slice == 40 {
                        cancellation.cancel();
                        assert!(task.resume(1).unwrap_err().contains("cancelled"));
                        assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Failed);
                        break;
                    }
                }
                ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
                ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
                ExecutionStatus::CompleteString(value) => {
                    panic!("unexpected string reply: {value:?}")
                }
                ExecutionStatus::CompleteRecord(value) => {
                    panic!("unexpected record reply: {value:?}")
                }
                ExecutionStatus::Complete(reply) => {
                    assert!(!cancel);
                    assert_eq!(reply, Some(30));
                    done = true;
                    break;
                }
            }
        }
        assert_eq!(done, !cancel);
        drop(task);
        assert_eq!(vm.used(), baseline);
    }
    for budget in [1, 32, 256] {
        let mut process = module.instantiate_with_heap(&bound, &account, 3).unwrap();
        let mut execution = process
            .start_controlled(
                "Preserve",
                &[1, 19],
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(
            execution
                .resume(1024)
                .unwrap_err()
                .contains("step budget exhausted")
        );
        assert_eq!(execution.snapshot().remaining_steps, 0);
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn_with_heap(&module, limits, 1, 3).unwrap();
    let pending = worker
        .request(
            pid,
            &module.handler("Nested").unwrap(),
            &[15],
            4096,
            Duration::from_secs(60),
        )
        .unwrap();
    let mut done = false;
    for _ in 0..4096 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(_) => {
                let snapshot = worker.snapshot(pid).unwrap();
                assert_eq!(snapshot.phase, ProcessPhase::Suspended);
                assert_eq!(snapshot.committed_state, None);
                assert!(snapshot.heap.unwrap().allocated <= 3);
            }
            WorkerEvent::Delivered(_) => {
                done = true;
                break;
            }
            event => panic!("unexpected event {event:?}"),
        }
    }
    assert!(done);
    assert_eq!(
        worker.take_reply(pending).unwrap(),
        Some(RequestOutcome::Reply(17))
    );
    let failed = worker
        .request(
            pid,
            &module.handler("Fail").unwrap(),
            &[20],
            4096,
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        matches!(worker.tick(4096).unwrap(), WorkerEvent::Exited { reason, .. } if reason.contains("division by zero"))
    );
    assert!(worker.snapshot(pid).is_err());
    let exit = *worker.last_exit().unwrap();
    assert_eq!(exit.handler(), "Fail");
    assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    assert_eq!(exit.committed_state, None);
    assert!(exit.execution.heap.unwrap().collections > 0);
    assert_eq!(
        worker.take_reply(failed).unwrap(),
        Some(RequestOutcome::Failed)
    );
    for (capacity, budget, fault) in [
        (3, 16, ExecutionFault::BudgetExhausted),
        (1, 4096, ExecutionFault::HeapExhausted),
    ] {
        let pid = worker
            .spawn_with_heap(&module, limits, 1, capacity)
            .unwrap();
        let request = worker
            .request(
                pid,
                &module.handler("Count").unwrap(),
                &[30],
                budget,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(matches!(
            worker.tick(4096).unwrap(),
            WorkerEvent::Exited { .. }
        ));
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, pid);
        assert_eq!(exit.handler(), "Count");
        assert_eq!(exit.execution.fault, Some(fault));
        assert_eq!(exit.execution.heap.unwrap().capacity, capacity);
        assert_eq!(exit.committed_state, None);
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
    }
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Record loops verified: zero/multiple/nested iterations, roots, slices, failure and accounting"
    );
}
