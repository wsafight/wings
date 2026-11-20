use super::*;
use std::time::Instant;

fn quota() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 16,
        messages: 128,
        requests: 64,
    }
}

fn fixture(workers: usize, capacity: usize) -> (ResourceAccount, ResourceAccount, Scheduler) {
    let vm = ResourceAccount::vm(quota());
    let tenant = vm.tenant_accounted(quota()).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers,
            capacity,
            requests: 4,
            slice_steps: 1,
            quantum: Duration::from_millis(1),
            yield_samples: 0,
            slice_samples: 0,
            lock_statistics: false,
        },
    )
    .unwrap();
    (vm, tenant, scheduler)
}

#[test]
fn scheduler_yield_capacity_and_startup_failures_preserve_accounting() {
    for capacity in [0, 1, 65536] {
        let vm = ResourceAccount::vm(quota());
        let tenant = vm.tenant_accounted(quota()).unwrap();
        let baseline = vm.used();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 1,
                capacity: 1,
                requests: 1,
                yield_samples: capacity,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        assert!(handle.take_yield_sample(0).unwrap().is_none());
        assert_eq!(handle.yield_statistics(0).unwrap().is_some(), capacity != 0);
        assert!(handle.take_yield_sample(1).is_err());
        assert!(handle.yield_statistics(1).is_err());
        drop(scheduler);
        assert!(vm.used().bytes > baseline.bytes);
        assert!(handle.take_yield_sample(0).unwrap().is_none());
        drop(handle);
        assert_eq!(vm.used(), baseline);
        for invalid in [65537, usize::MAX] {
            assert!(
                Scheduler::new(
                    &tenant,
                    SchedulerConfig {
                        yield_samples: invalid,
                        ..SchedulerConfig::default()
                    }
                )
                .is_err()
            );
            assert_eq!(vm.used(), baseline);
        }
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
    for level in [ResourceLevel::Tenant, ResourceLevel::Vm] {
        let vm = ResourceAccount::vm(quota());
        let tenant = vm.tenant_accounted(quota()).unwrap();
        let baseline = vm.used();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 2,
                capacity: 1,
                requests: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let default_bytes = vm.used().bytes - baseline.bytes;
        drop(scheduler);
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 2,
                capacity: 1,
                requests: 1,
                yield_samples: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let observed_extra = vm.used().bytes - baseline.bytes - default_bytes;
        drop(scheduler);
        let limited = if level == ResourceLevel::Vm {
            &vm
        } else {
            &tenant
        };
        for available in [65536, observed_extra - 1] {
            let fill = limited
                .reserve(Resources {
                    bytes: limited.limit().bytes - limited.used().bytes - default_bytes - available,
                    ..Resources::default()
                })
                .unwrap();
            let before = [vm.used(), tenant.used()];
            assert!(
                Scheduler::new(
                    &tenant,
                    SchedulerConfig {
                        workers: 2,
                        capacity: 1,
                        requests: 1,
                        yield_samples: if available == 65536 { 65536 } else { 1 },
                        ..SchedulerConfig::default()
                    }
                )
                .is_err()
            );
            assert_eq!([vm.used(), tenant.used()], before);
            drop(fill);
        }
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn scheduler_explicit_initializer_pool_preserves_states_and_rejects_foreign_or_busy_pools() {
    let (vm, tenant, scheduler) = fixture(1, 4);
    let handle = scheduler.handle();
    let module = module();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let baseline = vm.used();
    for _ in 0..2 {
        let pid = handle
            .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
            .unwrap();
        assert_eq!(pool.stats().cached, 1);
        assert_eq!(pool.stats().active, 0);
        assert_eq!(handle.snapshot(pid).unwrap().committed_state, Some(0));
        let request = handle
            .request(
                pid,
                &module.handler("Bump").unwrap(),
                &[7],
                32,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(reply(request), RequestOutcome::Reply(7));
        handle.stop(pid).unwrap();
        wait_until(|| handle.snapshot(pid).is_err());
    }
    let other = vm.tenant_accounted(quota()).unwrap();
    let foreign = StackPool::new_accounted(&other, 1, 1).unwrap();
    let unaccounted = StackPool::new(1, 1).unwrap();
    for invalid in [&foreign, &unaccounted] {
        let before = vm.used();
        assert!(
            handle
                .spawn_with_heap_in_pool(&module, quota(), 1, 4, invalid)
                .is_err()
        );
        assert_eq!(vm.used(), before);
    }
    let occupied = tenant.process_accounted(quota()).unwrap();
    let bound = pool.for_process(&occupied).unwrap();
    let stack = bound.acquire().unwrap();
    let before = vm.used();
    assert!(
        handle
            .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
            .is_err()
    );
    assert_eq!(vm.used(), before);
    drop((stack, bound, occupied));
    let pid = handle
        .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
        .unwrap();
    handle.stop(pid).unwrap();
    drop((handle, scheduler, module, foreign, unaccounted, other));
    assert_eq!(pool.stats().active, 0);
    assert_eq!(pool.stats().cached, 1);
    assert!(vm.used().bytes <= baseline.bytes);
    drop((pool, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_explicit_initializer_pool_rolls_back_quota_and_initializer_failure() {
    let (vm, tenant, scheduler) = fixture(1, 2);
    let handle = scheduler.handle();
    let mut failed = module();
    Arc::get_mut(&mut failed.inner).unwrap().initializer = fail;
    let module = module();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let pid = handle
        .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
        .unwrap();
    handle.stop(pid).unwrap();
    wait_until(|| handle.snapshot(pid).is_err());
    let baseline = vm.used();
    assert!(
        handle
            .spawn_with_heap_in_pool(&failed, quota(), 1, 4, &pool)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    assert!(
        handle
            .spawn_with_heap_in_pool(&module, limits(128 << 10), 1, 4, &pool)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    for limited in [&tenant, &vm] {
        let fill = limited
            .reserve(Resources {
                bytes: limited.limit().bytes - limited.used().bytes - 4095,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        assert!(
            handle
                .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
                .is_err()
        );
        assert_eq!(vm.used(), before);
        drop(fill);
    }
    let pid = handle
        .spawn_with_heap_in_pool(&module, quota(), 1, 4, &pool)
        .unwrap();
    assert_eq!(handle.snapshot(pid).unwrap().committed_state, Some(0));
    drop((scheduler, handle, pool, module, failed, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn supervisor_does_not_retain_explicit_initializer_pool_across_restart() {
    fn assert_send_sync<Value: Send + Sync>() {}
    assert_send_sync::<SchedulerSupervisor>();
    let (vm, tenant, mut supervisor) = supervisor_fixture(2);
    let handle = supervisor.handle();
    let module = module();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let child = supervisor
        .spawn_with_heap_in_pool(
            &module,
            quota(),
            1,
            4,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
            &pool,
        )
        .unwrap();
    drop(pool);
    let pid = supervisor.current_pid(child).unwrap();
    assert_eq!(
        reply(
            handle
                .request(
                    pid,
                    &module.handler("FailRequest").unwrap(),
                    &[],
                    32,
                    Duration::from_secs(5)
                )
                .unwrap()
        ),
        RequestOutcome::Failed
    );
    let mut replacement = None;
    wait_until(|| {
        if let Some(event) = supervisor.poll() {
            let SchedulerRestartStatus::Restarted(pid) = event.restart else {
                panic!("unexpected restart")
            };
            replacement = Some(pid);
        }
        replacement.is_some()
    });
    let pid = replacement.unwrap();
    assert_eq!(
        reply(
            handle
                .request(
                    pid,
                    &module.handler("Bump").unwrap(),
                    &[9],
                    32,
                    Duration::from_secs(5)
                )
                .unwrap()
        ),
        RequestOutcome::Reply(9)
    );
    supervisor.stop(child).unwrap();
    drop((handle, supervisor, module, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_timer_yields_have_worker_and_pid_identity_without_losing_cleanup() {
    let vm = ResourceAccount::vm(quota());
    let tenant = vm.tenant_accounted(quota()).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
            requests: 2,
            yield_samples: 1,
            slice_samples: 0,
            lock_statistics: false,
            slice_steps: u64::MAX,
            quantum: Duration::from_millis(1),
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let module = spinning_module();
    let actors: Vec<_> = (0..2)
        .map(|_| handle.spawn(&module, quota(), 1).unwrap())
        .collect();
    for pid in &actors {
        handle
            .send(*pid, &module.handler("Spin").unwrap(), &[], u64::MAX)
            .unwrap();
    }
    for worker in 0..2 {
        wait_until(|| handle.yield_statistics(worker).unwrap().unwrap().dropped > 0);
    }
    for pid in &actors {
        handle.stop(*pid).unwrap();
    }
    drop(scheduler);
    for (worker, pid) in actors.iter().enumerate() {
        let sample = handle.take_yield_sample(worker).unwrap().unwrap();
        assert_eq!(sample.worker, worker);
        assert_eq!(sample.pid, *pid);
        assert_eq!(sample.sequence, 1);
        assert!(sample.observation.yielded_at >= sample.observation.requested_at);
        assert!(handle.take_yield_sample(worker).unwrap().is_none());
        let statistics = handle.yield_statistics(worker).unwrap().unwrap();
        let counters = statistics.preemption.counters;
        assert!(statistics.dropped >= 1);
        assert_eq!(statistics.retained, 0);
        assert_eq!(statistics.taken, 1);
        assert_eq!(statistics.published, statistics.taken + statistics.dropped);
        assert_eq!(counters.confirmed, statistics.published);
        assert_eq!(counters.consumed, counters.confirmed);
        assert_eq!(
            counters.requests,
            counters.coalesced + counters.consumed + counters.cleared
        );
        assert!(!counters.overflowed && !statistics.overflowed);
        assert!(statistics.preemption.pending_since.is_none());
    }
    drop((handle, module, tenant));
    assert_eq!(vm.used(), Resources::default());
}

fn reply(mut request: IngressRequest) -> RequestOutcome {
    request.wait(Duration::from_secs(5)).unwrap().unwrap()
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "scheduler progress deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
}

unsafe extern "C" fn spin(
    _state: i64,
    _arguments: *const i64,
    _output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    loop {
        let status = unsafe { checkpoint(context) };
        if status != STATUS_OK {
            return status;
        }
    }
}

fn spinning_module() -> ScalarModule {
    let fixture = module();
    let mut manifest = fixture.manifest().clone();
    manifest.handlers.push(Handler {
        name: "Spin".into(),
        parameters: vec![],
        reply: None,
        completion: None,
    });
    let mut entries = fixture.inner.entries.to_vec();
    entries.push(spin);
    ScalarModule {
        inner: Arc::new(ModuleData {
            manifest,
            initializer: initialize,
            entries: entries.into_boxed_slice(),
            direct_meta: Box::new([]),
            _library: None,
            lease: None,
        }),
    }
}

#[test]
fn scheduler_runs_more_actors_than_workers_and_migrates_without_reinitializing() {
    fn assert_send_sync<Value: Send + Sync>() {}
    assert_send_sync::<Scheduler>();
    assert_send_sync::<SchedulerHandle>();
    let (vm, tenant, scheduler) = fixture(2, 8);
    let handle = scheduler.handle();
    let module = module();
    let actors: Vec<_> = (0..8)
        .map(|_| handle.spawn(&module, quota(), 16).unwrap())
        .collect();
    assert!(handle.spawn(&module, quota(), 1).is_err());
    for round in 1..=8 {
        for (index, pid) in actors.iter().copied().enumerate() {
            let request = handle
                .request(
                    pid,
                    &module.handler("Bump").unwrap(),
                    &[(index + 1) as i64],
                    32,
                    Duration::from_secs(5),
                )
                .unwrap();
            assert_eq!(
                reply(request),
                RequestOutcome::Reply((round * (index + 1)) as i64)
            );
            let snapshot = handle.snapshot(pid).unwrap();
            assert_eq!(snapshot.migrations, (round - 1) as u64);
            assert_eq!(snapshot.owner, None);
        }
    }
    drop((scheduler, handle, tenant, module));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_concurrent_producers_preserve_single_actor_fifo_and_state() {
    let (vm, tenant, scheduler) = fixture(3, 4);
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, quota(), 32).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let handle = handle.clone();
            let handler = module.handler("Bump").unwrap();
            scope.spawn(move || {
                let mut previous = 0;
                for _ in 0..32 {
                    let request = handle
                        .request(pid, &handler, &[1], 32, Duration::from_secs(5))
                        .unwrap();
                    let RequestOutcome::Reply(value) = reply(request) else {
                        panic!("unexpected request outcome")
                    };
                    assert!(value > previous);
                    previous = value;
                }
            });
        }
    });
    let value = handle
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(value), RequestOutcome::Reply(128));
    assert_eq!(handle.snapshot(pid).unwrap().migrations, 128);
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_rejects_foreign_schema_and_rolls_back_failed_spawn() {
    let (vm, tenant, scheduler) = fixture(2, 2);
    let handle = scheduler.handle();
    let module = module();
    let baseline = vm.used();
    assert!(handle.spawn(&module, limits(0), 2).is_err());
    assert_eq!(vm.used(), baseline);
    let pid = handle.spawn(&module, quota(), 2).unwrap();
    let baseline = vm.used();
    for handler in [
        module.handler("Bump").unwrap(),
        super::module().handler("Add").unwrap(),
    ] {
        assert!(handle.send(pid, &handler, &[1], 32).is_err());
    }
    assert!(
        handle
            .send(pid, &module.handler("Bool").unwrap(), &[2], 32)
            .is_err()
    );
    assert!(
        handle
            .send(pid, &module.handler("Unit").unwrap(), &[1], 32)
            .is_err()
    );
    assert!(
        handle
            .request(
                pid,
                &module.handler("Add").unwrap(),
                &[1],
                32,
                Duration::from_secs(1)
            )
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    let foreign = fixture(1, 1);
    assert!(foreign.2.handle().snapshot(pid).is_err());
    handle.stop(pid).unwrap();
    assert!(handle.snapshot(pid).is_err());
    let replacement = handle.spawn(&module, quota(), 2).unwrap();
    assert_ne!(pid, replacement);
    assert!(
        handle
            .send(pid, &module.handler("Add").unwrap(), &[1], 32)
            .is_err()
    );
    drop((foreign, scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_deadline_and_cancel_do_not_retract_messages_and_failure_retires_pid() {
    let (vm, tenant, scheduler) = fixture(2, 2);
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, quota(), 8).unwrap();
    let expired = handle
        .request(
            pid,
            &module.handler("Bump").unwrap(),
            &[1],
            32,
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(reply(expired), RequestOutcome::TimedOut);
    let cancelled = handle
        .request(
            pid,
            &module.handler("Bump").unwrap(),
            &[1],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    cancelled.cancel().unwrap();
    assert!(matches!(
        reply(cancelled),
        RequestOutcome::Cancelled | RequestOutcome::Reply(2)
    ));
    let read = handle
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(read), RequestOutcome::Reply(2));
    let unit = handle
        .request(
            pid,
            &module.handler("Ack").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(unit), RequestOutcome::Reply(0));
    let failed = handle
        .request(
            pid,
            &module.handler("FailRequest").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(failed), RequestOutcome::Failed);
    wait_until(|| handle.snapshot(pid).is_err());
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_result_leases_outlive_shutdown_but_handle_cannot_execute() {
    let (vm, tenant, scheduler) = fixture(2, 2);
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, quota(), 8).unwrap();
    let result = handle
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    drop((scheduler, tenant));
    assert!(handle.snapshot(pid).is_err());
    assert!(handle.spawn(&module, quota(), 8).is_err());
    assert!(matches!(
        reply(result),
        RequestOutcome::Failed | RequestOutcome::Reply(0)
    ));
    assert!(vm.used().bytes > 0);
    drop(handle);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_pins_live_stack_bounds_mailbox_and_stops_without_starving_other_actors() {
    let (vm, tenant, scheduler) = fixture(2, 4);
    let handle = scheduler.handle();
    let module = spinning_module();
    let busy = handle.spawn(&module, quota(), 1).unwrap();
    let healthy = handle.spawn(&module, quota(), 8).unwrap();
    assert_eq!(handle.byte_state_snapshot(healthy).unwrap(), None);
    handle
        .send(busy, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| handle.snapshot(busy).unwrap().execution.is_some());
    let owner = handle.snapshot(busy).unwrap().owner.unwrap();
    assert!(handle.byte_state_snapshot(busy).is_err());
    let queued = handle
        .request(
            busy,
            &module.handler("Get").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    let baseline = handle.snapshot(busy).unwrap().resources;
    assert!(
        handle
            .send(busy, &module.handler("Add").unwrap(), &[1], 32)
            .is_err()
    );
    assert_eq!(handle.snapshot(busy).unwrap().resources, baseline);
    for expected in 1..=8 {
        let request = handle
            .request(
                healthy,
                &module.handler("Bump").unwrap(),
                &[1],
                32,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(reply(request), RequestOutcome::Reply(expected));
        let snapshot = handle.snapshot(busy).unwrap();
        assert_eq!(snapshot.owner, Some(owner));
        assert_eq!(snapshot.migrations, 0);
    }
    handle.stop(busy).unwrap();
    assert_eq!(reply(queued), RequestOutcome::Failed);
    wait_until(|| handle.snapshot(busy).is_err());
    handle
        .send(healthy, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| handle.snapshot(healthy).unwrap().owner.is_some());
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_large_mailboxes_bound_queued_messages_and_release_full_queues_on_stop() {
    for capacity in [64, 100, 256] {
        let limits = Resources {
            messages: 512,
            requests: 512,
            ..quota()
        };
        let vm = ResourceAccount::vm(Resources {
            bytes: limits.bytes * 2,
            ..limits
        });
        let tenant = vm.tenant_accounted(limits).unwrap();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                capacity: 2,
                requests: 256,
                slice_steps: 1,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let module = spinning_module();
        let baseline = vm.used();
        for invalid in [0, 257, usize::MAX] {
            assert!(handle.spawn(&module, limits, invalid).is_err());
            assert_eq!(vm.used(), baseline);
        }
        assert!(
            handle
                .spawn(&module, Resources { bytes: 1, ..limits }, capacity)
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        for ancestor in [&vm, &tenant] {
            let blocked = ancestor
                .reserve(Resources {
                    bytes: ancestor.limit().bytes - ancestor.used().bytes,
                    ..Resources::default()
                })
                .unwrap();
            let baseline = vm.used();
            assert!(handle.spawn(&module, limits, capacity).is_err());
            assert_eq!(vm.used(), baseline);
            drop(blocked);
        }
        let busy = handle.spawn(&module, limits, capacity).unwrap();
        let healthy = handle.spawn(&module, limits, 1).unwrap();
        handle
            .send(busy, &module.handler("Spin").unwrap(), &[], u64::MAX)
            .unwrap();
        wait_until(|| handle.snapshot(busy).unwrap().execution.is_some());
        let mut pending = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            pending.push(
                handle
                    .request(
                        busy,
                        &module.handler("Get").unwrap(),
                        &[],
                        32,
                        Duration::from_secs(5),
                    )
                    .unwrap(),
            );
        }
        let snapshot = handle.snapshot(busy).unwrap();
        assert_eq!(snapshot.queued, capacity);
        assert_eq!(snapshot.resources.messages, capacity as u64 + 1);
        assert_eq!(snapshot.resources.requests, capacity as u64);
        let baseline = vm.used();
        assert_eq!(
            handle
                .send(busy, &module.handler("Add").unwrap(), &[1], 32)
                .unwrap_err(),
            "mailbox_full"
        );
        assert_eq!(
            handle
                .request(
                    busy,
                    &module.handler("Get").unwrap(),
                    &[],
                    32,
                    Duration::from_secs(5)
                )
                .err()
                .unwrap(),
            "mailbox_full"
        );
        assert_eq!(vm.used(), baseline);
        if capacity == 256 {
            assert_eq!(
                handle
                    .request(
                        healthy,
                        &module.handler("Get").unwrap(),
                        &[],
                        32,
                        Duration::from_secs(5)
                    )
                    .err()
                    .unwrap(),
                "ingress_requests_full"
            );
            assert_eq!(vm.used(), baseline);
        }
        handle
            .send(healthy, &module.handler("Add").unwrap(), &[7], 32)
            .unwrap();
        wait_until(|| handle.snapshot(healthy).unwrap().committed_state == Some(7));
        handle.stop(busy).unwrap();
        for mut request in pending {
            assert_eq!(
                request.wait(Duration::from_secs(5)).unwrap(),
                Some(RequestOutcome::Failed)
            );
            assert!(request.try_take().is_err());
        }
        wait_until(|| handle.snapshot(busy).is_err());
        assert_eq!(vm.used().requests, 0);
        assert!(
            handle
                .send(busy, &module.handler("Add").unwrap(), &[1], 32)
                .is_err()
        );
        drop((scheduler, handle, module, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn scheduler_start_admission_failure_restores_front_message_and_other_actors_progress() {
    let (vm, tenant, scheduler) = fixture(2, 3);
    let handle = scheduler.handle();
    let module = module();
    let original = handle.spawn(&module, quota(), 1).unwrap();
    let baseline = handle.snapshot(original).unwrap().resources.bytes;
    handle.stop(original).unwrap();
    let mut limited = quota();
    limited.bytes = baseline
        + crate::stack_pool::EXECUTION_STACK_BYTES as u64
        + unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64
        + crate::execution::INVOCATION_OVERHEAD;
    let constrained = handle.spawn(&module, limited, 1).unwrap();
    let queued = handle
        .request(
            constrained,
            &module.handler("Bump").unwrap(),
            &[7],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    let healthy = handle.spawn(&module, quota(), 2).unwrap();
    for expected in 1..=8 {
        let result = handle
            .request(
                healthy,
                &module.handler("Bump").unwrap(),
                &[1],
                32,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(reply(result), RequestOutcome::Reply(expected));
        let snapshot = handle.snapshot(constrained).unwrap();
        assert_eq!(snapshot.committed_state, Some(0));
        assert_eq!(snapshot.resources.messages, 1);
        assert_eq!(snapshot.migrations, 0);
        assert!(
            handle
                .send(constrained, &module.handler("Add").unwrap(), &[1], 32)
                .is_err()
        );
    }
    handle.stop(constrained).unwrap();
    assert_eq!(reply(queued), RequestOutcome::Failed);
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_configuration_and_startup_admission_roll_back() {
    let vm = ResourceAccount::vm(quota());
    let tenant = vm.tenant_accounted(quota()).unwrap();
    let baseline = vm.used();
    for config in [
        SchedulerConfig {
            workers: 0,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            workers: 17,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            capacity: 0,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            capacity: 16385,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            requests: 257,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            slice_steps: 0,
            ..SchedulerConfig::default()
        },
        SchedulerConfig {
            quantum: Duration::ZERO,
            ..SchedulerConfig::default()
        },
    ] {
        assert!(Scheduler::new(&tenant, config).is_err());
        assert_eq!(vm.used(), baseline);
    }
    for bytes in [4 << 20, 9 << 19] {
        let small = vm.tenant_accounted(limits(bytes)).unwrap();
        let baseline = vm.used();
        assert!(
            Scheduler::new(
                &small,
                SchedulerConfig {
                    workers: 2,
                    capacity: 16,
                    requests: 4,
                    ..SchedulerConfig::default()
                }
            )
            .is_err()
        );
        assert_eq!(vm.used(), baseline);
    }
    drop(tenant);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_idle_capacity_does_not_premap_all_execution_stacks() {
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant_accounted(limits(16 << 20)).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 256,
            requests: 4,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    assert!(vm.used().bytes < 12 << 20);
    let handle = scheduler.handle();
    let module = module();
    let pid = handle.spawn(&module, quota(), 1).unwrap();
    assert_eq!(
        reply(
            handle
                .request(
                    pid,
                    &module.handler("Bump").unwrap(),
                    &[7],
                    32,
                    Duration::from_secs(5)
                )
                .unwrap()
        ),
        RequestOutcome::Reply(7)
    );
    assert!(vm.used().bytes < 12 << 20);
    drop((handle, scheduler, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_bool_replies_and_initializer_failure_preserve_budget() {
    let (vm, tenant, scheduler) = fixture(2, 2);
    let handle = scheduler.handle();
    let mut failed = module();
    Arc::get_mut(&mut failed.inner).unwrap().initializer = fail;
    let baseline = vm.used();
    assert!(handle.spawn(&failed, quota(), 1).is_err());
    assert_eq!(vm.used(), baseline);
    let mut module = module();
    Arc::get_mut(&mut module.inner).unwrap().manifest.handlers[1].reply = Some(ValueType::Bool);
    let pid = handle.spawn(&module, quota(), 2).unwrap();
    let result = handle
        .request(
            pid,
            &module.handler("Bump").unwrap(),
            &[1],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(result), RequestOutcome::Reply(1));
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_language_request_deadlines_and_cancellation_keep_terminal_outcome() {
    let (vm, tenant, scheduler) = fixture(2, 4);
    let handle = scheduler.handle();
    let mut module = super::request_events::fixture();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers.push(Handler {
        name: "Spin".into(),
        parameters: vec![],
        reply: None,
        completion: None,
    });
    let mut entries = data.entries.to_vec();
    entries.push(spin);
    data.entries = entries.into_boxed_slice();
    let owner = handle.spawn(&module, quota(), 8).unwrap();
    let target = handle.spawn(&module, quota(), 8).unwrap();
    let capability = match reply(
        handle
            .request(
                target,
                &module.handler("Fail").unwrap(),
                &[],
                128,
                Duration::from_secs(5),
            )
            .unwrap(),
    ) {
        RequestOutcome::Reply(value) => value,
        outcome => panic!("unexpected capability {outcome:?}"),
    };
    handle
        .send(target, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| {
        handle
            .snapshot(target)
            .unwrap()
            .execution
            .is_some_and(|execution| execution.max_steps == u64::MAX)
    });
    for (timeout, mode, expected) in [(0, 0, 100), (60000, 1, 200), (60000, 0, 300)] {
        let begun = handle
            .request(
                owner,
                &module.handler("Ack").unwrap(),
                &[capability, timeout, mode],
                128,
                Duration::from_secs(5),
            )
            .unwrap();
        let outcome = reply(begun);
        assert!(
            matches!(outcome, RequestOutcome::Reply(value) if value > 0 || (mode == 1 && value == 0)),
            "unexpected begin outcome: {outcome:?}"
        );
        if expected == 300 {
            handle.stop(target).unwrap();
        }
        wait_until(|| handle.snapshot(owner).unwrap().committed_state == Some(expected));
    }
    let baseline = handle.snapshot(owner).unwrap().resources;
    assert_eq!(baseline.requests, 0);
    assert_eq!(baseline.messages, 0);
    drop((scheduler, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_exit_diagnostics_keep_old_state_and_survive_cleanup_without_business_leases() {
    let (vm, tenant, scheduler) = fixture(2, 4);
    let handle = scheduler.handle();
    let module = module();
    assert_eq!(handle.last_exit(), None);
    let first = handle.spawn(&module, quota(), 8).unwrap();
    let result = handle
        .request(
            first,
            &module.handler("Bump").unwrap(),
            &[7],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(result), RequestOutcome::Reply(7));
    let previous_worker = handle.snapshot(first).unwrap().last_worker.unwrap();
    let failed = handle
        .request(
            first,
            &module.handler("FailRequest").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(failed), RequestOutcome::Failed);
    let diagnostic = handle.last_exit().unwrap();
    assert_eq!(diagnostic.pid, first);
    assert_eq!(diagnostic.worker, (previous_worker + 1) % 2);
    assert_eq!(diagnostic.handler(), "FailRequest");
    assert_eq!(
        diagnostic.reason,
        SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
    );
    assert_eq!(diagnostic.committed_state, Some(7));
    assert!(diagnostic.resources_before_cleanup.bytes > 0);
    assert_eq!(
        diagnostic.execution.unwrap().fault,
        Some(ExecutionFault::DivisionByZero)
    );
    assert!(handle.snapshot(first).is_err());
    assert_eq!(handle.last_exit(), Some(diagnostic));
    let second = handle.spawn(&module, quota(), 8).unwrap();
    let result = handle
        .request(
            second,
            &module.handler("Bump").unwrap(),
            &[3],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(result), RequestOutcome::Reply(3));
    assert_eq!(handle.last_exit(), Some(diagnostic));
    let budget = handle
        .request(
            second,
            &module.handler("Bump").unwrap(),
            &[9],
            2,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(budget), RequestOutcome::Failed);
    let budget_exit = handle.last_exit().unwrap();
    assert_eq!(budget_exit.pid, second);
    assert_eq!(
        budget_exit.reason,
        SchedulerExitReason::Managed(ExecutionFault::BudgetExhausted)
    );
    assert_eq!(budget_exit.committed_state, Some(3));
    assert_eq!(budget_exit.execution.unwrap().remaining_steps, 0);
    let stopped = handle.spawn(&module, quota(), 8).unwrap();
    handle.stop(stopped).unwrap();
    assert_eq!(handle.last_exit(), Some(budget_exit));
    drop((scheduler, tenant));
    let baseline = vm.used();
    assert!(baseline.bytes > 0);
    assert_eq!(baseline.processes, 0);
    assert_eq!(baseline.messages, 0);
    assert_eq!(baseline.requests, 0);
    for _ in 0..2 {
        assert_eq!(handle.last_exit(), Some(budget_exit));
        assert_eq!(vm.used(), baseline);
    }
    drop(handle);
    assert_eq!(vm.used(), Resources::default());
    assert_eq!(diagnostic.handler(), "FailRequest");
}

#[test]
fn scheduler_reference_exit_does_not_expose_heap_handle_and_active_stop_preserves_failure() {
    let (vm, tenant, scheduler) = fixture(2, 3);
    let handle = scheduler.handle();
    let mut module = spinning_module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::Ref;
    data.initializer = super::persistent_state::record_initializer;
    let pid = handle.spawn_with_heap(&module, quota(), 4, 8).unwrap();
    assert_eq!(handle.byte_state_snapshot(pid).unwrap(), None);
    let failed = handle
        .request(
            pid,
            &module.handler("FailRequest").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(failed), RequestOutcome::Failed);
    let diagnostic = handle.last_exit().unwrap();
    assert_eq!(diagnostic.state_type, ValueType::Ref);
    assert_eq!(diagnostic.committed_state, None);
    assert_eq!(diagnostic.byte_state, None);
    assert!(diagnostic.execution.unwrap().heap.is_some());
    let busy = handle.spawn_with_heap(&module, quota(), 4, 8).unwrap();
    handle
        .send(busy, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| handle.snapshot(busy).unwrap().execution.is_some());
    handle.stop(busy).unwrap();
    wait_until(|| handle.snapshot(busy).is_err());
    assert_eq!(handle.last_exit(), Some(diagnostic));
    let busy = handle.spawn_with_heap(&module, quota(), 4, 8).unwrap();
    handle
        .send(busy, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| handle.snapshot(busy).unwrap().execution.is_some());
    drop((scheduler, tenant));
    assert_eq!(handle.last_exit(), Some(diagnostic));
    drop(handle);
    assert_eq!(vm.used(), Resources::default());
}

fn supervisor_fixture(capacity: usize) -> (ResourceAccount, ResourceAccount, SchedulerSupervisor) {
    let vm = ResourceAccount::vm(quota());
    let tenant = vm.tenant_accounted(quota()).unwrap();
    let supervisor = SchedulerSupervisor::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity,
            requests: capacity.max(4),
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    (vm, tenant, supervisor)
}

#[test]
fn scheduler_shared_reply_capacity_spans_actors_and_preserves_mixed_types_after_close() {
    let vm = ResourceAccount::vm(quota());
    let tenant = vm.tenant_accounted(quota()).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 3,
            requests: 3,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let integers = module();
    let mut booleans = module();
    Arc::get_mut(&mut booleans.inner).unwrap().manifest.handlers[1].reply = Some(ValueType::Bool);
    let first = handle.spawn(&integers, quota(), 4).unwrap();
    let second = handle.spawn(&booleans, quota(), 4).unwrap();
    let unused = handle.spawn(&integers, quota(), 4).unwrap();
    let mut int_result = handle
        .request(
            first,
            &integers.handler("Bump").unwrap(),
            &[7],
            32,
            Duration::from_secs(60),
        )
        .unwrap();
    let mut bool_result = handle
        .request(
            second,
            &booleans.handler("Bump").unwrap(),
            &[1],
            32,
            Duration::from_secs(60),
        )
        .unwrap();
    let mut unit_result = handle
        .request(
            second,
            &booleans.handler("Ack").unwrap(),
            &[],
            32,
            Duration::from_secs(60),
        )
        .unwrap();
    wait_until(|| {
        handle.snapshot(first).unwrap().committed_state == Some(7)
            && handle.snapshot(second).unwrap().committed_state == Some(1)
            && handle.snapshot(second).unwrap().owner.is_none()
            && handle.snapshot(second).unwrap().queued == 0
    });
    assert_eq!(int_result.reply_type(), ValueType::Int);
    assert_eq!(bool_result.reply_type(), ValueType::Bool);
    assert_eq!(unit_result.reply_type(), ValueType::Unit);
    assert_eq!(handle.snapshot(first).unwrap().resources.requests, 1);
    assert_eq!(handle.snapshot(second).unwrap().resources.requests, 2);
    assert_eq!(handle.snapshot(unused).unwrap().resources.requests, 0);
    let baseline = vm.used();
    assert_eq!(
        handle
            .request(
                unused,
                &integers.handler("Get").unwrap(),
                &[],
                32,
                Duration::from_secs(60)
            )
            .err()
            .unwrap(),
        "ingress_requests_full"
    );
    assert_eq!(vm.used(), baseline);
    assert_eq!(
        int_result.try_take().unwrap(),
        Some(RequestOutcome::Reply(7))
    );
    let replacement = handle
        .request(
            unused,
            &integers.handler("Bump").unwrap(),
            &[9],
            32,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(reply(replacement), RequestOutcome::Reply(9));
    drop((scheduler, handle, integers, booleans, tenant));
    assert_eq!(vm.used().requests, 2);
    assert_eq!(vm.used().processes, 0);
    assert_eq!(
        bool_result.try_take().unwrap(),
        Some(RequestOutcome::Reply(1))
    );
    assert_eq!(
        unit_result.try_take().unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    assert_eq!(vm.used().requests, 0);
    assert!(vm.used().bytes > 0);
    drop((int_result, bool_result, unit_result));
    assert_eq!(vm.used(), Resources::default());
}

fn supervisor_event(supervisor: &mut SchedulerSupervisor) -> SchedulerSupervisorEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = supervisor.poll() {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "supervision termination deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scheduler_supervision_reuses_indexed_slots_and_rejects_foreign_or_stale_children() {
    let (vm, tenant, mut supervisor) = supervisor_fixture(4);
    let mut foreign = SchedulerSupervisor::new(
        &tenant,
        SchedulerConfig {
            capacity: 1,
            requests: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let module = module();
    let foreign_child = foreign
        .spawn(&module, quota(), 1, RestartPolicy::Never)
        .unwrap();
    assert!(supervisor.current_pid(foreign_child).is_err());
    assert!(supervisor.stop(foreign_child).is_err());
    let mut previous = Vec::new();
    for _ in 0..8 {
        let children: Vec<_> = (0..4)
            .map(|_| {
                supervisor
                    .spawn(&module, quota(), 1, RestartPolicy::Never)
                    .unwrap()
            })
            .collect();
        let pids: Vec<_> = children
            .iter()
            .map(|child| supervisor.current_pid(*child).unwrap())
            .collect();
        for child in &previous {
            assert!(supervisor.current_pid(*child).is_err());
            assert!(supervisor.stop(*child).is_err());
        }
        let baseline = vm.used();
        assert!(
            supervisor
                .spawn(&module, quota(), 1, RestartPolicy::Never)
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        for index in [2, 0, 3, 1] {
            assert_eq!(
                supervisor.current_pid(children[index]).unwrap(),
                pids[index]
            );
            supervisor.stop(children[index]).unwrap();
            assert!(supervisor.current_pid(children[index]).is_err());
        }
        let baseline = vm.used();
        assert!(
            supervisor
                .spawn(&module, limits(0), 1, RestartPolicy::Never)
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        previous.extend(children);
    }
    assert!(supervisor.poll().is_none());
    assert!(foreign.current_pid(previous[0]).is_err());
    assert!(foreign.stop(previous[0]).is_err());
    foreign.stop(foreign_child).unwrap();
    drop((supervisor, foreign, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_supervision_restarts_fresh_state_and_enforces_finite_intensity() {
    let (vm, tenant, mut supervisor) = supervisor_fixture(2);
    let handle = supervisor.handle();
    let module = module();
    let child = supervisor
        .spawn(
            &module,
            quota(),
            4,
            RestartPolicy::OnFailure {
                max_restarts: 2,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    for attempt in 0..3 {
        let pid = supervisor.current_pid(child).unwrap();
        assert_eq!(
            reply(
                handle
                    .request(
                        pid,
                        &module.handler("Bump").unwrap(),
                        &[7],
                        32,
                        Duration::from_secs(5)
                    )
                    .unwrap()
            ),
            RequestOutcome::Reply(7)
        );
        let result = handle
            .request(
                pid,
                &module.handler("FailRequest").unwrap(),
                &[],
                32,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(reply(result), RequestOutcome::Failed);
        let event = supervisor_event(&mut supervisor);
        assert_eq!(event.child, child);
        assert_eq!(event.pid, pid);
        let SchedulerTermination::Failed(exit) = event.termination else {
            panic!("missing failed termination")
        };
        assert_eq!(exit.committed_state, Some(7));
        assert_eq!(
            exit.reason,
            SchedulerExitReason::Managed(ExecutionFault::DivisionByZero)
        );
        assert!(handle.snapshot(pid).is_err());
        if attempt < 2 {
            let SchedulerRestartStatus::Restarted(replacement) = event.restart else {
                panic!("missing replacement")
            };
            assert_ne!(replacement, pid);
            assert_eq!(supervisor.current_pid(child).unwrap(), replacement);
            assert_eq!(
                handle.snapshot(replacement).unwrap().committed_state,
                Some(0)
            );
            assert_eq!(handle.last_exit(), Some(exit));
        } else {
            assert_eq!(event.restart, SchedulerRestartStatus::IntensityExceeded);
            assert!(supervisor.current_pid(child).is_err());
        }
    }
    assert!(supervisor.poll().is_none());
    drop((supervisor, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_supervision_keeps_each_concurrent_exit_despite_overwritten_last_failure() {
    let (vm, tenant, mut supervisor) = supervisor_fixture(8);
    let handle = supervisor.handle();
    let module = module();
    let mut children = Vec::new();
    let mut results = Vec::new();
    for _ in 0..8 {
        let child = supervisor
            .spawn(&module, quota(), 2, RestartPolicy::Never)
            .unwrap();
        let pid = supervisor.current_pid(child).unwrap();
        results.push(
            handle
                .request(
                    pid,
                    &module.handler("FailRequest").unwrap(),
                    &[],
                    32,
                    Duration::from_secs(5),
                )
                .unwrap(),
        );
        children.push(child);
    }
    for result in results {
        assert_eq!(reply(result), RequestOutcome::Failed);
    }
    for _ in 0..8 {
        let event = supervisor_event(&mut supervisor);
        assert_eq!(event.restart, SchedulerRestartStatus::Disabled);
        assert!(matches!(event.termination, SchedulerTermination::Failed(_)));
        let index = children
            .iter()
            .position(|child| *child == event.child)
            .unwrap();
        children.remove(index);
    }
    assert!(children.is_empty());
    assert!(supervisor.poll().is_none());
    drop((supervisor, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn scheduler_supervision_stops_and_restart_admission_failures_do_not_retry() {
    let (vm, tenant, mut supervisor) = supervisor_fixture(3);
    let handle = supervisor.handle();
    let module = spinning_module();
    let policy = RestartPolicy::OnFailure {
        max_restarts: 1,
        within: Duration::from_secs(60),
    };
    let child = supervisor.spawn(&module, quota(), 4, policy).unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    handle
        .send(pid, &module.handler("Spin").unwrap(), &[], u64::MAX)
        .unwrap();
    wait_until(|| handle.snapshot(pid).unwrap().execution.is_some());
    handle.stop(pid).unwrap();
    assert_eq!(
        supervisor_event(&mut supervisor).restart,
        SchedulerRestartStatus::Stopped
    );
    assert!(supervisor.current_pid(child).is_err());
    let child = supervisor.spawn(&module, quota(), 4, policy).unwrap();
    supervisor.stop(child).unwrap();
    assert!(supervisor.current_pid(child).is_err());
    assert!(supervisor.poll().is_none());
    let child = supervisor.spawn(&module, quota(), 4, policy).unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let result = handle
        .request(
            pid,
            &module.handler("FailRequest").unwrap(),
            &[],
            32,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply(result), RequestOutcome::Failed);
    wait_until(|| vm.used().processes == 0);
    let remaining = vm.limit().bytes - vm.used().bytes;
    let blocked = vm
        .reserve(Resources {
            bytes: remaining,
            ..Resources::default()
        })
        .unwrap();
    let event = supervisor_event(&mut supervisor);
    assert!(matches!(event.restart, SchedulerRestartStatus::Failed(_)));
    drop(blocked);
    assert!(supervisor.current_pid(child).is_err());
    assert!(supervisor.poll().is_none());
    let replacement = supervisor
        .spawn(&module, quota(), 1, RestartPolicy::Never)
        .unwrap();
    assert!(supervisor.current_pid(child).is_err());
    supervisor.stop(replacement).unwrap();
    drop((supervisor, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}

static SUPERVISOR_INIT_FAILS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn supervised_initializer(
    state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    if SUPERVISOR_INIT_FAILS.load(Ordering::SeqCst) {
        STATUS_DIVISION_BY_ZERO
    } else {
        unsafe { initialize(state, arguments, output, context) }
    }
}

#[test]
fn scheduler_supervision_initializer_failure_and_spawn_admission_release_tickets() {
    let (vm, tenant, mut supervisor) = supervisor_fixture(2);
    let handle = supervisor.handle();
    let mut module = module();
    Arc::get_mut(&mut module.inner).unwrap().initializer = supervised_initializer;
    let baseline = vm.used();
    assert!(
        supervisor
            .spawn(&module, limits(0), 2, RestartPolicy::Never)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    assert!(
        supervisor
            .spawn(
                &module,
                quota(),
                2,
                RestartPolicy::OnFailure {
                    max_restarts: 0,
                    within: Duration::from_secs(1)
                }
            )
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    let child = supervisor
        .spawn(
            &module,
            quota(),
            2,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    SUPERVISOR_INIT_FAILS.store(true, Ordering::SeqCst);
    assert_eq!(
        reply(
            handle
                .request(
                    pid,
                    &module.handler("FailRequest").unwrap(),
                    &[],
                    32,
                    Duration::from_secs(5)
                )
                .unwrap()
        ),
        RequestOutcome::Failed
    );
    let event = supervisor_event(&mut supervisor);
    SUPERVISOR_INIT_FAILS.store(false, Ordering::SeqCst);
    assert!(
        matches!(event.restart, SchedulerRestartStatus::Failed(error) if error.contains("division by zero"))
    );
    assert!(supervisor.current_pid(child).is_err());
    assert!(supervisor.poll().is_none());
    let replacement = supervisor
        .spawn(&module, quota(), 1, RestartPolicy::Never)
        .unwrap();
    supervisor.stop(replacement).unwrap();
    drop((supervisor, handle, tenant));
    assert_eq!(vm.used(), Resources::default());
}
