use super::*;

unsafe extern "C" fn byte_initialize(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    unsafe { byte_candidate(0, [5].as_ptr(), output, context) }
}

unsafe extern "C" fn byte_candidate(
    _state: i64,
    arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let value = unsafe { *arguments };
    let scratch = [40, value, 0];
    let mut reference = 0;
    let Some(call) = (unsafe { (*context).heap_call }) else {
        return STATUS_INVALID_CONTROL;
    };
    let status = unsafe {
        call(
            context,
            wings_abi::HEAP_FILL_BYTES,
            scratch.as_ptr() as usize as u64,
            &mut reference,
        )
    };
    if status != STATUS_OK {
        return status;
    }
    if value == 99 {
        return STATUS_DIVISION_BY_ZERO;
    }
    unsafe {
        *output = ScalarOutput {
            state: reference as i64,
            reply: value,
        }
    };
    STATUS_OK
}

#[test]
fn byte_state_diagnostics_copy_only_committed_data_without_resource_leases() {
    let [vm, tenant, _] = accounts();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::BytesRef;
    data.initializer = byte_initialize;
    data.entries[1] = byte_candidate;
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker
        .spawn_with_heap(&module, limits(8 << 20), 2, 4)
        .unwrap();
    let baseline = vm.used();
    let first = worker.byte_state_snapshot(pid).unwrap().unwrap();
    assert_eq!(
        first,
        ByteStateSnapshot {
            length: 40,
            prefix: [5; 32]
        }
    );
    assert_eq!(vm.used(), baseline);
    let handler = module.handler("Bump").unwrap();
    for value in [7, 99] {
        let request = worker
            .request(pid, &handler, &[value], 128, Duration::from_secs(60))
            .unwrap();
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
        assert!(worker.byte_state_snapshot(pid).is_err());
        let event = worker.tick(128).unwrap();
        if value == 7 {
            assert_eq!(event, WorkerEvent::Delivered(pid));
            assert_eq!(
                worker.take_reply(request).unwrap(),
                Some(RequestOutcome::Reply(7))
            );
            assert_eq!(
                worker.byte_state_snapshot(pid).unwrap().unwrap().prefix,
                [7; 32]
            );
        } else {
            assert!(matches!(event, WorkerEvent::Exited { pid: actual, .. } if actual == pid));
            assert_eq!(
                worker.take_reply(request).unwrap(),
                Some(RequestOutcome::Failed)
            );
        }
    }
    let exit = *worker.last_exit().unwrap();
    assert_eq!(
        exit.byte_state,
        Some(ByteStateSnapshot {
            length: 40,
            prefix: [7; 32]
        })
    );
    assert_eq!(exit.committed_state, None);
    assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    assert_eq!(first.prefix, [5; 32]);
    assert!(worker.byte_state_snapshot(pid).is_err());
    drop((worker, module, handler, tenant));
    assert_eq!(vm.used(), Resources::default());
    assert_eq!(exit.byte_state.unwrap().prefix, [7; 32]);
}

#[test]
fn scheduler_byte_state_snapshots_preserve_committed_data_without_leases() {
    let [vm, tenant, account] = accounts();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::BytesRef;
    data.initializer = byte_initialize;
    data.entries[1] = byte_candidate;
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            capacity: 2,
            requests: 2,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let pid = handle
        .spawn_with_heap(&module, limits(8 << 20), 2, 4)
        .unwrap();
    let baseline = vm.used();
    let first = handle.byte_state_snapshot(pid).unwrap().unwrap();
    assert_eq!(
        first,
        ByteStateSnapshot {
            length: 40,
            prefix: [5; 32]
        }
    );
    assert_eq!(vm.used(), baseline);
    let handler = module.handler("Bump").unwrap();
    for value in [7, 99] {
        let mut request = handle
            .request(pid, &handler, &[value], 128, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            request.wait(Duration::from_secs(5)).unwrap(),
            Some(if value == 7 {
                RequestOutcome::Reply(7)
            } else {
                RequestOutcome::Failed
            })
        );
        if value == 7 {
            assert_eq!(
                handle.byte_state_snapshot(pid).unwrap().unwrap().prefix,
                [7; 32]
            );
        }
    }
    let exit = handle.last_exit().unwrap();
    assert_eq!(
        exit.byte_state,
        Some(ByteStateSnapshot {
            length: 40,
            prefix: [7; 32]
        })
    );
    assert_eq!(exit.committed_state, None);
    assert_eq!(
        exit.execution.unwrap().fault,
        Some(ExecutionFault::DivisionByZero)
    );
    assert!(handle.byte_state_snapshot(pid).is_err());
    let replacement = handle
        .spawn_with_heap(&module, limits(8 << 20), 2, 4)
        .unwrap();
    assert_ne!(pid, replacement);
    assert!(handle.byte_state_snapshot(pid).is_err());
    assert_eq!(
        handle.byte_state_snapshot(replacement).unwrap(),
        Some(first)
    );
    drop(scheduler);
    assert!(handle.byte_state_snapshot(replacement).is_err());
    drop((handle, module, handler, account, tenant));
    assert_eq!(vm.used(), Resources::default());
    assert_eq!(first.prefix, [5; 32]);
    assert_eq!(exit.byte_state.unwrap().prefix, [7; 32]);
}

#[test]
fn byte_state_diagnostics_reject_other_types_and_zero_pad_short_buffers() {
    let [vm, tenant, account] = accounts();
    let module = module();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn(&module, limits(8 << 20), 1).unwrap();
    assert_eq!(worker.byte_state_snapshot(pid).unwrap(), None);
    worker.stop(pid).unwrap();
    let mut heap = crate::aot_heap::AotHeap::new(&account, 2).unwrap();
    assert_eq!(heap.byte_state_snapshot(), None);
    let flag = std::sync::atomic::AtomicU32::new(0);
    let mut context = ScalarContext {
        cancellation: &flag,
        remaining_steps: 128,
        yield_request: &flag,
        slice_remaining: 128,
        slow_path: None,
        data: std::ptr::null_mut(),
        stack_limit: 0,
        roots: std::ptr::null(),
        heap_call: Some(crate::aot_heap::heap_call),
        heap_data: std::ptr::from_mut(heap.as_mut()).cast(),
        effect_call: None,
    };
    for length in [0i64, 3] {
        let scratch = [length, 255, 0];
        let mut reference = 0;
        assert_eq!(
            unsafe {
                crate::aot_heap::heap_call(
                    &mut context,
                    wings_abi::HEAP_FILL_BYTES,
                    scratch.as_ptr() as usize as u64,
                    &mut reference,
                )
            },
            STATUS_OK
        );
        heap.commit_typed(ValueType::BytesRef, reference as i64)
            .unwrap();
        let snapshot = heap.byte_state_snapshot().unwrap();
        assert_eq!(snapshot.length, length as usize);
        assert_eq!(
            &snapshot.prefix[..length as usize],
            vec![255; length as usize]
        );
        assert!(
            snapshot.prefix[length as usize..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }
    drop((heap, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn execution_snapshots_track_ready_suspended_and_completed_without_advancing() {
    let module = module();
    let mut process = module.instantiate().unwrap();
    let mut execution = process
        .start_controlled(
            "Bump",
            &[7],
            8,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    let ready = execution.snapshot();
    assert_eq!(ready.phase, ExecutionPhase::Ready);
    assert_eq!(ready.remaining_steps, 8);
    assert_eq!(ready.consumed_steps(), 0);
    assert_eq!(ready.heap, None);
    assert_eq!(ready.fault, None);
    assert!(execution.resume(0).is_err());
    assert_eq!(execution.snapshot(), ready);
    for consumed in 1..4 {
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        let snapshot = execution.snapshot();
        assert_eq!(snapshot.phase, ExecutionPhase::Suspended);
        assert_eq!(snapshot.fault, None);
        assert_eq!(snapshot.max_steps, 8);
        assert_eq!(snapshot.consumed_steps(), consumed);
        assert_eq!(execution.snapshot(), snapshot);
    }
    assert_eq!(
        execution.resume(1).unwrap(),
        ExecutionStatus::Complete(Some(7))
    );
    let completed = execution.snapshot();
    assert_eq!(completed.phase, ExecutionPhase::Complete);
    assert_eq!(completed.fault, None);
    assert_eq!(completed.consumed_steps(), 4);
    assert!(execution.resume(1).is_err());
    assert_eq!(execution.snapshot(), completed);
    drop(execution);
    assert_eq!(process.state, 7);
}

#[test]
fn task_snapshots_preserve_last_result_until_successful_start_and_report_failures() {
    let module = module();
    let mut task = ScalarTask::new(module.instantiate().unwrap());
    let pool = StackPool::new(1, 0).unwrap();
    assert_eq!(task.snapshot(), None);
    assert_eq!(task.heap_stats(), None);
    for (budget, cancel) in [(2, false), (8, true)] {
        let mut task = ScalarTask::new(module.instantiate().unwrap());
        let cancellation = CancellationToken::default();
        task.start(
            &module.handler("Bump").unwrap(),
            &[7],
            budget,
            &cancellation,
            &PreemptionToken::default(),
            &pool,
        )
        .unwrap();
        assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Suspended);
        if cancel {
            cancellation.cancel();
        }
        assert!(task.resume(8).is_err());
        let failed = task.snapshot().unwrap();
        assert_eq!(failed.phase, ExecutionPhase::Failed);
        assert_eq!(
            failed.fault,
            Some(if cancel {
                ExecutionFault::Cancelled
            } else {
                ExecutionFault::BudgetExhausted
            })
        );
        assert_eq!(failed.consumed_steps(), if cancel { 1 } else { 2 });
        assert_eq!(task.committed_state(), 0);
        assert!(task.resume(1).is_err());
        assert_eq!(task.snapshot(), Some(failed));
    }
    let handler = module.handler("Get").unwrap();
    task.start(
        &handler,
        &[],
        8,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        &pool,
    )
    .unwrap();
    assert_eq!(task.resume(8).unwrap(), ExecutionStatus::Complete(Some(0)));
    let completed = task.snapshot();
    assert!(
        task.start(
            &handler,
            &[1],
            8,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool,
        )
        .is_err()
    );
    assert_eq!(task.snapshot(), completed);
    task.start(
        &handler,
        &[],
        3,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        &pool,
    )
    .unwrap();
    assert_eq!(task.snapshot().unwrap().phase, ExecutionPhase::Ready);
    assert_eq!(task.snapshot().unwrap().max_steps, 3);
}

#[test]
fn failed_output_validation_marks_snapshot_failed() {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::Bool;
    data.entries[1] = invalid_output;
    let mut process = module.instantiate().unwrap();
    let mut execution = process
        .start_controlled(
            "Bump",
            &[7],
            8,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    assert!(execution.resume(8).is_err());
    assert_eq!(execution.snapshot().phase, ExecutionPhase::Failed);
    assert_eq!(
        execution.snapshot().fault,
        Some(ExecutionFault::InvalidOutput)
    );
    assert_eq!(execution.snapshot().remaining_steps, 8);
    drop(execution);
    assert_eq!(process.state, 0);
}

#[test]
fn wrong_record_shape_cannot_commit_as_list_state() {
    let [vm, tenant, account] = accounts();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::ListRef;
    data.initializer = super::persistent_state::record_initializer;
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    assert!(
        module
            .instantiate_with_heap(&bound, &account, 4)
            .err()
            .unwrap()
            .contains("IntList node")
    );
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_snapshot_reports_queue_current_handler_committed_state_and_identity() {
    let (vm, tenant, mut worker, module, pid) = worker_fixture(1, 1, 2);
    let initial = worker.snapshot(pid).unwrap();
    assert_eq!(initial.phase, ProcessPhase::Idle);
    assert_eq!(initial.queued, 0);
    assert_eq!(initial.mailbox_capacity, 2);
    assert_eq!(initial.active_handler, None);
    assert_eq!(initial.execution, None);
    assert_eq!(initial.state_type, ValueType::Int);
    assert_eq!(initial.committed_state, Some(0));
    assert_eq!(initial.heap, None);
    assert_eq!(initial.resources, worker.resources(pid).unwrap());
    worker
        .send(pid, &module.handler("Add").unwrap(), &[7], 8)
        .unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[9], 8)
        .unwrap();
    assert_eq!(worker.snapshot(pid).unwrap().phase, ProcessPhase::Runnable);
    assert_eq!(worker.snapshot(pid).unwrap().queued, 2);
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    let used = vm.used();
    let suspended = worker.snapshot(pid).unwrap();
    assert_eq!(suspended.phase, ProcessPhase::Suspended);
    assert_eq!(suspended.active_handler, Some("Add"));
    assert_eq!(suspended.queued, 1);
    assert_eq!(suspended.committed_state, Some(0));
    assert_eq!(suspended.execution.unwrap().consumed_steps(), 1);
    for _ in 0..100 {
        assert_eq!(worker.snapshot(pid).unwrap(), suspended);
    }
    assert_eq!(vm.used(), used);
    assert_eq!(worker.tick(8).unwrap(), WorkerEvent::Delivered(pid));
    let runnable = worker.snapshot(pid).unwrap();
    assert_eq!(runnable.phase, ProcessPhase::Runnable);
    assert_eq!(runnable.active_handler, None);
    assert_eq!(runnable.committed_state, Some(7));
    assert_eq!(runnable.execution.unwrap().phase, ExecutionPhase::Complete);
    assert_eq!(worker.tick(8).unwrap(), WorkerEvent::Delivered(pid));
    assert_eq!(worker.snapshot(pid).unwrap().phase, ProcessPhase::Idle);
    assert_eq!(worker.snapshot(pid).unwrap().committed_state, Some(16));
    let other = ScalarWorker::new(&tenant, 1).unwrap();
    assert!(other.snapshot(pid).is_err());
    worker.stop(pid).unwrap();
    assert!(worker.snapshot(pid).is_err());
    let replacement = worker.spawn(&module, limits(4 << 20), 2).unwrap();
    assert!(worker.snapshot(pid).is_err());
    assert_eq!(worker.snapshot(replacement).unwrap().execution, None);
    drop((other, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn supervised_record_snapshot_hides_reference_and_resets_on_restart() {
    let [vm, tenant, _] = accounts();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::Ref;
    data.initializer = super::persistent_state::record_initializer;
    let mut supervisor = ScalarSupervisor::new(&tenant, 1, 1).unwrap();
    let child = supervisor
        .spawn_with_heap(
            &module,
            limits(4 << 20),
            2,
            2,
            RestartPolicy::OnFailure {
                max_restarts: 1,
                within: Duration::from_secs(60),
            },
        )
        .unwrap();
    let pid = supervisor.current_pid(child).unwrap();
    let snapshot = supervisor.snapshot(pid).unwrap();
    assert_eq!(snapshot.state_type, ValueType::Ref);
    assert_eq!(snapshot.committed_state, None);
    assert_eq!(snapshot.heap.unwrap().capacity, 2);
    assert_eq!(snapshot.heap.unwrap().allocated, 1);
    let used = vm.used();
    assert_eq!(supervisor.snapshot(pid).unwrap(), snapshot);
    assert_eq!(vm.used(), used);
    let initial_heap = snapshot.heap;
    supervisor
        .send(pid, &module.handler("Fail").unwrap(), &[], 8)
        .unwrap();
    assert!(matches!(
        supervisor.tick(8).unwrap(),
        SupervisorEvent::Exited {
            restart: RestartStatus::Restarted(_),
            ..
        }
    ));
    assert!(supervisor.snapshot(pid).is_err());
    let exit = *supervisor.last_exit().unwrap();
    assert_eq!(exit.pid, pid);
    assert_eq!(exit.handler(), "Fail");
    assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    assert_eq!(exit.committed_state, None);
    assert_eq!(exit.state_type, ValueType::Ref);
    assert_eq!(exit.execution.heap, initial_heap);
    let replacement = supervisor.current_pid(child).unwrap();
    assert_eq!(supervisor.snapshot(replacement).unwrap().execution, None);
    assert_eq!(supervisor.snapshot(replacement).unwrap().heap, initial_heap);
    supervisor.stop(child).unwrap();
    assert_eq!(supervisor.last_exit(), Some(&exit));
    drop(supervisor);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_retains_one_owned_exit_without_retaining_failed_actor_resources() {
    let [vm, tenant, _] = accounts();
    let module = module();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let baseline = vm.used();
    assert_eq!(worker.last_exit(), None);
    let pid = worker.spawn(&module, limits(4 << 20), 2).unwrap();
    worker
        .send(pid, &module.handler("Fail").unwrap(), &[], 8)
        .unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[99], 8)
        .unwrap();
    assert!(
        matches!(worker.tick(8).unwrap(), WorkerEvent::Exited { pid: exited, .. } if exited == pid)
    );
    assert_eq!(vm.used().processes, 0);
    assert_eq!(vm.used().messages, 0);
    let exit = *worker.last_exit().unwrap();
    assert_eq!(exit.pid, pid);
    assert_eq!(exit.handler(), "Fail");
    assert_eq!(exit.queued, 1);
    assert_eq!(exit.committed_state, Some(0));
    assert_eq!(exit.execution.fault, Some(ExecutionFault::DivisionByZero));
    assert_eq!(exit.execution.remaining_steps, 8);
    assert_eq!(exit.resources_before_cleanup.processes, 1);
    assert_eq!(exit.resources_before_cleanup.messages, 2);
    assert!(worker.snapshot(pid).is_err());
    let used = vm.used();
    assert_eq!(worker.last_exit(), Some(&exit));
    assert_eq!(vm.used(), used);
    let next = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    worker
        .send(next, &module.handler("Add").unwrap(), &[7], 8)
        .unwrap();
    assert_eq!(worker.tick(8).unwrap(), WorkerEvent::Delivered(next));
    assert_eq!(worker.last_exit(), Some(&exit));
    worker
        .send(next, &module.handler("Add").unwrap(), &[1], 1)
        .unwrap();
    assert!(matches!(
        worker.tick(8).unwrap(),
        WorkerEvent::Exited { .. }
    ));
    let latest = *worker.last_exit().unwrap();
    assert_eq!(latest.pid, next);
    assert_eq!(latest.handler(), "Add");
    assert_eq!(latest.committed_state, Some(7));
    assert_eq!(
        latest.execution.fault,
        Some(ExecutionFault::BudgetExhausted)
    );
    assert_eq!(latest.execution.remaining_steps, 0);
    let next = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    worker.stop(next).unwrap();
    assert_eq!(worker.last_exit(), Some(&latest));
    assert_eq!(vm.used().processes, baseline.processes);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn exit_handler_retains_full_manifest_sized_utf8_name_and_unknown_status() {
    unsafe extern "C" fn unknown(
        _state: i64,
        _arguments: *const i64,
        _output: *mut ScalarOutput,
        _context: *mut ScalarContext,
    ) -> u32 {
        u32::MAX
    }
    let [vm, tenant, _] = accounts();
    let name = "诊断".repeat(42) + "Test";
    assert_eq!(name.len(), 256);
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers[4].name = name.clone();
    data.entries[4] = unknown;
    data.manifest.validate().unwrap();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let pid = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    worker
        .send(pid, &module.handler(&name).unwrap(), &[], 8)
        .unwrap();
    assert!(matches!(
        worker.tick(8).unwrap(),
        WorkerEvent::Exited { .. }
    ));
    assert_eq!(worker.last_exit().unwrap().handler(), name);
    assert_eq!(
        worker.last_exit().unwrap().execution.fault,
        Some(ExecutionFault::UnknownStatus(u32::MAX))
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}
