use std::path::Path;
use std::time::Duration;

use wings_vm::{
    Pid, RequestOutcome, ResourceAccount, Resources, RestartPolicy, RestartStatus, ScalarModule,
    ScalarSupervisor, SupervisorEvent, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 8 << 20,
        processes: 4,
        messages: 16,
        requests: 16,
    }
}

fn deliver(supervisor: &mut ScalarSupervisor, pid: Pid, previous: i64, expected: i64) {
    for _ in 0..128 {
        match supervisor.tick(1).expect("delivery tick") {
            SupervisorEvent::Worker(WorkerEvent::Suspended(actual)) => {
                assert_eq!(actual, pid);
                assert_eq!(supervisor.committed_state(pid), Ok(previous));
            }
            SupervisorEvent::Worker(WorkerEvent::Delivered(actual)) => {
                assert_eq!(actual, pid);
                assert_eq!(supervisor.committed_state(pid), Ok(expected));
                return;
            }
            event => panic!("unexpected delivery: {event:?}"),
        }
    }
    panic!("handler did not complete within bounded ticks");
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let module = unsafe { ScalarModule::load_trusted(Path::new(&arguments[1])) }
        .expect("trusted Queue artifact");
    let vm = ResourceAccount::vm(limits());
    let tenant = vm.tenant(limits()).expect("tenant");
    let mut supervisor = ScalarSupervisor::new(&tenant, 2, 8).expect("supervisor");
    let child = supervisor
        .spawn(
            &module,
            limits(),
            2,
            RestartPolicy::OnFailure {
                max_restarts: 2,
                within: Duration::from_secs(3600),
            },
        )
        .expect("recoverable child");
    let sibling = supervisor
        .spawn(&module, limits(), 1, RestartPolicy::Never)
        .expect("sibling");
    let sibling_pid = supervisor.current_pid(sibling).expect("sibling Pid");
    let bump = module.handler("Bump").expect("Bump");
    let fail = module.handler("FailRequest").expect("FailRequest");
    let timeout = Duration::from_secs(60);
    supervisor
        .send(sibling_pid, &module.handler("Add").expect("Add"), &[11], 64)
        .expect("seed sibling");
    deliver(&mut supervisor, sibling_pid, 0, 11);

    for attempt in 0..3 {
        let pid = supervisor.current_pid(child).expect("current child Pid");
        let seed = supervisor
            .request(pid, &bump, &[5], 64, timeout)
            .expect("seed state");
        deliver(&mut supervisor, pid, 0, 5);
        assert_eq!(
            supervisor.take_reply(seed),
            Ok(Some(RequestOutcome::Reply(5)))
        );
        let failed = supervisor
            .request(pid, &fail, &[], 64, timeout)
            .expect("enqueue failure");
        let queued = supervisor
            .request(pid, &bump, &[999], 64, timeout)
            .expect("enqueue work behind failure");
        assert!(supervisor.request(pid, &bump, &[1], 64, timeout).is_err());
        let mut terminated = false;
        for _ in 0..128 {
            match supervisor.tick(1).expect("failure tick") {
                SupervisorEvent::Worker(WorkerEvent::Suspended(actual)) => {
                    assert_eq!(actual, pid);
                    assert_eq!(supervisor.committed_state(pid), Ok(5));
                }
                SupervisorEvent::Exited {
                    child: actual_child,
                    pid: actual_pid,
                    reason,
                    restart,
                } => {
                    assert_eq!(actual_child, child);
                    assert_eq!(actual_pid, pid);
                    assert!(reason.contains("division by zero"));
                    if attempt < 2 {
                        let next = supervisor.current_pid(child).expect("restarted Pid");
                        assert_ne!(pid, next);
                        assert_eq!(restart, RestartStatus::Restarted(next));
                        assert_eq!(supervisor.committed_state(next), Ok(0));
                        assert_eq!(supervisor.queued(next), Ok(0));
                        assert!(supervisor.send(pid, &bump, &[1], 64).is_err());
                        assert!(supervisor.request(pid, &bump, &[1], 64, timeout).is_err());
                    } else {
                        assert_eq!(restart, RestartStatus::IntensityExceeded);
                        assert!(supervisor.current_pid(child).is_err());
                    }
                    terminated = true;
                    break;
                }
                event => panic!("unexpected failure event: {event:?}"),
            }
        }
        assert!(terminated, "failure did not terminate within bounded ticks");
        assert_eq!(supervisor.current_pid(sibling), Ok(sibling_pid));
        assert_eq!(supervisor.committed_state(sibling_pid), Ok(11));
        for id in [failed, queued] {
            assert_eq!(supervisor.take_reply(id), Ok(Some(RequestOutcome::Failed)));
        }
        assert_eq!(tenant.used().requests, 0);
        assert_eq!(tenant.used().messages, 0);
    }
    assert_eq!(supervisor.process_count(), 1);
    let id = supervisor
        .request(sibling_pid, &bump, &[2], 64, timeout)
        .expect("sibling remains usable");
    assert_eq!(
        supervisor.tick(1),
        Ok(SupervisorEvent::Worker(WorkerEvent::Suspended(sibling_pid)))
    );
    let queued = supervisor
        .request(sibling_pid, &bump, &[3], 64, timeout)
        .expect("fill sibling queue");
    supervisor.stop(sibling).expect("stop suspended sibling");
    assert_eq!(supervisor.take_reply(id), Ok(Some(RequestOutcome::Failed)));
    assert_eq!(
        supervisor.take_reply(queued),
        Ok(Some(RequestOutcome::Failed))
    );
    assert_eq!(supervisor.process_count(), 0);
    assert_eq!(
        supervisor.tick(64),
        Ok(SupervisorEvent::Worker(WorkerEvent::Idle))
    );
    drop(supervisor);
    assert_eq!(tenant.used(), Resources::default());
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Supervision verified: fresh restart, bounded intensity, no replay, sibling isolation and cleanup"
    );
}
