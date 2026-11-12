use wings_vm::{ResourceAccount, ResourceLevel, Resources};

fn resources(values: [u64; 4]) -> Resources {
    let [bytes, processes, messages, requests] = values;
    Resources {
        bytes,
        processes,
        messages,
        requests,
    }
}

fn hierarchy(limits: [Resources; 3]) -> [ResourceAccount; 3] {
    let vm = ResourceAccount::vm(limits[0]);
    let tenant = vm.tenant(limits[1]).unwrap();
    let process = tenant.process(limits[2]).unwrap();
    [vm, tenant, process]
}

fn usage(accounts: &[ResourceAccount]) -> Vec<Resources> {
    accounts.iter().map(ResourceAccount::used).collect()
}

#[test]
fn hierarchy_rejects_skipped_repeated_and_reversed_levels() {
    let limit = resources([10; 4]);
    let [vm, tenant, process] = hierarchy([limit; 3]);
    assert_eq!(vm.level(), ResourceLevel::Vm);
    assert_eq!(tenant.level(), ResourceLevel::Tenant);
    assert_eq!(process.level(), ResourceLevel::Process);
    assert!(vm.process(limit).is_err());
    assert!(tenant.tenant(limit).is_err());
    assert!(process.tenant(limit).is_err());
    assert!(process.process(limit).is_err());
    assert_eq!(usage(&[vm, tenant, process]), vec![Resources::default(); 3]);
}

#[test]
fn reservation_charges_all_dimensions_and_drop_releases_every_ancestor() {
    let limit = resources([100; 4]);
    let accounts = hierarchy([limit; 3]);
    let amount = resources([7, 2, 3, 4]);
    let ticket = accounts[2].reserve(amount).unwrap();
    assert_eq!(ticket.amount(), amount);
    assert_eq!(usage(&accounts), vec![amount; 3]);
    assert!(accounts.iter().all(|account| account.limit() == limit));
    drop(ticket);
    assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
}

#[test]
fn each_dimension_at_each_level_rejects_without_partial_charge() {
    for level in 0..3 {
        for dimension in 0..4 {
            let mut limits = [resources([20; 4]); 3];
            let mut constrained = [20; 4];
            constrained[dimension] = 3;
            limits[level] = resources(constrained);
            let accounts = hierarchy(limits);
            let held = accounts[2].reserve(resources([3; 4])).unwrap();
            let before = usage(&accounts);
            let mut extra = [0; 4];
            extra[dimension] = 1;
            let error = accounts[2].reserve(resources(extra)).err().unwrap();
            assert!(error.contains(&format!("{:?}", accounts[level].level())));
            assert_eq!(
                usage(&accounts),
                before,
                "level {level}, dimension {dimension}"
            );
            drop(held);
            assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
        }
    }
}

#[test]
fn checked_add_overflow_in_every_dimension_is_atomic() {
    for dimension in 0..4 {
        let accounts = hierarchy([resources([u64::MAX; 4]); 3]);
        let mut full = [0; 4];
        full[dimension] = u64::MAX;
        let held = accounts[2].reserve(resources(full)).unwrap();
        let before = usage(&accounts);
        let mut extra = [0; 4];
        extra[dimension] = 1;
        assert!(accounts[2].reserve(resources(extra)).is_err());
        assert_eq!(usage(&accounts), before);
        drop(held);
        assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
    }
}

#[test]
fn zero_reservation_succeeds_even_at_zero_limits() {
    let accounts = hierarchy([Resources::default(); 3]);
    let mut ticket = accounts[2].reserve(Resources::default()).unwrap();
    ticket.transfer(&accounts[0]).unwrap();
    assert_eq!(ticket.amount(), Resources::default());
    drop(ticket);
    assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
}

#[test]
fn cloned_account_observes_the_same_usage_and_keeps_ancestors_alive() {
    let limit = resources([10; 4]);
    let accounts = hierarchy([limit; 3]);
    let clone = accounts[2].clone();
    let ticket = clone.reserve(resources([1; 4])).unwrap();
    assert_eq!(accounts[2].used(), resources([1; 4]));
    drop(accounts);
    assert_eq!(clone.used(), resources([1; 4]));
    drop(ticket);
    assert_eq!(clone.used(), Resources::default());
}

#[test]
fn sibling_processes_share_tenant_limits_but_not_local_usage() {
    let [vm, tenant, first] = hierarchy([resources([10; 4]); 3]);
    let second = tenant.process(resources([10; 4])).unwrap();
    let ticket = first.reserve(resources([6; 4])).unwrap();
    assert_eq!(second.used(), Resources::default());
    assert!(second.reserve(resources([5; 4])).is_err());
    let other = second.reserve(resources([4; 4])).unwrap();
    assert_eq!(vm.used(), resources([10; 4]));
    assert_eq!(tenant.used(), resources([10; 4]));
    drop(ticket);
    assert_eq!(first.used(), Resources::default());
    assert_eq!(second.used(), resources([4; 4]));
    drop(other);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn transfer_to_self_does_not_readmit_full_account() {
    let accounts = hierarchy([resources([5; 4]); 3]);
    let mut ticket = accounts[2].reserve(resources([5; 4])).unwrap();
    ticket.transfer(&accounts[2].clone()).unwrap();
    assert_eq!(usage(&accounts), vec![resources([5; 4]); 3]);
    drop(ticket);
    assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
}

#[test]
fn transfer_between_siblings_does_not_double_charge_full_ancestors() {
    let [vm, tenant, first] = hierarchy([resources([5; 4]); 3]);
    let second = tenant.process(resources([5; 4])).unwrap();
    let mut ticket = first.reserve(resources([5; 4])).unwrap();
    ticket.transfer(&second).unwrap();
    assert_eq!(first.used(), Resources::default());
    assert_eq!(second.used(), resources([5; 4]));
    assert_eq!(tenant.used(), resources([5; 4]));
    assert_eq!(vm.used(), resources([5; 4]));
    drop(ticket);
    assert_eq!(
        usage(&[vm, tenant, first, second]),
        vec![Resources::default(); 4]
    );
}

#[test]
fn transfer_across_tenants_only_moves_the_nonshared_path() {
    let [vm, first_tenant, first] = hierarchy([resources([10; 4]); 3]);
    let second_tenant = vm.tenant(resources([10; 4])).unwrap();
    let second = second_tenant.process(resources([10; 4])).unwrap();
    let mut ticket = first.reserve(resources([10; 4])).unwrap();
    ticket.transfer(&second).unwrap();
    assert_eq!(first.used(), Resources::default());
    assert_eq!(first_tenant.used(), Resources::default());
    assert_eq!(second.used(), resources([10; 4]));
    assert_eq!(second_tenant.used(), resources([10; 4]));
    assert_eq!(vm.used(), resources([10; 4]));
    drop(ticket);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn transfer_to_ancestor_then_back_only_changes_descendant_charges() {
    let accounts = hierarchy([resources([5; 4]); 3]);
    let mut ticket = accounts[2].reserve(resources([5; 4])).unwrap();
    ticket.transfer(&accounts[0]).unwrap();
    assert_eq!(
        usage(&accounts),
        vec![
            resources([5; 4]),
            Resources::default(),
            Resources::default()
        ]
    );
    ticket.transfer(&accounts[2]).unwrap();
    assert_eq!(usage(&accounts), vec![resources([5; 4]); 3]);
    drop(ticket);
    assert_eq!(usage(&accounts), vec![Resources::default(); 3]);
}

#[test]
fn failed_transfer_preserves_original_owner_and_all_usage() {
    for dimension in 0..4 {
        let [vm, tenant, source] = hierarchy([resources([20; 4]); 3]);
        let mut target_limit = [20; 4];
        target_limit[dimension] = 4;
        let target = tenant.process(resources(target_limit)).unwrap();
        let accounts = [vm, tenant, source, target];
        let mut ticket = accounts[2].reserve(resources([5; 4])).unwrap();
        let before = usage(&accounts);
        assert!(ticket.transfer(&accounts[3]).is_err());
        assert_eq!(usage(&accounts), before);
        assert_eq!(ticket.amount(), resources([5; 4]));
        drop(ticket);
        assert_eq!(usage(&accounts), vec![Resources::default(); 4]);
    }
}

#[test]
fn target_tenant_rejection_does_not_charge_its_process() {
    let [vm, tenant, source] = hierarchy([resources([20; 4]); 3]);
    let other_tenant = vm.tenant(resources([4; 4])).unwrap();
    let target = other_tenant.process(resources([20; 4])).unwrap();
    let accounts = [vm, tenant, source, other_tenant, target];
    let mut ticket = accounts[2].reserve(resources([5; 4])).unwrap();
    let before = usage(&accounts);
    assert!(
        ticket
            .transfer(&accounts[4])
            .unwrap_err()
            .contains("Tenant")
    );
    assert_eq!(usage(&accounts), before);
    drop(ticket);
    assert_eq!(usage(&accounts), vec![Resources::default(); 5]);
}

#[test]
fn cross_vm_transfer_is_rejected_even_for_zero_reservations() {
    let first = hierarchy([resources([10; 4]); 3]);
    let second = hierarchy([resources([10; 4]); 3]);
    for amount in [Resources::default(), resources([1; 4])] {
        let mut ticket = first[2].reserve(amount).unwrap();
        let before = usage(&first);
        assert!(ticket.transfer(&second[2]).unwrap_err().contains("same VM"));
        assert_eq!(usage(&first), before);
        assert_eq!(usage(&second), vec![Resources::default(); 3]);
        drop(ticket);
    }
    assert_eq!(usage(&first), vec![Resources::default(); 3]);
}

#[test]
fn reservations_can_outlive_their_original_account_handles() {
    let vm = ResourceAccount::vm(resources([10; 4]));
    let ticket = {
        let tenant = vm.tenant(resources([10; 4])).unwrap();
        let process = tenant.process(resources([10; 4])).unwrap();
        process.reserve(resources([3; 4])).unwrap()
    };
    assert_eq!(vm.used(), resources([3; 4]));
    drop(ticket);
    assert_eq!(vm.used(), Resources::default());
}
