//! Runtime unit tests use small trusted Rust ABI entries, not generated native code.
//! Compiler safepoint coverage and real AOT execution remain in verify_scalar_aot.py.
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use wings_abi::{
    SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_INVALID_CONTROL, STATUS_OK, STATUS_OVERFLOW, ScalarContext, ScalarEntry, ScalarOutput,
    ValueType,
};
use wings_artifact::{FORMAT_VERSION, Handler, Manifest};

use crate::module::ModuleData;
use crate::*;

mod primitives;

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 16,
        messages: 16,
        requests: 16,
    }
}

fn accounts() -> [ResourceAccount; 3] {
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant(limits(16 << 20)).unwrap();
    let process = tenant.process(limits(8 << 20)).unwrap();
    [vm, tenant, process]
}

unsafe extern "C" fn initialize(
    _state: i64,
    _args: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe { *output = ScalarOutput { state: 0, reply: 0 } };
    STATUS_OK
}

// Keep all failure paths non-panicking: unwinding across ScalarEntry is forbidden.
unsafe fn checkpoint(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    loop {
        if unsafe { &*context.cancellation }.load(Ordering::SeqCst) != 0 {
            return STATUS_CANCELLED;
        }
        if context.remaining_steps == 0 {
            return STATUS_BUDGET_EXHAUSTED;
        }
        if context.slice_remaining == 0
            || unsafe { &*context.yield_request }.load(Ordering::SeqCst) != 0
        {
            let Some(slow_path) = context.slow_path else {
                return STATUS_INVALID_CONTROL;
            };
            let status = unsafe { slow_path(context) };
            if status != STATUS_OK {
                return status;
            }
            continue;
        }
        context.remaining_steps -= 1;
        context.slice_remaining -= 1;
        return STATUS_OK;
    }
}

unsafe extern "C" fn increment(
    state: i64,
    args: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    for _ in 0..4 {
        let status = unsafe { checkpoint(context) };
        if status != STATUS_OK {
            return status;
        }
    }
    let Some(next) = state.checked_add(unsafe { *args }) else {
        return STATUS_OVERFLOW;
    };
    unsafe {
        *output = ScalarOutput {
            state: next,
            reply: next,
        }
    };
    STATUS_OK
}

unsafe extern "C" fn add(
    state: i64,
    args: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let status = unsafe { increment(state, args, output, context) };
    if status == STATUS_OK {
        unsafe { (*output).reply = 0 };
    }
    status
}

unsafe extern "C" fn get(
    state: i64,
    _args: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let status = unsafe { checkpoint(context) };
    if status == STATUS_OK {
        unsafe {
            *output = ScalarOutput {
                state,
                reply: state,
            }
        };
    }
    status
}

unsafe extern "C" fn unit(
    state: i64,
    _args: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let status = unsafe { checkpoint(context) };
    if status == STATUS_OK {
        unsafe { *output = ScalarOutput { state, reply: 0 } };
    }
    status
}

unsafe extern "C" fn fail(
    _state: i64,
    _args: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    // Even candidate output from a failed trusted entry must not be committed.
    unsafe {
        *output = ScalarOutput {
            state: 99,
            reply: 99,
        }
    };
    STATUS_DIVISION_BY_ZERO
}

unsafe extern "C" fn invalid_output(
    _state: i64,
    _args: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe { *output = ScalarOutput { state: 2, reply: 2 } };
    STATUS_OK
}

fn module() -> ScalarModule {
    let handlers = [
        (
            "Add",
            vec![("value".into(), ValueType::Int)],
            None,
            add as ScalarEntry,
        ),
        (
            "Bump",
            vec![("value".into(), ValueType::Int)],
            Some(ValueType::Int),
            increment as ScalarEntry,
        ),
        ("Get", vec![], Some(ValueType::Int), get as ScalarEntry),
        ("Ack", vec![], Some(ValueType::Unit), unit as ScalarEntry),
        ("Fail", vec![], None, fail as ScalarEntry),
        (
            "FailRequest",
            vec![],
            Some(ValueType::Int),
            fail as ScalarEntry,
        ),
        (
            "Bool",
            vec![("value".into(), ValueType::Bool)],
            None,
            unit as ScalarEntry,
        ),
        (
            "Unit",
            vec![("value".into(), ValueType::Unit)],
            None,
            unit as ScalarEntry,
        ),
    ];
    let manifest = Manifest {
        format: FORMAT_VERSION,
        abi: SCALAR_ABI_VERSION,
        target: wings_abi::host_target().unwrap().into(),
        process: "RuntimeFixture".into(),
        state: ValueType::Int,
        handlers: handlers
            .iter()
            .map(|(name, parameters, reply, _)| Handler {
                name: (*name).into(),
                parameters: parameters.clone(),
                reply: *reply,
            })
            .collect(),
    };
    manifest.validate().unwrap();
    ScalarModule {
        inner: Arc::new(ModuleData {
            manifest,
            initializer: initialize,
            entries: handlers.into_iter().map(|(_, _, _, entry)| entry).collect(),
        }),
    }
}

fn worker_fixture(
    capacity: usize,
    pending: usize,
    mailbox: usize,
) -> (
    ResourceAccount,
    ResourceAccount,
    ScalarWorker,
    ScalarModule,
    Pid,
) {
    let [vm, tenant, _] = accounts();
    let module = module();
    let mut worker = ScalarWorker::with_request_capacity(&tenant, capacity, pending).unwrap();
    let pid = worker.spawn(&module, limits(4 << 20), mailbox).unwrap();
    (vm, tenant, worker, module, pid)
}

#[test]
fn module_instances_share_schema_but_have_independent_state_and_failure() {
    let module = module();
    let mut first = module.instantiate().unwrap();
    let mut second = module.clone().instantiate().unwrap();
    let bump = module.handler("Bump").unwrap();
    assert_eq!(
        first
            .invoke_handler(&bump, &[5], 4, &CancellationToken::default())
            .unwrap(),
        Some(5)
    );
    assert_eq!(second.invoke("Get", &[]).unwrap(), Some(0));
    assert!(first.invoke("Fail", &[]).is_err());
    assert_eq!(first.state, 5);
    assert!(first.invoke("Get", &[]).is_err());
    assert_eq!(second.invoke("Bump", &[3]).unwrap(), Some(3));
    assert_eq!(module.manifest().process, "RuntimeFixture");
}

#[test]
fn handler_identity_is_module_bound_not_just_schema_bound() {
    let first = module();
    let other = module();
    let mut process = first.instantiate().unwrap();
    let foreign = other.handler("Get").unwrap();
    assert!(
        process
            .invoke_handler(&foreign, &[], 10, &CancellationToken::default())
            .unwrap_err()
            .contains("different module")
    );
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
    assert!(first.handler("Missing").is_err());
    assert_eq!(
        first.handler("Ack").unwrap().schema().reply,
        Some(ValueType::Unit)
    );
}

#[test]
fn invalid_arguments_and_names_do_not_poison_a_process() {
    let mut process = module().instantiate().unwrap();
    for (name, arguments) in [
        ("Missing", vec![]),
        ("Bump", vec![]),
        ("Bump", vec![1, 2]),
        ("Bool", vec![-1]),
        ("Bool", vec![2]),
        ("Unit", vec![1]),
    ] {
        assert!(process.invoke(name, &arguments).is_err());
        assert!(!process.failed);
        assert_eq!(process.state, 0);
    }
    for value in [0, 1] {
        assert_eq!(process.invoke("Bool", &[value]).unwrap(), None);
    }
    assert_eq!(process.invoke("Unit", &[0]).unwrap(), None);
    assert_eq!(process.invoke("Ack", &[]).unwrap(), Some(0));
}

#[test]
fn cancellation_is_shared_between_clones_and_thread_safe() {
    let token = CancellationToken::default();
    let clone = token.clone();
    std::thread::spawn(move || clone.cancel()).join().unwrap();
    let mut process = module().instantiate().unwrap();
    let error = process
        .invoke_controlled("Bump", &[2], 0, &token)
        .unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(process.failed);
    assert_eq!(process.state, 0);
}

#[test]
fn total_budget_is_exact_and_resets_on_each_successful_invocation() {
    for budget in 0..4 {
        let mut process = module().instantiate().unwrap();
        assert!(
            process
                .invoke_controlled("Bump", &[2], budget, &CancellationToken::default())
                .unwrap_err()
                .contains("budget")
        );
        assert_eq!(process.state, 0);
    }
    let mut process = module().instantiate().unwrap();
    for expected in [2, 4, 6] {
        assert_eq!(
            process
                .invoke_controlled("Bump", &[2], 4, &CancellationToken::default())
                .unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn status_mapping_and_unknown_status_fail_closed() {
    for (status, expected) in [
        (STATUS_OVERFLOW, "overflow"),
        (STATUS_DIVISION_BY_ZERO, "division by zero"),
        (STATUS_CANCELLED, "cancelled"),
        (STATUS_BUDGET_EXHAUSTED, "budget"),
        (STATUS_INVALID_CONTROL, "callback"),
        (u32::MAX, "unknown"),
    ] {
        assert!(check_status(status).unwrap_err().contains(expected));
    }
    assert!(check_status(STATUS_OK).is_ok());
}

#[test]
fn invalid_initializer_state_and_managed_initializer_failure_are_rejected() {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.state = ValueType::Bool;
    data.initializer = invalid_output;
    assert!(
        module
            .instantiate()
            .err()
            .unwrap()
            .contains("invalid scalar state")
    );
    Arc::get_mut(&mut module.inner).unwrap().initializer = fail;
    assert!(
        module
            .instantiate()
            .err()
            .unwrap()
            .contains("division by zero")
    );
}

#[test]
fn invalid_success_output_never_commits_state_and_terminates_process() {
    for (state_type, reply_type) in [
        (ValueType::Bool, Some(ValueType::Int)),
        (ValueType::Int, Some(ValueType::Bool)),
        (ValueType::Int, Some(ValueType::Unit)),
        (ValueType::Int, None),
    ] {
        let mut module = module();
        let data = Arc::get_mut(&mut module.inner).unwrap();
        data.manifest.state = state_type;
        data.manifest.handlers[2].reply = reply_type;
        data.entries[2] = invalid_output;
        let mut process = module.instantiate().unwrap();
        assert!(
            process
                .invoke("Get", &[])
                .unwrap_err()
                .contains("invalid scalar")
        );
        assert_eq!(process.state, 0);
        assert!(process.failed);
    }
}

#[test]
fn task_zero_slice_and_busy_rejection_preserve_the_active_invocation() {
    let module = module();
    let handler = module.handler("Bump").unwrap();
    let pool = StackPool::new(1, 1).unwrap();
    let mut task = ScalarTask::new(module.instantiate().unwrap());
    assert!(task.resume(1).is_err());
    task.start(
        &handler,
        &[7],
        4,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        &pool,
    )
    .unwrap();
    assert!(task.resume(0).is_err());
    assert!(
        task.start(
            &handler,
            &[9],
            4,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool
        )
        .is_err()
    );
    assert!(task.is_active());
    for _ in 0..3 {
        assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Suspended);
        assert_eq!(task.committed_state(), 0);
        assert_eq!(pool.stats().active, 1);
    }
    assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Complete(Some(7)));
    assert!(!task.is_active());
    assert_eq!(task.committed_state(), 7);
    assert_eq!(pool.stats().active, 0);
    assert!(task.resume(1).is_err());
}

#[test]
fn task_owns_arguments_and_can_move_between_slices_without_replay() {
    let module = module();
    let pool = StackPool::new(1, 0).unwrap();
    let mut task = ScalarTask::new(module.instantiate().unwrap());
    let mut arguments = [11];
    task.start(
        &module.handler("Bump").unwrap(),
        &arguments,
        4,
        &CancellationToken::default(),
        &PreemptionToken::default(),
        &pool,
    )
    .unwrap();
    arguments[0] = 99;
    assert_eq!(arguments[0], 99);
    assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Suspended);
    let mut moved = vec![task].pop().unwrap();
    assert_eq!(
        moved.resume(3).unwrap(),
        ExecutionStatus::Complete(Some(11))
    );
    let mut process = moved.into_process();
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(11));
}

#[test]
fn cancellation_between_slices_preserves_state_and_releases_stack() {
    let module = module();
    let pool = StackPool::new(1, 1).unwrap();
    let token = CancellationToken::default();
    let mut process = module.instantiate().unwrap();
    {
        let mut execution = process
            .start_in_pool("Bump", &[5], 4, &token, &PreemptionToken::default(), &pool)
            .unwrap();
        assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        token.cancel();
        assert!(execution.resume(3).unwrap_err().contains("cancelled"));
        assert!(execution.resume(1).is_err());
    }
    assert_eq!(process.state, 0);
    assert!(process.failed);
    assert_eq!(pool.stats().active, 0);
}

#[test]
fn preemption_request_suspends_without_cancelling_or_replenishing_budget() {
    let module = module();
    let pool = StackPool::new(1, 1).unwrap();
    let token = PreemptionToken::default();
    token.request();
    let mut task = ScalarTask::new(module.instantiate().unwrap());
    task.start(
        &module.handler("Bump").unwrap(),
        &[5],
        3,
        &CancellationToken::default(),
        &token,
        &pool,
    )
    .unwrap();
    assert_eq!(task.resume(100).unwrap(), ExecutionStatus::Suspended);
    assert_eq!(task.committed_state(), 0);
    assert!(task.resume(100).unwrap_err().contains("budget"));
    assert_eq!(task.committed_state(), 0);
    assert_eq!(pool.stats().active, 0);
}

#[test]
fn dropping_started_and_unstarted_execution_terminates_without_committing() {
    for start in [false, true] {
        let pool = StackPool::new(1, 1).unwrap();
        let mut process = module().instantiate().unwrap();
        let mut execution = process
            .start_in_pool(
                "Bump",
                &[5],
                4,
                &CancellationToken::default(),
                &PreemptionToken::default(),
                &pool,
            )
            .unwrap();
        if start {
            assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        drop(execution);
        assert_eq!(pool.stats().active, 0);
        assert_eq!(process.state, 0);
        assert!(process.invoke("Get", &[]).is_err());
    }
}

#[test]
fn into_process_cancels_an_active_task_but_preserves_an_idle_one() {
    let module = module();
    let mut idle = ScalarTask::new(module.instantiate().unwrap()).into_process();
    assert_eq!(idle.invoke("Get", &[]).unwrap(), Some(0));
    for start in [false, true] {
        let pool = StackPool::new(1, 0).unwrap();
        let mut task = ScalarTask::new(module.instantiate().unwrap());
        task.start(
            &module.handler("Bump").unwrap(),
            &[5],
            4,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool,
        )
        .unwrap();
        if start {
            assert_eq!(task.resume(1).unwrap(), ExecutionStatus::Suspended);
        }
        let mut process = task.into_process();
        assert_eq!(pool.stats().active, 0);
        assert_eq!(process.state, 0);
        assert!(process.invoke("Get", &[]).is_err());
    }
}

#[test]
fn exhausted_stack_pool_rejection_does_not_poison_process() {
    let pool = StackPool::new(1, 0).unwrap();
    let held = pool.acquire().unwrap();
    let mut process = module().instantiate().unwrap();
    assert!(
        process
            .start_in_pool(
                "Bump",
                &[5],
                4,
                &CancellationToken::default(),
                &PreemptionToken::default(),
                &pool
            )
            .err()
            .unwrap()
            .contains("exhausted")
    );
    assert!(!process.failed);
    drop(held);
    let mut execution = process
        .start_in_pool(
            "Bump",
            &[5],
            4,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool,
        )
        .unwrap();
    assert_eq!(
        execution.resume(4).unwrap(),
        ExecutionStatus::Complete(Some(5))
    );
}

#[test]
fn worker_constructor_rejects_invalid_capacity_account_and_quota_without_leaks() {
    let [vm, tenant, process] = accounts();
    for capacity in [0, 257, usize::MAX] {
        assert!(ScalarWorker::new(&tenant, capacity).is_err());
        assert!(ScalarWorker::with_request_capacity(&tenant, 1, capacity).is_err());
    }
    assert!(ScalarWorker::new(&vm, 1).is_err());
    assert!(ScalarWorker::new(&process, 1).is_err());
    assert_eq!(vm.used(), Resources::default());
    let low = vm.tenant(limits(1)).unwrap();
    assert!(ScalarWorker::new(&low, 1).is_err());
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_idle_and_zero_slice_do_not_modify_actor_or_resources() {
    let (vm, _tenant, mut worker, _module, pid) = worker_fixture(1, 1, 1);
    let before = vm.used();
    assert!(worker.tick(0).is_err());
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Idle);
    assert_eq!(worker.queued(pid).unwrap(), 0);
    assert_eq!(worker.process_count(), 1);
    assert_eq!(vm.used(), before);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn spawn_capacity_and_process_quota_failures_leave_no_partial_actor() {
    let [vm, tenant, _] = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let module = module();
    let before = vm.used();
    for capacity in [0, 65, usize::MAX] {
        assert!(worker.spawn(&module, limits(1 << 20), capacity).is_err());
    }
    let mut no_process = limits(1 << 20);
    no_process.processes = 0;
    assert!(worker.spawn(&module, no_process, 1).is_err());
    assert!(worker.spawn(&module, limits(1), 1).is_err());
    assert_eq!(vm.used(), before);
    assert_eq!(worker.process_count(), 0);
    worker.spawn(&module, limits(1 << 20), 1).unwrap();
    let before = vm.used();
    assert!(worker.spawn(&module, limits(1 << 20), 1).is_err());
    assert_eq!(vm.used(), before);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn mailbox_wraparound_keeps_fifo_and_rejects_full_without_charging() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 2);
    let add = module.handler("Add").unwrap();
    worker.send(pid, &add, &[1], 4).unwrap();
    worker.send(pid, &add, &[10], 4).unwrap();
    let full = vm.used();
    assert!(
        worker
            .send(pid, &add, &[1000], 4)
            .unwrap_err()
            .contains("mailbox_full")
    );
    assert_eq!(vm.used(), full);
    assert_eq!(worker.tick(4).unwrap(), WorkerEvent::Delivered(pid));
    assert_eq!(worker.committed_state(pid).unwrap(), 1);
    worker.send(pid, &add, &[100], 4).unwrap();
    for expected in [11, 111] {
        assert_eq!(worker.tick(4).unwrap(), WorkerEvent::Delivered(pid));
        assert_eq!(worker.committed_state(pid).unwrap(), expected);
    }
    assert_eq!(worker.resources(pid).unwrap().messages, 0);
}

#[test]
fn round_robin_visits_each_busy_actor_and_commits_only_on_completion() {
    let (_vm, _tenant, mut worker, module, first) = worker_fixture(2, 1, 1);
    let second = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    for pid in [first, second] {
        worker
            .send(pid, &module.handler("Add").unwrap(), &[3], 4)
            .unwrap();
    }
    for _ in 0..3 {
        for pid in [first, second] {
            assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.committed_state(pid).unwrap(), 0);
        }
    }
    for pid in [first, second] {
        assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Delivered(pid));
        assert_eq!(worker.committed_state(pid).unwrap(), 3);
    }
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Idle);
}

#[test]
fn send_and_request_validate_message_kind_arguments_and_module_atomically() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 2);
    let before = vm.used();
    let add = module.handler("Add").unwrap();
    let get = module.handler("Get").unwrap();
    let ack = module.handler("Ack").unwrap();
    assert!(worker.send(pid, &get, &[], 4).is_err());
    assert!(worker.send(pid, &ack, &[], 4).is_err());
    assert!(
        worker
            .request(pid, &add, &[1], 4, Duration::from_secs(60))
            .is_err()
    );
    assert!(worker.send(pid, &add, &[], 4).is_err());
    assert!(
        worker
            .send(pid, &module.handler("Bool").unwrap(), &[2], 4)
            .is_err()
    );
    let foreign = self::module().handler("Add").unwrap();
    assert!(worker.send(pid, &foreign, &[1], 4).is_err());
    assert_eq!(worker.queued(pid).unwrap(), 0);
    assert_eq!(vm.used(), before);
    let request = worker
        .request(pid, &ack, &[], 1, Duration::from_secs(60))
        .unwrap();
    worker.tick(1).unwrap();
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
}

#[test]
fn stale_and_foreign_pids_cannot_access_or_stop_reused_slots() {
    let (_vm, _tenant, mut worker, module, old) = worker_fixture(1, 1, 1);
    let (_, _, mut other, _, _) = worker_fixture(1, 1, 1);
    assert!(other.queued(old).is_err());
    assert!(other.stop(old).is_err());
    worker.stop(old).unwrap();
    let new = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    assert_ne!(old, new);
    assert!(worker.stop(old).is_err());
    assert!(worker.resources(old).is_err());
    assert!(worker.committed_state(old).is_err());
    assert!(
        worker
            .send(old, &module.handler("Add").unwrap(), &[1], 4)
            .is_err()
    );
    assert_eq!(worker.process_count(), 1);
    assert_eq!(worker.committed_state(new).unwrap(), 0);
}

#[test]
fn completed_reply_holds_pending_capacity_and_charge_until_taken() {
    let (_vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 2);
    let get = module.handler("Get").unwrap();
    let id = worker
        .request(pid, &get, &[], 1, Duration::from_secs(60))
        .unwrap();
    assert_eq!(worker.take_reply(id).unwrap(), None);
    assert_eq!(worker.resources(pid).unwrap().requests, 1);
    worker.tick(1).unwrap();
    assert!(
        worker
            .request(pid, &get, &[], 1, Duration::from_secs(60))
            .unwrap_err()
            .contains("request_table_full")
    );
    assert_eq!(worker.resources(pid).unwrap().messages, 0);
    assert_eq!(worker.resources(pid).unwrap().requests, 1);
    assert_eq!(
        worker.take_reply(id).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    assert_eq!(worker.resources(pid).unwrap().requests, 0);
    assert!(worker.take_reply(id).is_err());
    assert!(worker.cancel_request(id).is_err());
}

#[test]
fn zero_timeout_does_not_retract_message_or_overwrite_reused_request_slot() {
    let (_vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 2);
    let bump = module.handler("Bump").unwrap();
    let old = worker.request(pid, &bump, &[5], 4, Duration::ZERO).unwrap();
    assert_eq!(
        worker.take_reply(old).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    assert_eq!(worker.queued(pid).unwrap(), 1);
    let new = worker
        .request(pid, &bump, &[7], 4, Duration::from_secs(60))
        .unwrap();
    assert_ne!(old, new);
    worker.tick(4).unwrap();
    assert_eq!(worker.committed_state(pid).unwrap(), 5);
    assert_eq!(worker.take_reply(new).unwrap(), None);
    worker.tick(4).unwrap();
    assert_eq!(
        worker.take_reply(new).unwrap(),
        Some(RequestOutcome::Reply(12))
    );
    assert_eq!(worker.committed_state(pid).unwrap(), 12);
}

#[test]
fn cancellation_during_suspension_is_terminal_but_does_not_cancel_actor_effects() {
    let (_vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    let id = worker
        .request(
            pid,
            &module.handler("Bump").unwrap(),
            &[5],
            4,
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    worker.cancel_request(id).unwrap();
    worker.cancel_request(id).unwrap();
    assert_eq!(worker.tick(3).unwrap(), WorkerEvent::Delivered(pid));
    assert_eq!(worker.committed_state(pid).unwrap(), 5);
    assert_eq!(
        worker.take_reply(id).unwrap(),
        Some(RequestOutcome::Cancelled)
    );
}

#[test]
fn cancel_does_not_replace_already_completed_or_timed_out_outcome() {
    let (_vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    let get = module.handler("Get").unwrap();
    let complete = worker
        .request(pid, &get, &[], 1, Duration::from_secs(60))
        .unwrap();
    worker.tick(1).unwrap();
    worker.cancel_request(complete).unwrap();
    assert_eq!(
        worker.take_reply(complete).unwrap(),
        Some(RequestOutcome::Reply(0))
    );
    let timed_out = worker.request(pid, &get, &[], 1, Duration::ZERO).unwrap();
    worker.cancel_request(timed_out).unwrap();
    assert_eq!(
        worker.take_reply(timed_out).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
}

#[test]
fn request_enqueue_failure_rolls_back_pending_quota_and_slot() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker
        .send(pid, &module.handler("Add").unwrap(), &[1], 4)
        .unwrap();
    let before = vm.used();
    assert!(
        worker
            .request(
                pid,
                &module.handler("Get").unwrap(),
                &[],
                1,
                Duration::from_secs(60)
            )
            .unwrap_err()
            .contains("mailbox_full")
    );
    assert_eq!(vm.used(), before);
    assert_eq!(worker.resources(pid).unwrap().requests, 0);
    worker.tick(4).unwrap();
    let id = worker
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            1,
            Duration::from_secs(60),
        )
        .unwrap();
    worker.tick(1).unwrap();
    assert_eq!(
        worker.take_reply(id).unwrap(),
        Some(RequestOutcome::Reply(1))
    );
}

#[test]
fn request_and_message_quota_failures_do_not_consume_pending_slots() {
    for resource in ["requests", "messages"] {
        let [vm, tenant, _] = accounts();
        let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
        let module = module();
        let mut process_limit = limits(4 << 20);
        if resource == "requests" {
            process_limit.requests = 0;
        } else {
            process_limit.messages = 0;
        }
        let pid = worker.spawn(&module, process_limit, 1).unwrap();
        let before = vm.used();
        for _ in 0..2 {
            assert!(
                worker
                    .request(
                        pid,
                        &module.handler("Get").unwrap(),
                        &[],
                        1,
                        Duration::from_secs(60)
                    )
                    .unwrap_err()
                    .contains("resource_exhausted")
            );
            assert_eq!(worker.queued(pid).unwrap(), 0);
            assert_eq!(vm.used(), before);
        }
    }
}

#[test]
fn managed_failure_marks_queued_requests_failed_and_releases_all_actor_resources() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 2, 2);
    let first = worker
        .request(
            pid,
            &module.handler("FailRequest").unwrap(),
            &[],
            1,
            Duration::from_secs(60),
        )
        .unwrap();
    let second = worker
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            1,
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        matches!(worker.tick(1).unwrap(), WorkerEvent::Exited { pid: exited, reason } if exited == pid && reason.contains("division by zero"))
    );
    assert_eq!(worker.process_count(), 0);
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    assert_eq!(vm.used().requests, 2);
    for id in [first, second] {
        assert_eq!(worker.take_reply(id).unwrap(), Some(RequestOutcome::Failed));
    }
    assert_eq!(vm.used().requests, 0);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn stop_of_suspended_actor_preserves_terminal_results_and_cleans_live_stack() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 2, 2);
    let get = module.handler("Get").unwrap();
    let expired = worker.request(pid, &get, &[], 1, Duration::ZERO).unwrap();
    let waiting = worker
        .request(pid, &get, &[], 1, Duration::from_secs(60))
        .unwrap();
    worker.stop(pid).unwrap();
    assert_eq!(
        worker.take_reply(expired).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    assert_eq!(
        worker.take_reply(waiting).unwrap(),
        Some(RequestOutcome::Failed)
    );
    let pid = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[1], 4)
        .unwrap();
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    worker.stop(pid).unwrap();
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().processes, 0);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn request_ids_are_worker_bound_and_drop_releases_unconsumed_results() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    let id = worker
        .request(
            pid,
            &module.handler("Get").unwrap(),
            &[],
            1,
            Duration::from_secs(60),
        )
        .unwrap();
    let (_, _, mut other, _, _) = worker_fixture(1, 1, 1);
    assert!(other.take_reply(id).is_err());
    assert!(other.cancel_request(id).is_err());
    worker.tick(1).unwrap();
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn tick_start_quota_failure_preserves_mailbox_and_committed_state() {
    let [vm, tenant, _] = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let module = module();
    let pid = worker.spawn(&module, limits(16 * 1024), 1).unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[1], 4)
        .unwrap();
    let before = vm.used();
    assert!(worker.tick(4).unwrap_err().contains("resource_exhausted"));
    assert_eq!(worker.queued(pid).unwrap(), 1);
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    assert_eq!(vm.used(), before);
    worker.stop(pid).unwrap();
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_timer_is_optional_validates_arguments_and_charges_once() {
    let (vm, _tenant, mut worker, _, _) = worker_fixture(1, 1, 1);
    let before = vm.used();
    assert!(worker.tick_timed(1, Duration::from_secs(1)).is_err());
    assert!(worker.tick_timed(0, Duration::from_secs(1)).is_err());
    assert!(worker.tick_timed(1, Duration::ZERO).is_err());
    assert!(worker.tick_timed(1, Duration::MAX).is_err());
    assert_eq!(vm.used(), before);
    worker.enable_timer().unwrap();
    let enabled = vm.used();
    assert!(enabled.bytes > before.bytes);
    assert!(worker.enable_timer().is_err());
    assert_eq!(vm.used(), enabled);
    assert_eq!(
        worker.tick_timed(1, Duration::from_secs(60)).unwrap(),
        WorkerEvent::Idle
    );
    assert_eq!(vm.used(), enabled);
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_drop_cancels_suspended_work_and_joins_enabled_timer() {
    let (vm, _tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_timer().unwrap();
    worker
        .send(pid, &module.handler("Add").unwrap(), &[1], 4)
        .unwrap();
    assert_eq!(
        worker.tick_timed(1, Duration::from_secs(60)).unwrap(),
        WorkerEvent::Suspended(pid)
    );
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn timer_quota_failure_is_atomic_and_does_not_enable_timer() {
    let [vm, tenant, _] = accounts();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    let held = tenant
        .reserve(Resources {
            bytes: tenant.limit().bytes - tenant.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    let before = vm.used();
    assert!(
        worker
            .enable_timer()
            .unwrap_err()
            .contains("resource_exhausted")
    );
    assert_eq!(vm.used(), before);
    assert!(worker.tick_timed(1, Duration::from_secs(1)).is_err());
    drop(held);
    worker.enable_timer().unwrap();
    drop(worker);
    assert_eq!(vm.used(), Resources::default());
}
