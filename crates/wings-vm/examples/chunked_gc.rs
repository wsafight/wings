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
        messages: 8,
        requests: 8,
    };
    let vm = ResourceAccount::vm(limits);
    let tenant = vm.tenant(limits).unwrap();
    let account = tenant.process(limits).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut process = module
        .instantiate_with_heap(&bound, &account, 1024)
        .unwrap();
    let mut execution = process
        .start_controlled(
            "Sweep",
            &[],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut suspended = 0;
    loop {
        assert!(suspended < 1024);
        match execution.resume(1).unwrap() {
            ExecutionStatus::Suspended => suspended += 1,
            ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
            ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
            ExecutionStatus::CompleteString(value) => panic!("unexpected string reply: {value:?}"),
            ExecutionStatus::Complete(reply) => {
                assert_eq!(reply, Some(19));
                break;
            }
        }
    }
    assert!(suspended > 256);
    assert_eq!(execution.snapshot().heap.unwrap().collections, 2);
    drop(execution);
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(8));
    drop(process);
    assert_eq!(vm.used(), baseline);
    for checkpoints in [1, 16, 64, 80, 128, 160, 256] {
        for cancel in [false, true] {
            let mut process = module
                .instantiate_with_heap(&bound, &account, 1024)
                .unwrap();
            let cancellation = CancellationToken::default();
            let mut execution = process
                .start_controlled(
                    "Sweep",
                    &[],
                    4096,
                    &cancellation,
                    &PreemptionToken::default(),
                )
                .unwrap();
            for _ in 0..checkpoints {
                assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
            }
            if cancel {
                cancellation.cancel();
                assert!(execution.resume(1).is_err());
                assert_eq!(execution.snapshot().fault, Some(ExecutionFault::Cancelled));
            }
            drop(execution);
            drop(process);
            assert_eq!(vm.used(), baseline);
        }
    }
    for budget in [16, 80, 160, 256] {
        let mut process = module
            .instantiate_with_heap(&bound, &account, 1024)
            .unwrap();
        let mut execution = process
            .start_controlled(
                "Sweep",
                &[],
                budget,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert!(execution.resume(4096).is_err());
        assert_eq!(
            execution.snapshot().fault,
            Some(ExecutionFault::BudgetExhausted)
        );
        assert_eq!(execution.snapshot().remaining_steps, 0);
        drop(execution);
        drop(process);
        assert_eq!(vm.used(), baseline);
    }
    let mut process = module.instantiate_with_heap(&bound, &account, 32).unwrap();
    let mut execution = process
        .start_controlled(
            "Allocate",
            &[100],
            4096,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let mut complete = false;
    for _ in 0..4096 {
        if let ExecutionStatus::Complete(reply) = execution.resume(1).unwrap() {
            assert_eq!(reply, Some(100));
            complete = true;
            break;
        }
    }
    assert!(complete && execution.snapshot().heap.unwrap().collections > 0);
    drop(execution);
    drop(process);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
    let hot = worker.spawn_with_heap(&module, limits, 1, 1024).unwrap();
    let cold = worker.spawn_with_heap(&module, limits, 1, 2).unwrap();
    let timeout = Duration::from_secs(60);
    let sweep = worker
        .request(hot, &module.handler("Sweep").unwrap(), &[], 4096, timeout)
        .unwrap();
    let heartbeat = worker
        .request(cold, &module.handler("Get").unwrap(), &[], 128, timeout)
        .unwrap();
    let mut heart_done = false;
    let mut sweep_done = false;
    for _ in 0..16 {
        match worker.tick(16).unwrap() {
            WorkerEvent::Delivered(pid) if pid == cold => heart_done = true,
            WorkerEvent::Delivered(pid) if pid == hot => sweep_done = true,
            WorkerEvent::Suspended(_) | WorkerEvent::Idle => {}
            event => panic!("unexpected GC event {event:?}"),
        }
    }
    assert!(heart_done && !sweep_done);
    assert_eq!(
        worker.take_reply(heartbeat).unwrap(),
        Some(RequestOutcome::Reply(7))
    );
    assert!(
        worker
            .snapshot(hot)
            .unwrap()
            .execution
            .unwrap()
            .heap
            .unwrap()
            .collections
            < 2
    );
    for _ in 0..64 {
        if worker.tick(16).unwrap() == WorkerEvent::Delivered(hot) {
            sweep_done = true;
            break;
        }
    }
    assert!(sweep_done);
    assert_eq!(
        worker.take_reply(sweep).unwrap(),
        Some(RequestOutcome::Reply(19))
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Chunked GC verified: bounded work, nested roots, pause/cancel/budget, allocation retry, fairness and cleanup"
    );
}
