use wings_vm::{Reservation, ResourceAccount, ResourceLevel, Resources};

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 8,
        messages: 16,
        requests: 4,
    }
}

fn bytes(amount: u64) -> Resources {
    Resources {
        bytes: amount,
        ..Resources::default()
    }
}

fn resources(amount: [u64; 4]) -> Resources {
    let [bytes, processes, messages, requests] = amount;
    Resources {
        bytes,
        processes,
        messages,
        requests,
    }
}

struct ModelAccount {
    handle: ResourceAccount,
    parent: Option<usize>,
    vm: usize,
    limit: [u64; 4],
}

#[derive(Clone, Copy)]
struct Charge {
    owner: usize,
    amount: [u64; 4],
}

fn model_accounts() -> Vec<ModelAccount> {
    let mut accounts = Vec::with_capacity(14);
    for vm in 0..2 {
        let vm_limit = [120, 24, 40, 16];
        let root = ResourceAccount::vm(resources(vm_limit));
        let root_index = accounts.len();
        accounts.push(ModelAccount {
            handle: root.clone(),
            parent: None,
            vm,
            limit: vm_limit,
        });
        for _ in 0..2 {
            let tenant_limit = [70, 16, 24, 10];
            let tenant = root.tenant(resources(tenant_limit)).expect("model tenant");
            let tenant_index = accounts.len();
            accounts.push(ModelAccount {
                handle: tenant.clone(),
                parent: Some(root_index),
                vm,
                limit: tenant_limit,
            });
            for _ in 0..2 {
                let process_limit = [45, 10, 16, 6];
                accounts.push(ModelAccount {
                    handle: tenant
                        .process(resources(process_limit))
                        .expect("model process"),
                    parent: Some(tenant_index),
                    vm,
                    limit: process_limit,
                });
            }
        }
    }
    accounts
}

fn model_usage(accounts: &[ModelAccount], charges: &[Option<Charge>]) -> Vec<[u64; 4]> {
    let mut usage = vec![[0; 4]; accounts.len()];
    for charge in charges.iter().flatten() {
        for (total, amount) in usage[charge.owner].iter_mut().zip(charge.amount) {
            *total += amount;
        }
    }
    for account_index in (0..accounts.len()).rev() {
        if let Some(parent) = accounts[account_index].parent {
            let child_usage = usage[account_index];
            for (total, amount) in usage[parent].iter_mut().zip(child_usage) {
                *total += amount;
            }
        }
    }
    usage
}

fn model_admits(accounts: &[ModelAccount], charges: &[Option<Charge>]) -> bool {
    model_usage(accounts, charges)
        .iter()
        .zip(accounts)
        .all(|(used, account)| {
            used.iter()
                .zip(account.limit)
                .all(|(used, limit)| *used <= limit)
        })
}

fn snapshot(accounts: &[ModelAccount]) -> Vec<Resources> {
    accounts
        .iter()
        .map(|account| account.handle.used())
        .collect()
}

fn verify_model(
    accounts: &[ModelAccount],
    charges: &[Option<Charge>],
    tickets: &[Option<Reservation>],
    step: usize,
) {
    assert!(
        model_admits(accounts, charges),
        "model limit at step {step}"
    );
    for (account_index, (account, expected)) in accounts
        .iter()
        .zip(model_usage(accounts, charges))
        .enumerate()
    {
        assert_eq!(account.handle.limit(), resources(account.limit));
        assert_eq!(
            account.handle.used(),
            resources(expected),
            "account {account_index} at step {step}"
        );
    }
    for (slot, (ticket, charge)) in tickets.iter().zip(charges).enumerate() {
        assert_eq!(
            ticket.is_some(),
            charge.is_some(),
            "slot {slot} at step {step}"
        );
        if let (Some(ticket), Some(charge)) = (ticket, charge) {
            assert_eq!(
                ticket.amount(),
                resources(charge.amount),
                "slot {slot} at step {step}"
            );
        }
    }
}

fn next_choice(seed: &mut u64, count: usize) -> usize {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*seed >> 32) as usize % count
}

fn verify_deterministic_conservation() {
    const SLOTS: usize = 24;
    const STEPS: usize = 4096;
    let accounts = model_accounts();
    let mut charges = [None; SLOTS];
    let mut tickets: [Option<Reservation>; SLOTS] = std::array::from_fn(|_| None);
    let mut seed = 0x0014_5eed_cafe_beef;
    let mut reserve_successes = 0;
    let mut reserve_failures = 0;
    let mut transfer_successes = 0;
    let mut transfer_failures = 0;
    let mut drops = 0;
    verify_model(&accounts, &charges, &tickets, 0);
    for step in 1..=STEPS {
        let slot = next_choice(&mut seed, SLOTS);
        let target = next_choice(&mut seed, accounts.len());
        let before = snapshot(&accounts);
        let mut candidate = charges;
        if let Some(charge) = charges[slot] {
            if next_choice(&mut seed, 3) == 0 {
                drop(tickets[slot].take());
                charges[slot] = None;
                drops += 1;
            } else {
                candidate[slot] = Some(Charge {
                    owner: target,
                    ..charge
                });
                let admitted = accounts[charge.owner].vm == accounts[target].vm
                    && model_admits(&accounts, &candidate);
                let result = tickets[slot]
                    .as_mut()
                    .expect("live reservation")
                    .transfer(&accounts[target].handle);
                assert_eq!(result.is_ok(), admitted, "transfer at step {step}");
                if result.is_ok() {
                    charges = candidate;
                    transfer_successes += 1;
                } else {
                    assert_eq!(
                        snapshot(&accounts),
                        before,
                        "failed transfer at step {step}"
                    );
                    transfer_failures += 1;
                }
            }
        } else {
            let amount = if next_choice(&mut seed, 8) == 0 {
                [0; 4]
            } else {
                [
                    next_choice(&mut seed, 25) as u64,
                    next_choice(&mut seed, 6) as u64,
                    next_choice(&mut seed, 9) as u64,
                    next_choice(&mut seed, 4) as u64,
                ]
            };
            candidate[slot] = Some(Charge {
                owner: target,
                amount,
            });
            let admitted = model_admits(&accounts, &candidate);
            let result = accounts[target].handle.reserve(resources(amount));
            assert_eq!(result.is_ok(), admitted, "reserve at step {step}");
            match result {
                Ok(ticket) => {
                    tickets[slot] = Some(ticket);
                    charges = candidate;
                    reserve_successes += 1;
                }
                Err(_) => {
                    assert_eq!(snapshot(&accounts), before, "failed reserve at step {step}");
                    reserve_failures += 1;
                }
            }
        }
        verify_model(&accounts, &charges, &tickets, step);
    }
    assert!(reserve_successes > 0 && reserve_failures > 0);
    assert!(transfer_successes > 0 && transfer_failures > 0 && drops > 0);
    for slot in (0..SLOTS).rev() {
        drop(tickets[slot].take());
        charges[slot] = None;
        verify_model(&accounts, &charges, &tickets, STEPS + SLOTS - slot);
    }
    assert!(
        snapshot(&accounts)
            .iter()
            .all(|used| *used == Resources::default())
    );
}

fn verify_overflow() {
    let unlimited = resources([u64::MAX; 4]);
    let vm = ResourceAccount::vm(unlimited);
    let tenant = vm.tenant(unlimited).expect("overflow tenant");
    let process = tenant.process(unlimited).expect("overflow process");
    let accounts = [&vm, &tenant, &process];
    for dimension in 0..4 {
        let mut maximum = [0; 4];
        maximum[dimension] = u64::MAX;
        let mut increment = [0; 4];
        increment[dimension] = 1;
        for account in accounts {
            let ticket = account.reserve(resources(maximum)).expect("maximum");
            let before = accounts.map(ResourceAccount::used);
            let error = process
                .reserve(resources(increment))
                .err()
                .expect("overflow rejection");
            assert!(error.contains(&format!("{:?}", account.level())));
            assert_eq!(accounts.map(ResourceAccount::used), before);
            assert_eq!(ticket.amount(), resources(maximum));
            drop(ticket);
            assert_eq!(
                accounts.map(ResourceAccount::used),
                [Resources::default(); 3]
            );
        }
    }
}

fn main() {
    let vm = ResourceAccount::vm(limits(100));
    assert_eq!(vm.level(), ResourceLevel::Vm);
    assert!(vm.process(limits(100)).is_err());
    let first_tenant = vm.tenant(limits(80)).expect("tenant");
    let second_tenant = vm.tenant(limits(40)).expect("tenant");
    assert!(first_tenant.tenant(limits(100)).is_err());
    let first = first_tenant.process(limits(60)).expect("process");
    let second = first_tenant.process(limits(60)).expect("process");
    let third = second_tenant.process(limits(40)).expect("process");
    assert!(first.process(limits(100)).is_err());
    let mut ticket = first.reserve(bytes(60)).expect("reserve");
    assert_eq!(
        (
            first.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (60, 60, 60)
    );
    assert!(
        first
            .reserve(bytes(1))
            .err()
            .expect("process rejection")
            .contains("Process")
    );
    assert!(
        second
            .reserve(bytes(21))
            .err()
            .expect("tenant rejection")
            .contains("Tenant")
    );
    assert_eq!(second.used().bytes, 0);
    let extra = third.reserve(bytes(40)).expect("fill VM");
    assert!(
        vm.reserve(bytes(1))
            .err()
            .expect("VM rejection")
            .contains("Vm")
    );
    ticket
        .transfer(&second)
        .expect("shared ancestor already full");
    assert_eq!(
        (
            first.used().bytes,
            second.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 60, 60, 100)
    );
    assert!(ticket.transfer(&third).is_err());
    assert_eq!(
        (second.used().bytes, third.used().bytes, vm.used().bytes),
        (60, 40, 100)
    );
    ticket.transfer(&second).expect("same account");
    assert!(ticket.transfer(&ResourceAccount::vm(limits(100))).is_err());
    drop(extra);
    ticket.transfer(&vm).expect("ancestor transfer");
    assert_eq!(
        (
            second.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 0, 60)
    );
    drop(ticket);
    assert_eq!(vm.used(), Resources::default());
    let mut ticket = first.reserve(bytes(30)).expect("cross tenant source");
    ticket.transfer(&third).expect("cross tenant");
    assert_eq!(
        (
            first_tenant.used().bytes,
            second_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 30, 30)
    );
    assert_eq!(ticket.amount(), bytes(30));
    drop(ticket);
    let amount = Resources {
        bytes: 7,
        processes: 1,
        messages: 2,
        requests: 1,
    };
    let ticket = first.reserve(amount).expect("four dimensions");
    assert_eq!(vm.used(), amount);
    drop(ticket);
    let zero = first.reserve(Resources::default()).expect("zero");
    assert_eq!(vm.used(), Resources::default());
    drop(zero);
    for dimension in [
        Resources {
            processes: 9,
            ..Resources::default()
        },
        Resources {
            messages: 17,
            ..Resources::default()
        },
        Resources {
            requests: 5,
            ..Resources::default()
        },
    ] {
        assert!(first.reserve(dimension).is_err());
        assert_eq!(vm.used(), Resources::default());
    }
    verify_overflow();
    let observer = vm.clone();
    let ticket = first.reserve(bytes(11)).expect("survives handles");
    drop(first);
    drop(first_tenant);
    drop(vm);
    assert_eq!(observer.used().bytes, 11);
    drop(ticket);
    assert_eq!(observer.used(), Resources::default());
    verify_deterministic_conservation();
    println!(
        "Resource accounts verified: hierarchy, four limits, overflow, atomic rollback, net transfers, lifetime and 4096 deterministic conservation steps"
    );
}
