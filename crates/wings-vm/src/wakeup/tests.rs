use std::io;
use std::time::Duration;

use super::{WAIT_CONTROL_BYTES, WakeEndpoint, WorkerParker, wait_with_timeout};
use crate::{ResourceAccount, Resources};

fn bytes(amount: u64) -> Resources {
    Resources {
        bytes: amount,
        ..Resources::default()
    }
}

#[test]
fn fixed_wait_charge_covers_logical_endpoint_waiter_and_arc_storage() {
    assert!(
        (std::mem::size_of::<WakeEndpoint>()
            + std::mem::size_of::<WorkerParker>()
            + 2 * std::mem::size_of::<usize>()) as u64
            <= WAIT_CONTROL_BYTES
    );
}

#[test]
fn accounted_waiter_admits_before_os_creation_at_each_resource_level() {
    for restricted in 0..3 {
        for available in [0, WAIT_CONTROL_BYTES - 1, WAIT_CONTROL_BYTES] {
            let vm = ResourceAccount::vm_accounted(bytes(1 << 20)).unwrap();
            let baseline = vm.used();
            let tenant = vm.tenant_accounted(bytes(1 << 19)).unwrap();
            let process = tenant.process_accounted(bytes(1 << 18)).unwrap();
            let accounts = [&vm, &tenant, &process];
            let owner = accounts[restricted];
            let filler = owner
                .reserve(bytes(owner.limit().bytes - owner.used().bytes - available))
                .unwrap();
            let before = accounts.map(ResourceAccount::used);
            let mut called = false;
            let result = WorkerParker::new_accounted_with(&process, |charge| {
                called = true;
                assert_eq!(owner.used().bytes, owner.limit().bytes);
                WorkerParker::create(charge)
            });
            assert_eq!(called, available == WAIT_CONTROL_BYTES);
            assert_eq!(result.is_ok(), available == WAIT_CONTROL_BYTES);
            if let Ok(mut parker) = result {
                let notifier = parker.notifier();
                notifier.notify().unwrap();
                assert!(parker.wait(Duration::ZERO).unwrap());
                drop((parker, notifier));
            }
            assert_eq!(accounts.map(ResourceAccount::used), before);
            drop(filler);
            drop((process, tenant));
            assert_eq!(vm.used(), baseline);
        }
    }
}

#[test]
fn os_creation_failure_releases_the_prepaid_wait_charge() {
    let vm = ResourceAccount::vm_accounted(bytes(1 << 20)).unwrap();
    let baseline = vm.used();
    let result = WorkerParker::new_accounted_with(&vm, |charge| {
        assert!(charge.is_some());
        assert_eq!(vm.used().bytes, baseline.bytes + WAIT_CONTROL_BYTES);
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    });
    assert!(result.err().unwrap().contains("create worker wait"));
    assert_eq!(vm.used(), baseline);
}

#[test]
fn last_notifier_keeps_its_endpoint_account_and_charge_across_threads() {
    let vm = ResourceAccount::vm_accounted(bytes(1 << 20)).unwrap();
    let baseline = vm.used();
    let tenant = vm.tenant_accounted(bytes(1 << 19)).unwrap();
    let process = tenant.process_accounted(bytes(1 << 18)).unwrap();
    let parker = WorkerParker::new_accounted(&process).unwrap();
    let notifier = parker.notifier();
    let endpoint = std::sync::Arc::downgrade(&notifier.endpoint);
    let charged = vm.used();
    assert_eq!(process.used(), bytes(WAIT_CONTROL_BYTES));
    let retained = notifier.clone();
    drop((parker, notifier, process, tenant));
    assert_eq!(vm.used(), charged);
    retained.notify().unwrap();
    std::thread::spawn(move || drop(retained)).join().unwrap();
    assert!(endpoint.upgrade().is_none());
    assert_eq!(vm.used(), baseline);
}

#[test]
fn wait_helper_retries_interruptions_and_early_timeouts() {
    let mut calls = 0;
    assert!(
        wait_with_timeout(Duration::from_secs(1), |_| {
            calls += 1;
            match calls {
                1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                2 => Ok(false),
                _ => Ok(true),
            }
        })
        .unwrap()
    );
    assert_eq!(calls, 3);
}

#[test]
fn wait_helper_zero_timeout_still_polls_and_propagates_errors() {
    let mut calls = 0;
    assert!(
        !wait_with_timeout(Duration::ZERO, |_| {
            calls += 1;
            Ok(false)
        })
        .unwrap()
    );
    assert_eq!(calls, 1);
    assert!(wait_with_timeout(Duration::ZERO, |_| Ok(true)).unwrap());
    let error = wait_with_timeout(Duration::from_secs(1), |_| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let error = wait_with_timeout(Duration::MAX, |_| {
        panic!("overflow must reject before polling")
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
