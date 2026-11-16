use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use serde_json::json;
use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let deadline = Instant::now() + Duration::from_secs(3);
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let mut samples = Vec::with_capacity(2);
    for remote in [false, true] {
        assert!(Instant::now() < deadline);
        let token = PreemptionToken::new_observed(&account).unwrap();
        let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
        let mut execution = process
            .start_controlled(
                "Cpu",
                &[1, 2, 0, 0],
                10_000_000,
                &CancellationToken::default(),
                &token,
            )
            .unwrap();
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        assert_eq!(token.observation().unwrap().last_yield, None);
        let consumed = execution.snapshot().consumed_steps();
        let sender = if remote {
            let barrier = Arc::new(Barrier::new(2));
            let ready = barrier.clone();
            let request = token.clone();
            let sender = std::thread::spawn(move || {
                ready.wait();
                request.request();
            });
            barrier.wait();
            Some(sender)
        } else {
            token.request();
            None
        };
        let outcome = execution.resume(u64::MAX).unwrap();
        let returned = Instant::now();
        if let Some(sender) = sender {
            sender.join().unwrap();
        }
        let observation = token.observation().unwrap();
        match outcome {
            ExecutionStatus::Suspended => {
                let observed = observation.last_yield.expect("atomic request consumed");
                assert!(observed.requested_at <= observed.yielded_at);
                assert!(observed.yielded_at <= returned);
                assert_eq!(observation.pending_since, None);
                samples.push(json!({"remote": remote, "observed": true,
                    "latency_ns": observed.latency().as_nanos() as u64,
                    "steps_before_resume": consumed,
                    "steps_at_yield": execution.snapshot().consumed_steps()}));
                assert_eq!(
                    execution.resume(u64::MAX),
                    Ok(ExecutionStatus::Complete(Some(1_000_000)))
                );
            }
            ExecutionStatus::CompleteList(value) => panic!("unexpected list reply: {value:?}"),
            ExecutionStatus::CompleteBytes(value) => panic!("unexpected byte reply: {value:?}"),
            ExecutionStatus::Complete(reply) => {
                assert!(remote);
                assert_eq!(reply, Some(1_000_000));
                assert_eq!(observation.last_yield, None);
                assert!(observation.pending_since.is_some());
                samples.push(
                    json!({"remote": true, "observed": false, "latency_ns": null,
                    "reason": "request_not_consumed_before_completion"}),
                );
            }
        }
        drop(execution);
        assert_eq!(process.invoke("Pulse", &[1, 1, 1, 0]), Ok(Some(2)));
        drop((process, token));
        assert_eq!(vm.used(), baseline);
        assert!(Instant::now() < deadline);
    }
    drop((bound, pool, account, tenant, module));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "{}",
        json!({"version": 1, "status": "ok", "samples": samples,
        "states_verified": true, "resources_released": true, "fairness_gate_proven": false})
    );
}
