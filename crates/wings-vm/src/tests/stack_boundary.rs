use super::*;
use crate::message_values::{BytePayload, ListPayload};

unsafe extern "C" fn check_stack(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let marker = 0_u64;
    let address = std::ptr::addr_of!(marker) as usize;
    let limit = unsafe { (*context).stack_limit };
    if limit == 0 || address <= limit || address - limit > 256 * 1024 {
        return STATUS_STACK_EXHAUSTED;
    }
    unsafe {
        *output = ScalarOutput {
            state,
            reply: state,
        }
    };
    STATUS_OK
}

unsafe extern "C" fn stack_failure(
    _state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    unsafe {
        *output = ScalarOutput {
            state: 99,
            reply: 99,
        }
    };
    STATUS_STACK_EXHAUSTED
}

fn with_entry(entry: ScalarEntry) -> ScalarModule {
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.initializer = entry;
    data.entries.fill(entry);
    module
}

#[test]
fn initializer_sync_and_resumable_calls_use_bounded_stacks() {
    let module = with_entry(check_stack);
    let mut process = module.instantiate().unwrap();
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
    let mut execution = process
        .start_controlled(
            "Get",
            &[],
            64,
            &CancellationToken::default(),
            &PreemptionToken::default(),
        )
        .unwrap();
    assert_eq!(execution.resume(64), Ok(ExecutionStatus::Complete(Some(0))));
}

#[test]
fn direct_accounted_execution_retains_control_capacity_while_its_stack_is_live() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let module = module();
    let mut process = module.instantiate_in_pool(&bound).unwrap();
    let baseline = vm.used();
    let stack_bytes =
        (pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack) as u64;
    let mut execution = process
        .start_in_pool(
            "Bump",
            &[7],
            128,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &bound,
        )
        .unwrap();
    assert_eq!(
        account.used().bytes,
        stack_bytes + crate::execution::INVOCATION_OVERHEAD
    );
    assert_eq!(
        vm.used().bytes,
        baseline.bytes + crate::execution::INVOCATION_OVERHEAD
    );
    assert_eq!(execution.resume(1).unwrap(), ExecutionStatus::Suspended);
    assert_eq!(
        account.used().bytes,
        stack_bytes + crate::execution::INVOCATION_OVERHEAD
    );
    drop(execution);
    assert_eq!(vm.used(), baseline);
    assert_eq!(pool.stats().active, 0);
    assert_eq!(account.used(), Resources::default());
    assert_eq!(process.state, 0);
    drop((process, bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn direct_initialization_and_calls_reject_each_control_quota_before_borrowing_a_stack() {
    for level in [
        ResourceLevel::Process,
        ResourceLevel::Tenant,
        ResourceLevel::Vm,
    ] {
        let [vm, tenant, account] = accounts();
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let module = module();
        let mut process = module.instantiate_in_pool(&bound).unwrap();
        let owner = match level {
            ResourceLevel::Process => &account,
            ResourceLevel::Tenant => &tenant,
            ResourceLevel::Vm => &vm,
        };
        let fill = owner
            .reserve(Resources {
                bytes: owner.limit().bytes
                    - owner.used().bytes
                    - crate::execution::INVOCATION_OVERHEAD
                    + 1,
                ..Resources::default()
            })
            .unwrap();
        let baseline = vm.used();
        let slots = pool.stats();
        assert!(
            module
                .instantiate_in_pool(&bound)
                .err()
                .unwrap()
                .contains(&format!("{level:?}"))
        );
        assert_eq!(vm.used(), baseline);
        assert_eq!(pool.stats(), slots);
        assert!(
            process
                .invoke("Bump", &[7])
                .err()
                .unwrap()
                .contains(&format!("{level:?}"))
        );
        assert_eq!(vm.used(), baseline);
        assert_eq!(pool.stats(), slots);
        assert!(!process.failed);
        assert_eq!(process.state, 0);
        let mut task = ScalarTask::new(process);
        let control_creations = std::cell::Cell::new(0);
        let controls = || {
            control_creations.set(control_creations.get() + 1);
            (CancellationToken::default(), PreemptionToken::default())
        };
        assert!(
            task.start_kind(
                &module.handler("Bump").unwrap(),
                &[7],
                128,
                controls,
                &bound,
                false,
                0,
                &ListPayload::default(),
                &mut BytePayload::default(),
            )
            .is_err()
        );
        assert_eq!(control_creations.get(), 0);
        assert_eq!(vm.used(), baseline);
        assert_eq!(pool.stats(), slots);
        assert!(!task.is_active());
        drop(fill);
        let stack = bound.acquire().unwrap();
        let busy = vm.used();
        assert!(
            task.start_kind(
                &module.handler("Bump").unwrap(),
                &[7],
                128,
                controls,
                &bound,
                false,
                0,
                &ListPayload::default(),
                &mut BytePayload::default(),
            )
            .is_err()
        );
        assert_eq!(control_creations.get(), 0);
        assert_eq!(vm.used(), busy);
        assert!(!task.is_active());
        drop(stack);
        task.start_kind(
            &module.handler("Bump").unwrap(),
            &[7],
            128,
            controls,
            &bound,
            false,
            0,
            &ListPayload::default(),
            &mut BytePayload::default(),
        )
        .unwrap();
        assert_eq!(control_creations.get(), 1);
        assert_eq!(
            task.resume(128).unwrap(),
            ExecutionStatus::Complete(Some(7))
        );
        assert_eq!(account.used(), Resources::default());
        drop((task, bound, pool));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn scalar_initialization_keeps_explicit_pool_for_subsequent_default_calls() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let module = module();
    let mut process = module.instantiate_in_pool(&bound).unwrap();
    let baseline = vm.used();
    drop(bound);
    let stack = pool.for_process(&account).unwrap().acquire().unwrap();
    let active = vm.used();
    assert!(process.invoke("Bump", &[7]).is_err());
    assert_eq!(vm.used(), active);
    assert!(!process.failed);
    assert_eq!(process.state, 0);
    drop(stack);
    assert_eq!(vm.used(), baseline);
    assert_eq!(process.invoke("Bump", &[7]).unwrap(), Some(7));
    assert_eq!(vm.used(), baseline);
    drop(pool);
    assert!(vm.used().bytes > 0);
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
    drop(process);
    assert_eq!(vm.used(), Resources::default());
}

thread_local! {
    static EXECUTION_ACCOUNT: std::cell::RefCell<Option<ResourceAccount>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "C" fn observed_control_account(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    _context: *mut ScalarContext,
) -> u32 {
    EXECUTION_ACCOUNT.with(|slot| {
        let slot = slot.borrow();
        let Some(account) = slot.as_ref() else {
            return STATUS_INVALID_CONTROL;
        };
        unsafe {
            *output = ScalarOutput {
                state,
                reply: account.used().bytes as i64,
            };
        }
        STATUS_OK
    })
}

#[test]
fn initializer_and_sync_handler_observe_one_control_reservation_inside_trusted_entries() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let module = with_entry(observed_control_account);
    EXECUTION_ACCOUNT.with(|slot| *slot.borrow_mut() = Some(account.clone()));
    let expected = (pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack)
        as u64
        + crate::execution::INVOCATION_OVERHEAD;
    let (status, output, heap) =
        crate::execution::initialize(observed_control_account, &bound, None).unwrap();
    assert_eq!(status, STATUS_OK);
    assert_eq!(output.reply, expected as i64);
    assert!(heap.is_none());
    assert_eq!(account.used(), Resources::default());
    let mut process = module.instantiate_in_pool(&bound).unwrap();
    assert_eq!(process.invoke("Get", &[]).unwrap(), Some(expected as i64));
    assert_eq!(account.used(), Resources::default());
    EXECUTION_ACCOUNT.with(|slot| slot.borrow_mut().take());
    drop((process, bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn initializer_uses_process_accounted_pool_and_releases_its_stack() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    let module = with_entry(check_stack);
    module.instantiate_in_pool(&bound).unwrap();
    assert_eq!(pool.stats().active, 0);
    assert_eq!(vm.used(), baseline);
    assert_eq!(account.used(), Resources::default());
    let stack = bound.acquire().unwrap();
    assert!(module.instantiate_in_pool(&bound).is_err());
    drop(stack);
    module.instantiate_in_pool(&bound).unwrap();
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn default_pool_exhaustion_is_rejected_before_execution_without_poisoning() {
    let module = module();
    let mut process = module.instantiate().unwrap();
    let pool = crate::execution::default_stack_pool().unwrap();
    let baseline = pool.stats();
    let mut stacks = Vec::new();
    for _ in 0..baseline.available {
        stacks.push(pool.acquire().unwrap());
    }
    assert!(process.invoke("Bump", &[7]).is_err());
    assert!(!process.failed);
    assert_eq!(process.state, 0);
    assert!(module.instantiate().is_err());
    drop(stacks);
    assert_eq!(pool.stats().active, baseline.active);
    assert_eq!(process.invoke("Bump", &[7]).unwrap(), Some(7));
    module.instantiate().unwrap();
}

#[test]
fn stack_failure_rejects_initializer_and_never_commits_handler_output() {
    assert!(
        with_entry(stack_failure)
            .instantiate()
            .err()
            .unwrap()
            .contains("stack exhausted")
    );
    for resumable in [false, true] {
        let mut module = module();
        Arc::get_mut(&mut module.inner)
            .unwrap()
            .entries
            .fill(stack_failure);
        let mut process = module.instantiate().unwrap();
        let error = if resumable {
            process
                .start_controlled(
                    "Get",
                    &[],
                    64,
                    &CancellationToken::default(),
                    &PreemptionToken::default(),
                )
                .unwrap()
                .resume(64)
                .unwrap_err()
        } else {
            process.invoke("Get", &[]).unwrap_err()
        };
        assert!(error.contains("stack exhausted"));
        assert!(process.failed);
        assert_eq!(process.state, 0);
        assert!(
            process
                .invoke("Get", &[])
                .unwrap_err()
                .contains("terminated")
        );
    }
}

#[test]
fn heap_initialization_rejections_release_every_reservation() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let baseline = vm.used();
    for capacity in [0, 1025, usize::MAX] {
        assert!(
            module()
                .instantiate_with_heap(&bound, &account, capacity)
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
    }
    let small = tenant.process(limits(1)).unwrap();
    let small_bound = pool.for_process(&small).unwrap();
    assert!(
        module()
            .instantiate_with_heap(&small_bound, &small, 1)
            .is_err()
    );
    drop(small_bound);
    assert_eq!(vm.used(), baseline);
    let probe = module().instantiate_with_heap(&bound, &account, 1).unwrap();
    let full_heap_bytes = account.used().bytes;
    drop(probe);
    let small = tenant.process(limits(full_heap_bytes - 1)).unwrap();
    let small_bound = pool.for_process(&small).unwrap();
    assert!(
        module()
            .instantiate_with_heap(&small_bound, &small, 1)
            .is_err()
    );
    drop(small_bound);
    assert_eq!(vm.used(), baseline);
    let stack = bound.acquire().unwrap();
    let active = vm.used();
    assert!(module().instantiate_with_heap(&bound, &account, 1).is_err());
    assert_eq!(vm.used(), active);
    drop(stack);
    assert_eq!(vm.used(), baseline);
    assert!(
        with_entry(stack_failure)
            .instantiate_with_heap(&bound, &account, 1)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    let process = module().instantiate_with_heap(&bound, &account, 1).unwrap();
    assert!(vm.used().bytes > baseline.bytes);
    drop(process);
    assert_eq!(vm.used(), baseline);
    drop((bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn heap_calls_keep_their_accounted_pool_and_reject_foreign_pools_before_execution() {
    let [vm, tenant, account] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let module = module();
    let baseline = vm.used();
    let mut process = module.instantiate_with_heap(&bound, &account, 1).unwrap();
    let held = vm.used();
    let stack = bound.acquire().unwrap();
    let active = vm.used();
    assert!(process.invoke("Bump", &[7]).is_err());
    assert_eq!(vm.used(), active);
    assert!(!process.failed);
    drop(stack);
    assert_eq!(vm.used(), held);
    let other = tenant.process(limits(1 << 20)).unwrap();
    let foreign = pool.for_process(&other).unwrap();
    let unaccounted = StackPool::new(1, 0).unwrap();
    for invalid in [&foreign, &unaccounted] {
        assert!(
            process
                .start_in_pool(
                    "Get",
                    &[],
                    64,
                    &CancellationToken::default(),
                    &PreemptionToken::default(),
                    invalid
                )
                .is_err()
        );
        assert_eq!(vm.used(), held);
        assert!(!process.failed);
        assert!(module.instantiate_with_heap(invalid, &account, 1).is_err());
        assert_eq!(vm.used(), held);
    }
    assert_eq!(process.invoke("Bump", &[7]).unwrap(), Some(7));
    assert_eq!(vm.used(), held);
    drop(process);
    assert_eq!(vm.used(), baseline);
    drop((foreign, unaccounted, bound, pool));
    assert_eq!(vm.used(), Resources::default());
}
