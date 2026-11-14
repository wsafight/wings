use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    ExecutionFault, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    Scheduler, SchedulerConfig, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 8,
        requests: 4,
    }
}

fn scalar(directory: &Path) {
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
        let pid = worker.spawn(&module, limits(), 2).unwrap();
        let handler = module.handler("Inc").unwrap();
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
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant(limits()).unwrap();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }.unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn_with_heap(&module, limits(), 1, 4).unwrap();
    for (name, arguments, expected) in [
        ("Fill", &[4096, 7][..], RequestOutcome::Reply(4096)),
        ("Fail", &[][..], RequestOutcome::Failed),
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
        if name == "Fill" {
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

fn scheduler(directory: &Path) {
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
        let pid = handle.spawn(&module, limits(), 1).unwrap();
        let mut request = handle
            .request(
                pid,
                &module.handler("Inc").unwrap(),
                &[3],
                128,
                Duration::from_secs(3),
            )
            .unwrap();
        drop((scheduler, handle, module, tenant));
        assert_eq!(vm.used().requests, 1);
        assert_eq!(vm.used().messages, 0);
        assert_eq!(vm.used().processes, 0);
        assert!(vm.used().bytes > bootstrap.bytes);
        assert!(matches!(
            request.try_take().unwrap(),
            Some(RequestOutcome::Reply(3) | RequestOutcome::Failed)
        ));
        assert_eq!(vm.used().requests, 0);
        assert!(vm.used().bytes > bootstrap.bytes);
        drop(request);
        assert_eq!(vm.used(), bootstrap);
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    scalar(Path::new(&arguments[1]));
    heap(Path::new(&arguments[2]));
    scheduler(Path::new(&arguments[1]));
    println!(
        "Accounted VM bootstrap verified: bounded root admission, enforced child metadata, AOT scalar/Bytes cleanup and one/two worker result leases; not complete MVP"
    );
}
