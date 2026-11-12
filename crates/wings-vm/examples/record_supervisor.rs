use std::path::Path;
use std::time::Duration;

use wings_vm::{
    Pid, RequestId, RequestOutcome, ResourceAccount, Resources, RestartPolicy, RestartStatus,
    ScalarModule, ScalarSupervisor, SupervisorEvent, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 16 << 20,
        processes: 8,
        messages: 8,
        requests: 8,
    }
}

fn request(
    supervisor: &mut ScalarSupervisor,
    module: &ScalarModule,
    pid: Pid,
    name: &str,
    arguments: &[i64],
) -> RequestId {
    supervisor
        .request(
            pid,
            &module.handler(name).unwrap(),
            arguments,
            128,
            Duration::from_secs(60),
        )
        .unwrap()
}

fn deliver(supervisor: &mut ScalarSupervisor, pid: Pid) {
    for _ in 0..128 {
        match supervisor.tick(1).unwrap() {
            SupervisorEvent::Worker(WorkerEvent::Suspended(actual)) => assert_eq!(actual, pid),
            SupervisorEvent::Worker(WorkerEvent::Delivered(actual)) => {
                assert_eq!(actual, pid);
                return;
            }
            event => panic!("unexpected event {event:?}"),
        }
    }
    panic!("bounded record delivery did not complete");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }.unwrap();
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).unwrap();
    let mut supervisor = ScalarSupervisor::new(&tenant, 2, 8).unwrap();
    let baseline = vm.used();
    let child = supervisor
        .spawn_with_heap(
            &module,
            limits(),
            2,
            2,
            RestartPolicy::OnFailure {
                max_restarts: 2,
                within: Duration::from_secs(3600),
            },
        )
        .unwrap();
    let sibling = supervisor
        .spawn_with_heap(&module, limits(), 2, 2, RestartPolicy::Never)
        .unwrap();
    let sibling_pid = supervisor.current_pid(sibling).unwrap();
    let seed = request(&mut supervisor, &module, sibling_pid, "Set", &[22]);
    deliver(&mut supervisor, sibling_pid);
    assert_eq!(
        supervisor.take_reply(seed).unwrap(),
        Some(RequestOutcome::Reply(22))
    );

    for attempt in 0..3 {
        let pid = supervisor.current_pid(child).unwrap();
        let initial = request(&mut supervisor, &module, pid, "Get", &[]);
        deliver(&mut supervisor, pid);
        assert_eq!(
            supervisor.take_reply(initial).unwrap(),
            Some(RequestOutcome::Reply(7))
        );
        let set = request(&mut supervisor, &module, pid, "Set", &[11]);
        deliver(&mut supervisor, pid);
        assert_eq!(
            supervisor.take_reply(set).unwrap(),
            Some(RequestOutcome::Reply(11))
        );
        let fail = request(&mut supervisor, &module, pid, "Fail", &[]);
        let queued = request(&mut supervisor, &module, pid, "Set", &[999]);
        let mut exited = false;
        for _ in 0..128 {
            match supervisor.tick(1).unwrap() {
                SupervisorEvent::Worker(WorkerEvent::Suspended(actual)) => assert_eq!(actual, pid),
                SupervisorEvent::Exited {
                    child: actual_child,
                    pid: actual,
                    reason,
                    restart,
                } => {
                    assert_eq!((actual_child, actual), (child, pid));
                    assert!(reason.contains("division by zero"));
                    if attempt < 2 {
                        let next = supervisor.current_pid(child).unwrap();
                        assert_ne!(next, pid);
                        assert_eq!(restart, RestartStatus::Restarted(next));
                        assert_eq!(supervisor.queued(next).unwrap(), 0);
                        assert!(
                            supervisor
                                .send(pid, &module.handler("Replace").unwrap(), &[1], 128)
                                .is_err()
                        );
                    } else {
                        assert_eq!(restart, RestartStatus::IntensityExceeded);
                        assert!(supervisor.current_pid(child).is_err());
                    }
                    exited = true;
                    break;
                }
                event => panic!("unexpected failure event {event:?}"),
            }
        }
        assert!(exited);
        for pending in [fail, queued] {
            assert_eq!(
                supervisor.take_reply(pending).unwrap(),
                Some(RequestOutcome::Failed)
            );
        }
        assert_eq!(supervisor.current_pid(sibling).unwrap(), sibling_pid);
        let get = request(&mut supervisor, &module, sibling_pid, "Get", &[]);
        deliver(&mut supervisor, sibling_pid);
        assert_eq!(
            supervisor.take_reply(get).unwrap(),
            Some(RequestOutcome::Reply(22))
        );
    }
    let pending = request(&mut supervisor, &module, sibling_pid, "Set", &[44]);
    assert_eq!(
        supervisor.tick(1).unwrap(),
        SupervisorEvent::Worker(WorkerEvent::Suspended(sibling_pid))
    );
    supervisor.stop(sibling).unwrap();
    assert_eq!(
        supervisor.take_reply(pending).unwrap(),
        Some(RequestOutcome::Failed)
    );
    assert_eq!(vm.used(), baseline);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Record supervision verified: fresh heaps, reset state, no replay, intensity, sibling isolation and cleanup"
    );
}
