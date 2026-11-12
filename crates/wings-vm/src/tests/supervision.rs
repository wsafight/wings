use super::*;

fn policy(max_restarts: usize) -> RestartPolicy {
    RestartPolicy::OnFailure {
        max_restarts,
        within: Duration::from_secs(3600),
    }
}

fn fixture(
    capacity: usize,
) -> (
    ResourceAccount,
    ResourceAccount,
    ScalarSupervisor,
    ScalarModule,
) {
    let [vm, tenant, _] = accounts();
    let supervisor = ScalarSupervisor::new(&tenant, capacity, 8).unwrap();
    (vm, tenant, supervisor, module())
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
            64,
            Duration::from_secs(60),
        )
        .unwrap()
}

fn failure(supervisor: &mut ScalarSupervisor, module: &ScalarModule, pid: Pid) -> SupervisorEvent {
    supervisor
        .send(pid, &module.handler("Fail").unwrap(), &[], 64)
        .unwrap();
    supervisor.tick(64).unwrap()
}

#[test]
fn supervisor_constructor_and_policy_rejections_leave_all_accounts_unchanged() {
    let [vm, tenant, process] = accounts();
    for account in [&vm, &process] {
        assert!(ScalarSupervisor::new(account, 1, 1).is_err());
    }
    for (capacity, pending) in [(0, 1), (257, 1), (1, 0), (1, 257)] {
        assert!(ScalarSupervisor::new(&tenant, capacity, pending).is_err());
        assert_eq!(vm.used(), Resources::default());
    }
    let module = module();
    let mut supervisor = ScalarSupervisor::new(&tenant, 1, 1).unwrap();
    let baseline = vm.used();
    for invalid in [
        policy(0),
        policy(65),
        RestartPolicy::OnFailure {
            max_restarts: 1,
            within: Duration::ZERO,
        },
        RestartPolicy::OnFailure {
            max_restarts: 1,
            within: Duration::MAX,
        },
    ] {
        assert!(
            supervisor
                .spawn(&module, limits(1 << 20), 1, invalid)
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        assert_eq!(supervisor.process_count(), 0);
    }
    for mailbox in [0, 65] {
        assert!(
            supervisor
                .spawn(&module, limits(1 << 20), mailbox, policy(64))
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
    }
    assert!(supervisor.spawn(&module, limits(1), 1, policy(1)).is_err());
    assert_eq!(vm.used(), baseline);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn failed_child_restarts_fresh_without_replay_or_affecting_healthy_sibling() {
    let (vm, tenant, mut supervisor, module) = fixture(2);
    let child = supervisor
        .spawn(&module, limits(1 << 20), 4, policy(2))
        .unwrap();
    let sibling = supervisor
        .spawn(&module, limits(1 << 20), 1, RestartPolicy::Never)
        .unwrap();
    let old = supervisor.current_pid(child).unwrap();
    let healthy = supervisor.current_pid(sibling).unwrap();
    let seed = request(&mut supervisor, &module, old, "Bump", &[7]);
    assert_eq!(
        supervisor.tick(64).unwrap(),
        SupervisorEvent::Worker(WorkerEvent::Delivered(old))
    );
    let sibling_request = request(&mut supervisor, &module, healthy, "Bump", &[11]);
    assert_eq!(
        supervisor.tick(64).unwrap(),
        SupervisorEvent::Worker(WorkerEvent::Delivered(healthy))
    );
    let failed = request(&mut supervisor, &module, old, "FailRequest", &[]);
    let queued = request(&mut supervisor, &module, old, "Bump", &[900]);
    let cancelled = request(&mut supervisor, &module, old, "Get", &[]);
    supervisor.cancel_request(cancelled).unwrap();
    let timeout = supervisor
        .request(
            old,
            &module.handler("Get").unwrap(),
            &[],
            64,
            Duration::ZERO,
        )
        .unwrap();
    let event = supervisor.tick(64).unwrap();
    let replacement = supervisor.current_pid(child).unwrap();
    assert_ne!(replacement, old);
    assert_eq!(
        event,
        SupervisorEvent::Exited {
            child,
            pid: old,
            reason: "managed division by zero".into(),
            restart: RestartStatus::Restarted(replacement),
        }
    );
    assert_eq!(supervisor.current_pid(sibling), Ok(healthy));
    assert_eq!(supervisor.committed_state(healthy), Ok(11));
    assert_eq!(supervisor.committed_state(replacement), Ok(0));
    assert_eq!(supervisor.queued(replacement), Ok(0));
    assert!(
        supervisor
            .send(old, &module.handler("Add").unwrap(), &[1], 64)
            .is_err()
    );
    assert!(
        supervisor
            .request(
                old,
                &module.handler("Get").unwrap(),
                &[],
                64,
                Duration::from_secs(1)
            )
            .is_err()
    );
    for (id, result) in [
        (seed, RequestOutcome::Reply(7)),
        (sibling_request, RequestOutcome::Reply(11)),
        (failed, RequestOutcome::Failed),
        (queued, RequestOutcome::Failed),
        (cancelled, RequestOutcome::Cancelled),
        (timeout, RequestOutcome::TimedOut),
    ] {
        assert_eq!(supervisor.take_reply(id), Ok(Some(result)));
        assert!(supervisor.take_reply(id).is_err());
    }
    let recovered = request(&mut supervisor, &module, replacement, "Bump", &[3]);
    assert_eq!(
        supervisor.tick(64).unwrap(),
        SupervisorEvent::Worker(WorkerEvent::Delivered(replacement))
    );
    assert_eq!(
        supervisor.take_reply(recovered),
        Ok(Some(RequestOutcome::Reply(3)))
    );
    assert_eq!(supervisor.committed_state(replacement), Ok(3));
    assert_eq!(tenant.used().messages, 0);
    assert_eq!(tenant.used().requests, 0);
    assert_eq!(tenant.used().processes, 2);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn successful_messages_do_not_reset_restart_intensity_or_revive_retired_child_ids() {
    let (vm, _, mut supervisor, module) = fixture(1);
    let baseline = vm.used();
    let child = supervisor
        .spawn(&module, limits(1 << 20), 1, policy(2))
        .unwrap();
    for _ in 0..2 {
        let old = supervisor.current_pid(child).unwrap();
        let event = failure(&mut supervisor, &module, old);
        let next = supervisor.current_pid(child).unwrap();
        assert!(
            matches!(event, SupervisorEvent::Exited { restart: RestartStatus::Restarted(pid), .. } if pid == next)
        );
        let id = request(&mut supervisor, &module, next, "Bump", &[1]);
        assert_eq!(
            supervisor.tick(64).unwrap(),
            SupervisorEvent::Worker(WorkerEvent::Delivered(next))
        );
        assert_eq!(
            supervisor.take_reply(id),
            Ok(Some(RequestOutcome::Reply(1)))
        );
    }
    let old = supervisor.current_pid(child).unwrap();
    assert_eq!(
        failure(&mut supervisor, &module, old),
        SupervisorEvent::Exited {
            child,
            pid: old,
            reason: "managed division by zero".into(),
            restart: RestartStatus::IntensityExceeded,
        }
    );
    assert!(supervisor.current_pid(child).is_err());
    assert!(supervisor.stop(child).is_err());
    assert_eq!(supervisor.process_count(), 0);
    assert_eq!(vm.used(), baseline);
    assert_eq!(
        supervisor.tick(64),
        Ok(SupervisorEvent::Worker(WorkerEvent::Idle))
    );
    let next = supervisor
        .spawn(&module, limits(1 << 20), 1, RestartPolicy::Never)
        .unwrap();
    assert_ne!(child, next);
    assert!(supervisor.current_pid(child).is_err());
    let pid = supervisor.current_pid(next).unwrap();
    assert!(matches!(
        failure(&mut supervisor, &module, pid),
        SupervisorEvent::Exited {
            restart: RestartStatus::Disabled,
            ..
        }
    ));
    assert!(supervisor.current_pid(next).is_err());
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn stopping_a_suspended_child_bypasses_full_queue_and_never_restarts() {
    let (vm, tenant, mut supervisor, module) = fixture(1);
    let baseline = vm.used();
    let child = supervisor
        .spawn(&module, limits(1 << 20), 1, policy(1))
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let active = request(&mut supervisor, &module, pid, "Bump", &[5]);
    assert_eq!(
        supervisor.tick(1),
        Ok(SupervisorEvent::Worker(WorkerEvent::Suspended(pid)))
    );
    let queued = request(&mut supervisor, &module, pid, "Bump", &[6]);
    assert_eq!(supervisor.queued(pid), Ok(1));
    supervisor.stop(child).unwrap();
    assert!(supervisor.current_pid(child).is_err());
    assert_eq!(supervisor.process_count(), 0);
    assert_eq!(
        supervisor.take_reply(active),
        Ok(Some(RequestOutcome::Failed))
    );
    assert_eq!(
        supervisor.take_reply(queued),
        Ok(Some(RequestOutcome::Failed))
    );
    assert_eq!(vm.used(), baseline);
    assert_eq!(tenant.used(), baseline);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn supervisor_history_reservation_and_child_capacity_are_bounded_and_atomic() {
    let (vm, tenant, mut supervisor, module) = fixture(1);
    let baseline = vm.used();
    let fill = tenant
        .reserve(Resources {
            bytes: tenant.limit().bytes - tenant.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    let full = vm.used();
    assert!(
        supervisor
            .spawn(&module, limits(1 << 20), 1, policy(1))
            .is_err()
    );
    assert_eq!(vm.used(), full);
    drop(fill);
    let child = supervisor
        .spawn(&module, limits(1 << 20), 1, policy(64))
        .unwrap();
    let charged = vm.used();
    assert!(
        supervisor
            .spawn(&module, limits(1 << 20), 1, RestartPolicy::Never)
            .is_err()
    );
    assert_eq!(vm.used(), charged);
    supervisor.stop(child).unwrap();
    assert_eq!(vm.used(), baseline);
    let never = supervisor
        .spawn(&module, limits(1 << 20), 1, RestartPolicy::Never)
        .unwrap();
    assert_eq!(
        charged.bytes - vm.used().bytes,
        64 * std::mem::size_of::<Option<std::time::Instant>>() as u64
    );
    supervisor.stop(never).unwrap();
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn foreign_child_and_request_ids_are_rejected_without_changes() {
    let (vm, tenant, mut first, module) = fixture(1);
    let mut second = ScalarSupervisor::new(&tenant, 1, 1).unwrap();
    let child = first.spawn(&module, limits(1 << 20), 1, policy(1)).unwrap();
    let pid = first.current_pid(child).unwrap();
    let id = request(&mut first, &module, pid, "Get", &[]);
    let before = vm.used();
    assert!(second.current_pid(child).is_err());
    assert!(second.stop(child).is_err());
    assert!(
        second
            .send(pid, &module.handler("Add").unwrap(), &[1], 64)
            .is_err()
    );
    assert!(second.take_reply(id).is_err());
    assert!(second.cancel_request(id).is_err());
    assert_eq!(vm.used(), before);
    drop((first, second));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn restart_initializer_failure_keeps_original_exit_and_stops_retrying() {
    static INITIALIZATIONS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    unsafe extern "C" fn initializer(
        state: i64,
        args: *const i64,
        output: *mut ScalarOutput,
        context: *mut ScalarContext,
    ) -> u32 {
        if INITIALIZATIONS.fetch_add(1, Ordering::SeqCst) == 0 {
            unsafe { initialize(state, args, output, context) }
        } else {
            STATUS_BUDGET_EXHAUSTED
        }
    }
    let (vm, tenant, mut supervisor, mut module) = fixture(1);
    Arc::get_mut(&mut module.inner).unwrap().initializer = initializer;
    let baseline = vm.used();
    let child = supervisor
        .spawn(&module, limits(1 << 20), 1, policy(2))
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let id = request(&mut supervisor, &module, pid, "FailRequest", &[]);
    assert_eq!(
        supervisor.tick(64),
        Ok(SupervisorEvent::Exited {
            child,
            pid,
            reason: "managed division by zero".into(),
            restart: RestartStatus::Failed("managed execution step budget exhausted".into()),
        })
    );
    assert!(supervisor.current_pid(child).is_err());
    assert_eq!(supervisor.take_reply(id), Ok(Some(RequestOutcome::Failed)));
    assert_eq!(vm.used(), baseline);
    assert_eq!(tenant.used(), baseline);
    assert_eq!(
        supervisor.tick(64),
        Ok(SupervisorEvent::Worker(WorkerEvent::Idle))
    );
    assert_eq!(INITIALIZATIONS.load(Ordering::SeqCst), 2);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn supervisor_timed_execution_validates_and_drop_joins_with_suspended_child() {
    let (vm, _, mut supervisor, module) = fixture(1);
    let child = supervisor
        .spawn(&module, limits(1 << 20), 1, policy(1))
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    request(&mut supervisor, &module, pid, "Bump", &[1]);
    assert!(supervisor.tick_timed(1, Duration::from_secs(1)).is_err());
    let before = vm.used();
    supervisor.enable_timer().unwrap();
    assert!(vm.used().bytes > before.bytes);
    let before = vm.used();
    assert!(supervisor.enable_timer().is_err());
    assert!(supervisor.tick(0).is_err());
    assert!(supervisor.tick_timed(1, Duration::ZERO).is_err());
    assert_eq!(vm.used(), before);
    assert_eq!(
        supervisor.tick_timed(1, Duration::from_secs(1)),
        Ok(SupervisorEvent::Worker(WorkerEvent::Suspended(pid)))
    );
    assert_eq!(supervisor.resources(pid).unwrap().messages, 1);
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}
