use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, PreemptionToken, RequestOutcome,
    ResourceAccount, Resources, ScalarModule, ScalarTask, ScalarWorker, Scheduler, SchedulerConfig,
    StackPool, WorkerEvent,
};

const CONTROL_BYTES: u64 = 4352;

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 4,
    }
}

fn tool_execution(directory: &Path) {
    let module = unsafe { ScalarModule::load_trusted(directory) }.unwrap();
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant(limits()).unwrap();
    let account = tenant.process(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let slots = bound.stats();
    assert!(module.instantiate_in_pool(&bound).is_err());
    assert!(module.instantiate_with_heap(&bound, &account, 4).is_err());
    assert_eq!(vm.used(), baseline);
    assert_eq!(bound.stats(), slots);
    let tools = StackPool::new(1, 0).unwrap();
    let mut process = module.instantiate_in_pool(&tools).unwrap();
    assert_eq!(process.invoke("inc", &[7]).unwrap(), Some(7));
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    assert!(
        process
            .start_in_pool("inc", &[99], 128, &cancellation, &preemption, &bound)
            .is_err()
    );
    let mut task = ScalarTask::new(process);
    assert!(
        task.start(
            &module.handler("inc").unwrap(),
            &[99],
            128,
            &cancellation,
            &preemption,
            &bound
        )
        .is_err()
    );
    assert!(!task.is_active());
    let mut image = task.into_process().detach();
    assert!(image.attach(&bound).is_err());
    assert_eq!(vm.used(), baseline);
    assert_eq!(bound.stats(), slots);
    let mut process = image.attach(&tools).unwrap();
    assert!(image.attach(&tools).is_err());
    assert_eq!(process.invoke("inc", &[2]).unwrap(), Some(9));
    drop((process, image, module, tools, bound, pool, account, tenant));
    assert_eq!(vm.used(), bootstrap);
}

fn direct_entries(directory: &Path) {
    for level in [
        wings_vm::ResourceLevel::Process,
        wings_vm::ResourceLevel::Tenant,
        wings_vm::ResourceLevel::Vm,
    ] {
        let vm = ResourceAccount::vm_accounted(limits()).unwrap();
        let bootstrap = vm.used();
        let tenant = vm
            .tenant(Resources {
                bytes: 3 << 20,
                ..limits()
            })
            .unwrap();
        let account = tenant
            .process(Resources {
                bytes: 1 << 20,
                ..limits()
            })
            .unwrap();
        let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let mut process = module.instantiate_in_pool(&bound).unwrap();
        let owner = match level {
            wings_vm::ResourceLevel::Process => &account,
            wings_vm::ResourceLevel::Tenant => &tenant,
            wings_vm::ResourceLevel::Vm => &vm,
        };
        let fill = owner
            .reserve(Resources {
                bytes: owner.limit().bytes - owner.used().bytes - CONTROL_BYTES + 1,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        let slots = pool.stats();
        let error = module.instantiate_in_pool(&bound).err().unwrap();
        assert!(error.contains(&format!("resource_exhausted at {level:?} account")));
        assert_eq!(vm.used(), before);
        assert_eq!(pool.stats(), slots);
        let error = process.invoke("inc", &[7]).unwrap_err();
        assert!(error.contains(&format!("resource_exhausted at {level:?} account")));
        assert_eq!(vm.used(), before);
        assert_eq!(pool.stats(), slots);
        drop(fill);
        assert_eq!(process.invoke("get", &[]).unwrap(), Some(0));
        assert_eq!(process.invoke("inc", &[7]).unwrap(), Some(7));
        let baseline = vm.used();
        let stack_bytes =
            (pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack) as u64;
        let mut execution = process
            .start_controlled(
                "inc",
                &[2],
                128,
                &CancellationToken::default(),
                &PreemptionToken::default(),
            )
            .unwrap();
        assert_eq!(account.used().bytes, stack_bytes + CONTROL_BYTES);
        assert_eq!(vm.used().bytes, baseline.bytes + CONTROL_BYTES);
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        assert_eq!(account.used().bytes, stack_bytes + CONTROL_BYTES);
        assert_eq!(
            execution.resume(128).unwrap(),
            ExecutionStatus::Complete(Some(9))
        );
        drop(execution);
        assert_eq!(vm.used(), baseline);
        drop((process, module, bound, pool, account, tenant));
        assert_eq!(vm.used(), bootstrap);
    }
}

fn scalar(directory: &Path) {
    let tool_module = unsafe { ScalarModule::load_trusted(directory) }.unwrap();
    for stop in [false, true] {
        let vm = ResourceAccount::vm_accounted(limits()).unwrap();
        let bootstrap = vm.used();
        assert!(bootstrap.bytes > 0);
        let full = vm
            .reserve(Resources {
                bytes: vm.limit().bytes - bootstrap.bytes,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        assert!(vm.tenant(limits()).is_err());
        assert!(unsafe { ScalarModule::load_accounted(directory, &vm) }.is_err());
        assert_eq!(vm.used(), before);
        drop(full);
        let tenant = vm.tenant(limits()).unwrap();
        assert!(vm.used().bytes > bootstrap.bytes);
        let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
        let full = vm
            .reserve(Resources {
                bytes: vm.limit().bytes - vm.used().bytes,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        assert!(ScalarWorker::new(&tenant, 1).is_err());
        assert_eq!(vm.used(), before);
        drop(full);
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let before = vm.used();
        assert!(
            worker
                .spawn(&tool_module, limits(), 2)
                .unwrap_err()
                .contains("accounted module")
        );
        assert_eq!(vm.used(), before);
        let pid = worker.spawn(&module, limits(), 2).unwrap();
        let handler = module.handler("inc").unwrap();
        let request = worker
            .request(pid, &handler, &[5], 128, Duration::ZERO)
            .unwrap();
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::TimedOut)
        );
        assert_eq!(vm.used().requests, 1);
        if stop {
            worker.stop(pid).unwrap();
        } else {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                assert!(Instant::now() < deadline, "accounted root scalar deadline");
                if worker.tick(128).unwrap() == WorkerEvent::Delivered(pid) {
                    break;
                }
            }
            assert_eq!(worker.committed_state(pid).unwrap(), 5);
        }
        assert_eq!(vm.used().requests, 0);
        assert_eq!(vm.used().messages, 0);
        drop((handler, worker, module, tenant));
        assert_eq!(vm.used(), bootstrap);
    }
}

fn heap(directory: &Path) {
    let tool_module = unsafe { ScalarModule::load_trusted(directory) }.unwrap();
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant(limits()).unwrap();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    let account = tenant.process(limits()).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0)
        .unwrap()
        .for_process(&account)
        .unwrap();
    let before = vm.used();
    let slots = pool.stats();
    assert!(
        tool_module
            .instantiate_with_heap(&pool, &account, 4)
            .is_err()
    );
    assert_eq!(vm.used(), before);
    assert_eq!(pool.stats(), slots);
    drop((pool, account));
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let before = vm.used();
    assert!(
        worker
            .spawn_with_heap(&tool_module, limits(), 1, 4)
            .unwrap_err()
            .contains("accounted module")
    );
    assert_eq!(vm.used(), before);
    let pid = worker.spawn_with_heap(&module, limits(), 1, 4).unwrap();
    for (name, arguments, expected) in [
        ("fill", &[4096, 7][..], RequestOutcome::Reply(4096)),
        ("fail", &[][..], RequestOutcome::Failed),
    ] {
        let handler = module.handler(name).unwrap();
        let request = worker
            .request(pid, &handler, arguments, 8192, Duration::from_secs(3))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline, "accounted root heap deadline");
            worker.tick(64).unwrap();
            if let Some(outcome) = worker.take_reply(request).unwrap() {
                assert_eq!(outcome, expected);
                break;
            }
        }
        if name == "fill" {
            let state = worker.byte_state_snapshot(pid).unwrap().unwrap();
            assert_eq!(state.length, 4096);
            assert_eq!(state.prefix, [7; 32]);
        } else {
            let exit = worker.last_exit().unwrap();
            assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
            let state = exit.byte_state.unwrap();
            assert_eq!(state.length, 4096);
            assert_eq!(state.prefix, [7; 32]);
        }
    }
    drop((worker, module, tenant));
    assert_eq!(vm.used(), bootstrap);
}

fn scheduler(directory: &Path, heap: bool) {
    let tool_module = unsafe { ScalarModule::load_trusted(directory) }.unwrap();
    for workers in [1, 2] {
        let vm = ResourceAccount::vm_accounted(limits()).unwrap();
        let bootstrap = vm.used();
        let tenant = vm.tenant(limits()).unwrap();
        let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers,
                capacity: 1,
                requests: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let before = vm.used();
        let rejected = if heap {
            handle.spawn_with_heap(&tool_module, limits(), 1, 4)
        } else {
            handle.spawn(&tool_module, limits(), 1)
        };
        assert!(rejected.unwrap_err().contains("accounted module"));
        assert_eq!(vm.used(), before);
        let pid = if heap {
            handle.spawn_with_heap(&module, limits(), 1, 4)
        } else {
            handle.spawn(&module, limits(), 1)
        }
        .unwrap();
        let (handler, arguments, steps, expected) = if heap {
            ("fill", &[4096, 7][..], 8192, 4096)
        } else {
            ("inc", &[3][..], 128, 3)
        };
        let mut request = handle
            .request(
                pid,
                &module.handler(handler).unwrap(),
                arguments,
                steps,
                Duration::from_secs(3),
            )
            .unwrap();
        drop((scheduler, handle, module, tenant));
        assert_eq!(vm.used().requests, 1);
        assert_eq!(vm.used().messages, 0);
        assert_eq!(vm.used().processes, 0);
        assert!(vm.used().bytes > bootstrap.bytes);
        assert!(match request.try_take().unwrap() {
            Some(RequestOutcome::Reply(reply)) => reply == expected,
            Some(RequestOutcome::Failed) => true,
            _ => false,
        });
        assert_eq!(vm.used().requests, 0);
        assert!(vm.used().bytes > bootstrap.bytes);
        drop(request);
        assert_eq!(vm.used(), bootstrap);
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    tool_execution(Path::new(&arguments[1]));
    direct_entries(Path::new(&arguments[1]));
    scalar(Path::new(&arguments[1]));
    heap(Path::new(&arguments[2]));
    scheduler(Path::new(&arguments[1]), false);
    scheduler(Path::new(&arguments[2]), true);
    println!(
        "Accounted VM bootstrap verified: bounded root admission, enforced child metadata and module leases, AOT scalar/Bytes cleanup and one/two worker result leases; not complete MVP"
    );
}
