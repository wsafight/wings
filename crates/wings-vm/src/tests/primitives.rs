use std::time::{Duration, Instant};

use super::{accounts, limits};
use crate::execution::PreemptionToken;
use crate::stack_pool::{EXECUTION_STACK_BYTES, StackPool};
use crate::timer::DeadlineTimer;
use crate::{ResourceAccount, Resources, WorkerParker};
use corosensei::stack::Stack;

#[test]
fn stack_pool_rejects_invalid_capacity_and_cache_limits() {
    for (capacity, cached) in [(0, 0), (2049, 0), (usize::MAX, 0), (1, 2)] {
        assert!(StackPool::new(capacity, cached).is_err());
    }
}

#[test]
fn stack_slots_are_aligned_disjoint_and_bounded_by_guard_pages() {
    let pool = StackPool::new(2, 0).unwrap();
    let first = pool.acquire().unwrap();
    let second = pool.acquire().unwrap();
    let stats = pool.stats();
    assert_eq!(stats.active, 2);
    assert_eq!(stats.available, 0);
    assert_eq!(
        stats.reserved_bytes,
        2 * (stats.usable_bytes_per_stack + stats.guard_bytes_per_stack)
    );
    assert!(stats.usable_bytes_per_stack >= EXECUTION_STACK_BYTES);
    assert!(stats.guard_bytes_per_stack.is_power_of_two());
    for stack in [&first, &second] {
        assert_eq!(stack.base().get() % corosensei::stack::STACK_ALIGNMENT, 0);
        assert_eq!(
            stack.base().get() - stack.limit().get(),
            stats.reserved_bytes / 2
        );
    }
    assert_ne!(first.base(), second.base());
    assert!(pool.acquire().is_err());
    drop(first);
    assert_eq!(pool.stats().active, 1);
    assert_eq!(pool.stats().available, 1);
    let replacement = pool.acquire().unwrap();
    assert_ne!(replacement.base(), second.base());
    drop((replacement, second));
    assert_eq!(pool.stats().active, 0);
    assert_eq!(pool.stats().cached, 0);
    assert_eq!(pool.stats().reset_failures, 0);
}

#[test]
fn stack_cache_is_shared_by_clones_and_trim_does_not_touch_active_slots() {
    let pool = StackPool::new(2, 1).unwrap();
    let clone = pool.clone();
    let active = pool.acquire().unwrap();
    let released = clone.acquire().unwrap();
    drop(released);
    assert_eq!(pool.stats().cached, 1);
    assert_eq!(clone.stats(), pool.stats());
    pool.trim().unwrap();
    assert_eq!(pool.stats().cached, 0);
    assert_eq!(pool.stats().active, 1);
    drop(active);
    assert_eq!(pool.stats().cached, 1);
    clone.trim().unwrap();
    clone.trim().unwrap();
    assert_eq!(pool.stats().available, 2);
    assert_eq!(pool.stats().reset_failures, 0);
}

#[test]
fn active_stack_keeps_pool_alive_after_handles_are_dropped() {
    let [vm, tenant, process] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&process).unwrap();
    let stack = bound.acquire().unwrap();
    let charged = vm.used();
    drop((pool, bound));
    assert_eq!(vm.used(), charged);
    drop(stack);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn accounted_stack_pool_validates_levels_tenant_and_process_binding() {
    let [vm, tenant, process] = accounts();
    assert!(StackPool::new_accounted(&vm, 1, 0).is_err());
    assert!(StackPool::new_accounted(&process, 1, 0).is_err());
    let unaccounted = StackPool::new(1, 0).unwrap();
    assert!(unaccounted.for_process(&process).is_err());
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let before = vm.used();
    assert!(pool.acquire().is_err());
    assert!(pool.for_process(&tenant).is_err());
    let other_tenant = vm.tenant(limits(1 << 20)).unwrap();
    let other_process = other_tenant.process(limits(1 << 20)).unwrap();
    assert!(pool.for_process(&other_process).is_err());
    assert_eq!(vm.used(), before);
    let bound = pool.for_process(&process).unwrap();
    let stack = bound.acquire().unwrap();
    assert!(process.used().bytes > 0);
    drop(stack);
    assert_eq!(process.used(), Resources::default());
    drop((pool, bound));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn stack_capacity_admission_failure_rolls_back_metadata_and_prior_slots() {
    for constrained_level in 0..2 {
        let mut amounts = [4 << 20; 2];
        amounts[constrained_level] = EXECUTION_STACK_BYTES as u64 + 128 * 1024;
        let vm = ResourceAccount::vm(limits(amounts[0]));
        let tenant = vm.tenant(limits(amounts[1])).unwrap();
        assert!(StackPool::new_accounted(&tenant, 2, 0).is_err());
        assert_eq!(vm.used(), Resources::default());
        assert_eq!(tenant.used(), Resources::default());
    }
}

#[test]
fn process_stack_limit_rejects_without_activation_then_allows_another_process() {
    let [vm, tenant, process] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let low = tenant.process(limits(1)).unwrap();
    let low_pool = pool.for_process(&low).unwrap();
    let before = vm.used();
    assert!(low_pool.acquire().is_err());
    assert_eq!(pool.stats().active, 0);
    assert_eq!(pool.stats().available, 1);
    assert_eq!(vm.used(), before);
    let bound = pool.for_process(&process).unwrap();
    let stack = bound.acquire().unwrap();
    assert_eq!(vm.used(), before);
    assert_eq!(
        process.used().bytes as usize,
        pool.stats().usable_bytes_per_stack + pool.stats().guard_bytes_per_stack
    );
    drop(stack);
    assert_eq!(process.used(), Resources::default());
    pool.trim().unwrap();
    assert_eq!(vm.used(), before);
    drop((pool, low_pool, bound));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn stack_transfer_succeeds_when_vm_and_tenant_are_exactly_full() {
    let [vm, tenant, process] = accounts();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let bound = pool.for_process(&process).unwrap();
    let filler = tenant
        .reserve(Resources {
            bytes: tenant.limit().bytes - tenant.used().bytes,
            ..Resources::default()
        })
        .unwrap();
    assert_eq!(vm.used().bytes, vm.limit().bytes);
    let before = vm.used();
    let stack = bound.acquire().unwrap();
    assert_eq!(vm.used(), before);
    drop(stack);
    assert_eq!(vm.used(), before);
    drop((filler, bound, pool));
    assert_eq!(vm.used(), Resources::default());
}

fn preemption_requested(token: &PreemptionToken) -> bool {
    let module = super::module();
    let pool = StackPool::new(1, 0).unwrap();
    let mut task = crate::ScalarTask::new(module.instantiate().unwrap());
    task.start(
        &module.handler("Get").unwrap(),
        &[],
        1,
        &crate::CancellationToken::default(),
        token,
        &pool,
    )
    .unwrap();
    task.resume(1).unwrap() == crate::ExecutionStatus::Suspended
}

#[test]
fn deadline_timer_past_deadline_sets_request_and_disarm_clears_it() {
    let [vm, tenant, _] = accounts();
    let token = PreemptionToken::default();
    let timer = DeadlineTimer::new(&tenant, &token).unwrap();
    timer.arm(Instant::now()).unwrap();
    assert!(preemption_requested(&token));
    // Observing a suspension consumes the flag; set it again to test disarm itself.
    token.request();
    timer.disarm();
    assert!(!preemption_requested(&token));
    drop(timer);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn deadline_timer_rearm_clears_previous_flag_and_drop_does_not_wait_for_deadline() {
    let [vm, tenant, _] = accounts();
    let token = PreemptionToken::default();
    let timer = DeadlineTimer::new(&tenant, &token).unwrap();
    timer.arm(Instant::now()).unwrap();
    timer
        .arm(Instant::now() + Duration::from_secs(3600))
        .unwrap();
    assert!(!preemption_requested(&token));
    let start = Instant::now();
    drop(timer);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(!preemption_requested(&token));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn worker_wakeup_handles_early_notifications_coalescing_and_zero_timeout() {
    let mut parker = WorkerParker::new().unwrap();
    assert!(!parker.wait(Duration::ZERO).unwrap());
    let notifier = parker.notifier();
    for _ in 0..32 {
        notifier.notify().unwrap();
    }
    assert!(parker.wait(Duration::ZERO).unwrap());
    assert!(!parker.wait(Duration::ZERO).unwrap());
    notifier.notify().unwrap();
    assert!(parker.wait(Duration::from_secs(1)).unwrap());
    assert!(!parker.wait(Duration::ZERO).unwrap());
}

#[test]
fn worker_wakeup_accepts_cross_thread_notification_and_survives_owner_drop() {
    let mut parker = WorkerParker::new().unwrap();
    let notifier = parker.notifier();
    let clone = notifier.clone();
    let sender = std::thread::spawn(move || clone.notify().unwrap());
    assert!(parker.wait(Duration::from_secs(5)).unwrap());
    sender.join().unwrap();
    drop(parker);
    notifier.notify().unwrap();
}
