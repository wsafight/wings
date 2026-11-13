use super::*;

#[test]
fn default_preemption_has_no_observer() {
    let token = PreemptionToken::default();
    token.request();
    assert_eq!(token.take_request(), None);
    token.confirm_yield(std::time::Instant::now());
    assert_eq!(token.observation(), None);
}

#[test]
fn observed_requests_coalesce_and_clear_without_creating_a_yield() {
    let [vm, tenant, account] = accounts();
    let token = PreemptionToken::new_observed(&account).unwrap();
    let baseline = account.used();
    assert_eq!(token.observation(), Some(PreemptionObservation::default()));
    let before = std::time::Instant::now();
    token.request();
    let first = token.observation().unwrap().pending_since.unwrap();
    assert!(first >= before && first <= std::time::Instant::now());
    token.request();
    assert_eq!(token.observation().unwrap().pending_since, Some(first));
    assert_eq!(token.take_request(), Some(first));
    let consumed = token.observation().unwrap();
    assert_eq!(consumed.pending_since, None);
    assert_eq!(consumed.last_yield, None);
    assert_eq!(
        consumed.counters,
        PreemptionCounters {
            requests: 2,
            coalesced: 1,
            consumed: 1,
            ..PreemptionCounters::default()
        }
    );
    token.request();
    token.clear();
    let cleared = token.observation().unwrap();
    assert_eq!(cleared.pending_since, None);
    assert_eq!(cleared.last_yield, None);
    assert_eq!(
        cleared.counters,
        PreemptionCounters {
            requests: 3,
            coalesced: 1,
            consumed: 1,
            cleared: 1,
            ..PreemptionCounters::default()
        }
    );
    assert_eq!(account.used(), baseline);
    drop((token, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn observed_yield_is_confirmed_after_coroutine_returns_to_caller() {
    let [vm, tenant, account] = accounts();
    let token = PreemptionToken::new_observed(&account).unwrap();
    let mut process = module().instantiate().unwrap();
    let mut execution = process
        .start_controlled("Bump", &[7], 4, &CancellationToken::default(), &token)
        .unwrap();
    token.request();
    let requested = token.observation().unwrap().pending_since.unwrap();
    assert_eq!(token.observation().unwrap().last_yield, None);
    assert_eq!(execution.resume(100), Ok(ExecutionStatus::Suspended));
    let first = token.observation().unwrap();
    assert_eq!(first.pending_since, None);
    let first_yield = first.last_yield.unwrap();
    assert_eq!(first_yield.requested_at, requested);
    assert!(first_yield.yielded_at <= std::time::Instant::now());
    assert_eq!(first_yield.latency(), first_yield.yielded_at - requested);
    assert_eq!(execution.snapshot().consumed_steps(), 0);
    token.request();
    assert_eq!(token.observation().unwrap().last_yield, first.last_yield);
    assert_eq!(execution.resume(100), Ok(ExecutionStatus::Suspended));
    let second = token.observation().unwrap().last_yield.unwrap();
    assert!(second.requested_at >= first_yield.yielded_at);
    assert!(second.yielded_at >= second.requested_at);
    assert_eq!(
        execution.resume(100),
        Ok(ExecutionStatus::Complete(Some(7)))
    );
    assert_eq!(token.observation().unwrap().last_yield, Some(second));
    token.request();
    token.clear();
    assert_eq!(token.observation().unwrap().last_yield, Some(second));
    drop(execution);
    drop((process, token, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn step_suspension_does_not_invent_an_atomic_request() {
    let [vm, tenant, account] = accounts();
    let token = PreemptionToken::new_observed(&account).unwrap();
    let mut process = module().instantiate().unwrap();
    let mut execution = process
        .start_controlled("Bump", &[7], 4, &CancellationToken::default(), &token)
        .unwrap();
    assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
    assert_eq!(token.observation(), Some(PreemptionObservation::default()));
    drop(execution);
    drop((process, token, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn cancellation_and_budget_return_without_claiming_atomic_yield() {
    for cancelled in [false, true] {
        let [vm, tenant, account] = accounts();
        let token = PreemptionToken::new_observed(&account).unwrap();
        let cancellation = CancellationToken::default();
        if cancelled {
            cancellation.cancel();
        }
        let mut process = module().instantiate().unwrap();
        let mut execution = process
            .start_controlled("Bump", &[7], 0, &cancellation, &token)
            .unwrap();
        token.request();
        let requested = token.observation().unwrap().pending_since;
        assert!(execution.resume(100).is_err());
        assert_eq!(token.observation().unwrap().last_yield, None);
        assert_eq!(token.observation().unwrap().pending_since, requested);
        token.clear();
        drop(execution);
        drop((process, token, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn remote_request_is_observed_and_clones_keep_the_fixed_charge() {
    let [vm, tenant, account] = accounts();
    let token = PreemptionToken::new_observed(&account).unwrap();
    let remote = token.clone();
    std::thread::spawn(move || remote.request()).join().unwrap();
    let mut process = module().instantiate().unwrap();
    let mut execution = process
        .start_controlled("Get", &[], 1, &CancellationToken::default(), &token)
        .unwrap();
    assert_eq!(execution.resume(100), Ok(ExecutionStatus::Suspended));
    assert!(token.observation().unwrap().last_yield.is_some());
    drop(token);
    assert!(account.used().bytes >= 4096);
    assert_eq!(
        execution.resume(100),
        Ok(ExecutionStatus::Complete(Some(0)))
    );
    assert_eq!(account.used(), Resources::default());
    drop(execution);
    drop(process);
    assert_eq!(account.used(), Resources::default());
    drop((account, tenant));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn observer_admission_at_all_three_levels_rolls_back() {
    for level in [
        ResourceLevel::Process,
        ResourceLevel::Tenant,
        ResourceLevel::Vm,
    ] {
        let [vm, tenant, account] = accounts();
        let limited = match level {
            ResourceLevel::Process => &account,
            ResourceLevel::Tenant => &tenant,
            ResourceLevel::Vm => &vm,
        };
        let fill = limited
            .reserve(Resources {
                bytes: limited.limit().bytes - limited.used().bytes - 4095,
                ..Resources::default()
            })
            .unwrap();
        let before = [vm.used(), tenant.used(), account.used()];
        assert!(PreemptionToken::new_observed(&account).is_err());
        assert_eq!([vm.used(), tenant.used(), account.used()], before);
        drop(fill);
        let token = PreemptionToken::new_observed(&account).unwrap();
        drop((token, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}

unsafe extern "C" fn effect_without_checkpoint(
    state: i64,
    _arguments: *const i64,
    output: *mut ScalarOutput,
    context: *mut ScalarContext,
) -> u32 {
    let request = wings_abi::EffectRequest {
        operation: wings_abi::EFFECT_SELF_PID,
        callback: 0,
        timeout_ms: 0,
        record_mask: 0,
        list_mask: 0,
        handler: 0,
        target: 0,
        arguments: std::ptr::null(),
        count: 0,
    };
    let mut ignored = 0;
    let status = unsafe { ((*context).effect_call.unwrap())(context, &request, &mut ignored) };
    unsafe { *output = ScalarOutput { state, reply: 0 } };
    status
}

#[test]
fn effect_suspension_does_not_consume_or_confirm_atomic_request() {
    let [vm, tenant, account] = accounts();
    let token = PreemptionToken::new_observed(&account).unwrap();
    let mut module = module();
    Arc::get_mut(&mut module.inner).unwrap().entries[3] = effect_without_checkpoint;
    let mut task = ScalarTask::new(module.instantiate().unwrap());
    task.enable_effects();
    task.start(
        &module.handler("Ack").unwrap(),
        &[],
        100,
        &CancellationToken::default(),
        &token,
        &StackPool::new(1, 0).unwrap(),
    )
    .unwrap();
    token.request();
    assert_eq!(task.resume(100), Ok(ExecutionStatus::Suspended));
    assert!(task.take_effect().is_some());
    assert_eq!(token.observation().unwrap().last_yield, None);
    assert!(token.observation().unwrap().pending_since.is_some());
    task.acknowledge_effect(STATUS_OK, 0);
    assert_eq!(task.resume(100), Ok(ExecutionStatus::Complete(Some(0))));
    assert_eq!(token.observation().unwrap().last_yield, None);
    token.clear();
    drop((task, token, account, tenant));
    assert_eq!(vm.used(), Resources::default());
}
