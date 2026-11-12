use std::path::Path;
use std::time::Duration;

use wings_vm::{
    ExecutionFault, Pid, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 8,
        messages: 16,
        requests: 8,
    }
}

fn read(worker: &mut ScalarWorker, module: &ScalarModule, pid: Pid) -> i64 {
    let ticket = worker
        .request(
            pid,
            &module.handler("Read").unwrap(),
            &[],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..64 {
        worker.tick(128).unwrap();
        if let Some(RequestOutcome::Reply(value)) = worker.take_reply(ticket).unwrap() {
            return value;
        }
    }
    panic!("read did not finish");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    for (path, heap) in [(&arguments[1], false), (&arguments[2], true)] {
        let module = unsafe { ScalarModule::load_trusted(Path::new(path)) }.unwrap();
        for fail in [false, true] {
            let vm = ResourceAccount::vm(limits());
            let tenant = vm.tenant(limits()).unwrap();
            let mut worker = ScalarWorker::new(&tenant, 3).unwrap();
            let pid = if heap {
                worker.spawn_with_heap(&module, limits(), 2, 3).unwrap()
            } else {
                worker.spawn(&module, limits(), 2).unwrap()
            };
            let ticket = worker
                .request(
                    pid,
                    &module.handler("Start").unwrap(),
                    &[4, i64::from(fail)],
                    128,
                    Duration::from_secs(60),
                )
                .unwrap();
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.process_count(), 2);
            let child = worker.pids().find(|child| *child != pid).unwrap();
            assert_eq!(worker.queued(child).unwrap(), 0);
            assert_eq!(
                worker.snapshot(pid).unwrap().committed_state,
                if heap { None } else { Some(0) }
            );
            assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.queued(child).unwrap(), 1);
            let mut result = None;
            for _ in 0..32 {
                worker.tick(128).unwrap();
                if let Some(outcome) = worker.take_reply(ticket).unwrap() {
                    result = Some(outcome);
                    break;
                }
            }
            assert_eq!(
                result,
                Some(if fail {
                    RequestOutcome::Failed
                } else {
                    RequestOutcome::Reply(9)
                })
            );
            assert_eq!(read(&mut worker, &module, child), 4);
            assert_eq!(worker.process_count(), if fail { 1 } else { 2 });
            if fail {
                assert_eq!(
                    worker.last_exit().unwrap().execution.fault,
                    Some(ExecutionFault::DivisionByZero)
                );
            } else {
                assert_eq!(read(&mut worker, &module, pid), 9);
                worker.stop(pid).unwrap();
                assert_eq!(read(&mut worker, &module, child), 4);
            }
            worker.stop(child).unwrap();
            drop(worker);
            assert_eq!(vm.used(), Resources::default());
        }
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 2).unwrap();
        let pid = if heap {
            worker.spawn_with_heap(&module, limits(), 1, 3).unwrap()
        } else {
            worker.spawn(&module, limits(), 1).unwrap()
        };
        worker
            .request(
                pid,
                &module.handler("Start").unwrap(),
                &[3, 0],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        let child = worker.pids().find(|child| *child != pid).unwrap();
        worker.stop(pid).unwrap();
        assert_eq!(read(&mut worker, &module, child), 0);
        drop(worker);
        assert_eq!(vm.used(), Resources::default());

        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant(limits()).unwrap();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let pid = if heap {
            worker.spawn_with_heap(&module, limits(), 1, 3).unwrap()
        } else {
            worker.spawn(&module, limits(), 1).unwrap()
        };
        let ticket = worker
            .request(
                pid,
                &module.handler("Start").unwrap(),
                &[3, 0],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(worker.tick(128).unwrap(), WorkerEvent::Suspended(pid));
        assert_eq!(worker.process_count(), 1);
        assert!(matches!(
            worker.tick(128).unwrap(),
            WorkerEvent::Exited { .. }
        ));
        assert_eq!(
            worker.last_exit().unwrap().execution.fault,
            Some(ExecutionFault::SpawnRejected)
        );
        assert_eq!(
            worker.take_reply(ticket).unwrap(),
            Some(RequestOutcome::Failed)
        );
        drop(worker);
        assert_eq!(vm.used(), Resources::default());
    }

    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut worker = ScalarWorker::new(&tenant, 4).unwrap();
    let pid = worker.spawn(&module, limits(), 2).unwrap();
    let ticket = worker
        .request(
            pid,
            &module.handler("Me").unwrap(),
            &[6],
            128,
            Duration::from_secs(60),
        )
        .unwrap();
    for _ in 0..16 {
        worker.tick(128).unwrap();
    }
    assert_eq!(
        worker.take_reply(ticket).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    assert_eq!(read(&mut worker, &module, pid), 6);
    for flag in [false, true] {
        let ticket = worker
            .request(
                pid,
                &module.handler("Choose").unwrap(),
                &[i64::from(flag)],
                128,
                Duration::from_secs(60),
            )
            .unwrap();
        for _ in 0..16 {
            worker.tick(128).unwrap();
        }
        assert!(matches!(
            worker.take_reply(ticket).unwrap(),
            Some(RequestOutcome::Reply(_))
        ));
    }
    assert_eq!(read(&mut worker, &module, pid), 13);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Local processes verified: capabilities, fresh children, call-point sends, parent failure, stop and cleanup"
    );
}
