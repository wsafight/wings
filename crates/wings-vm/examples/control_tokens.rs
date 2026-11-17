use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    StackPool,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 1,
        messages: 0,
        requests: 0,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let started = Instant::now();
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let account = tenant.process_accounted(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let baseline = account.used();
    for mode in 0..4 {
        assert!(started.elapsed() < Duration::from_secs(3));
        let mut process = module.instantiate_in_pool(&pool).unwrap();
        let cancellation = CancellationToken::new_accounted(&account).unwrap();
        let preemption = PreemptionToken::new_accounted(&account).unwrap();
        let tokens = account.used();
        assert!(tokens.bytes > baseline.bytes);
        let retained = cancellation.clone();
        let mut execution = process
            .start_controlled("Inc", &[7], 128, &cancellation, &preemption)
            .unwrap();
        if mode == 1 {
            preemption.request();
            assert_eq!(execution.resume(128).unwrap(), ExecutionStatus::Suspended);
        } else {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        drop((cancellation, preemption));
        if mode == 2 {
            std::thread::spawn(move || retained.cancel())
                .join()
                .unwrap();
            assert!(execution.resume(128).unwrap_err().contains("cancelled"));
        } else if mode != 3 {
            assert_eq!(
                execution.resume(128).unwrap(),
                ExecutionStatus::Complete(Some(7))
            );
            drop(retained);
        } else {
            drop(retained);
        }
        drop(execution);
        if mode < 2 {
            assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
        } else {
            assert!(process.invoke("Get", &[]).is_err());
        }
        drop(process);
        assert_eq!(account.used(), baseline);
    }
    drop((pool, account, tenant, module));
    assert_eq!(vm.used(), bootstrap);
    println!(
        "Accounted control tokens verified: real AOT completion, atomic yield, cross-thread cancel, suspended Drop and final-owner cleanup; not complete resource accounting or MVP"
    );
}
