use std::sync::{Arc, Barrier};
use std::thread;

use wings_vm::{Reservation, ResourceAccount, Resources};

fn resources(values: [u64; 4]) -> Resources {
    let [bytes, processes, messages, requests] = values;
    Resources {
        bytes,
        processes,
        messages,
        requests,
    }
}

fn assert_send_sync<Value: Send + Sync>() {}

#[test]
fn accounts_and_unique_reservations_support_safe_cross_thread_ownership() {
    assert_send_sync::<ResourceAccount>();
    assert_send_sync::<Reservation>();
    let vm = ResourceAccount::vm(resources([8; 4]));
    let tenant = vm.tenant(resources([8; 4])).unwrap();
    let process = tenant.process(resources([8; 4])).unwrap();
    let ticket = process.reserve(resources([8; 4])).unwrap();
    drop((tenant, process));
    thread::spawn(move || drop(ticket)).join().unwrap();
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn concurrent_contenders_cannot_exceed_any_dimension_at_any_level() {
    for level in 0..3 {
        for dimension in 0..4 {
            let mut limits = [resources([64; 4]); 3];
            let mut constrained = [64; 4];
            constrained[dimension] = 4;
            limits[level] = resources(constrained);
            let vm = ResourceAccount::vm(limits[0]);
            let tenant = vm.tenant(limits[1]).unwrap();
            let process = tenant.process(limits[2]).unwrap();
            let mut amount = [0; 4];
            amount[dimension] = 1;
            let barrier = Barrier::new(9);
            thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        let process = &process;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            let ticket = process.reserve(resources(amount));
                            barrier.wait();
                            barrier.wait();
                            ticket
                        })
                    })
                    .collect();
                barrier.wait();
                barrier.wait();
                let used = [vm.used(), tenant.used(), process.used()];
                barrier.wait();
                let results: Vec<_> = handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect();
                assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 4);
                amount[dimension] = 4;
                assert_eq!(used, [resources(amount); 3]);
                for error in results.iter().filter_map(|result| result.as_ref().err()) {
                    assert!(error.contains(&format!(
                        "{:?}",
                        [vm.level(), tenant.level(), process.level()][level]
                    )));
                }
                drop(results);
            });
            assert_eq!(vm.used(), Resources::default());
            assert_eq!(tenant.used(), Resources::default());
            assert_eq!(process.used(), Resources::default());
        }
    }
}

#[test]
fn concurrent_overflow_checks_preserve_all_levels_and_dimensions() {
    for dimension in 0..4 {
        let vm = ResourceAccount::vm(resources([u64::MAX; 4]));
        let tenant = vm.tenant(resources([u64::MAX; 4])).unwrap();
        let process = tenant.process(resources([u64::MAX; 4])).unwrap();
        // Bytes, messages, and requests share one packed counter word, so their
        // widest representable reservation is the account's (saturated) limit,
        // not u64::MAX. Processes keeps the full width.
        let widest = [
            vm.limit().bytes,
            u64::MAX,
            vm.limit().messages,
            vm.limit().requests,
        ];
        let mut amount = [0; 4];
        amount[dimension] = widest[dimension];
        let held = process.reserve(resources(amount)).unwrap();
        let mut increment = [0; 4];
        increment[dimension] = 1;
        thread::scope(|scope| {
            for account in [&vm, &tenant, &process] {
                scope.spawn(move || {
                    for _ in 0..128 {
                        assert!(account.reserve(resources(increment)).is_err());
                    }
                });
            }
        });
        assert_eq!(
            [vm.used(), tenant.used(), process.used()],
            [resources(amount); 3]
        );
        drop(held);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn concurrent_transfers_preserve_full_common_ancestors_without_double_admission() {
    let limit = resources([8; 4]);
    let vm = ResourceAccount::vm(limit);
    let first_tenant = vm.tenant(limit).unwrap();
    let second_tenant = vm.tenant(limit).unwrap();
    let sources: Vec<_> = (0..8)
        .map(|_| first_tenant.process(limit).unwrap())
        .collect();
    let targets: Vec<_> = (0..8)
        .map(|_| second_tenant.process(limit).unwrap())
        .collect();
    let tickets: Vec<_> = sources
        .iter()
        .map(|source| source.reserve(resources([1; 4])).unwrap())
        .collect();
    let barrier = Barrier::new(8);
    let returned = thread::scope(|scope| {
        let handles: Vec<_> = tickets
            .into_iter()
            .enumerate()
            .map(|(index, mut ticket)| {
                let route = [&targets[index], &sources[index], &first_tenant, &vm];
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..256 {
                        for target in route {
                            ticket.transfer(target).unwrap();
                        }
                    }
                    ticket
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(vm.used(), limit);
    assert_eq!(first_tenant.used(), Resources::default());
    assert_eq!(second_tenant.used(), Resources::default());
    assert!(
        sources
            .iter()
            .chain(&targets)
            .all(|account| account.used() == Resources::default())
    );
    assert!(vm.reserve(resources([1; 4])).is_err());
    thread::spawn(move || drop(returned)).join().unwrap();
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn rejected_concurrent_transfers_keep_source_owner_and_cross_vm_isolation() {
    let vm = ResourceAccount::vm(resources([16; 4]));
    let first = vm.tenant(resources([8; 4])).unwrap();
    let second = vm.tenant(resources([8; 4])).unwrap();
    let blocker = second.reserve(resources([8; 4])).unwrap();
    let sources: Vec<_> = (0..8)
        .map(|_| first.process(resources([1; 4])).unwrap())
        .collect();
    let tickets: Vec<_> = sources
        .iter()
        .map(|source| source.reserve(resources([1; 4])).unwrap())
        .collect();
    let foreign = ResourceAccount::vm(resources([u64::MAX; 4]));
    thread::scope(|scope| {
        let handles: Vec<_> = tickets
            .into_iter()
            .map(|mut ticket| {
                let target = &second;
                let foreign = &foreign;
                scope.spawn(move || {
                    for _ in 0..128 {
                        assert!(ticket.transfer(target).is_err());
                        assert!(ticket.transfer(foreign).is_err());
                    }
                    ticket
                })
            })
            .collect();
        let returned: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(vm.used(), resources([16; 4]));
        assert_eq!(first.used(), resources([8; 4]));
        assert!(
            sources
                .iter()
                .all(|source| source.used() == resources([1; 4]))
        );
        assert_eq!(foreign.used(), Resources::default());
        drop(returned);
    });
    assert_eq!(first.used(), Resources::default());
    drop(blocker);
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn concurrent_snapshots_never_observe_partially_committed_dimensions() {
    let vm = ResourceAccount::vm(resources([64; 4]));
    let tenant = vm.tenant(resources([64; 4])).unwrap();
    let barrier = Arc::new(Barrier::new(9));
    thread::scope(|scope| {
        for _ in 0..8 {
            let process = tenant.process(resources([8; 4])).unwrap();
            let barrier = barrier.clone();
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..1024 {
                    let ticket = process.reserve(resources([1; 4])).unwrap();
                    thread::yield_now();
                    drop(ticket);
                }
            });
        }
        barrier.wait();
        for _ in 0..8192 {
            let used = vm.used();
            assert_eq!(used.bytes, used.processes);
            assert_eq!(used.bytes, used.messages);
            assert_eq!(used.bytes, used.requests);
            assert!(used.bytes <= 8);
        }
    });
    assert_eq!(vm.used(), Resources::default());
    assert_eq!(tenant.used(), Resources::default());
}
