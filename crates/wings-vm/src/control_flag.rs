use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use crate::{Reservation, ResourceAccount, Resources};

#[derive(Default)]
pub(crate) struct UnaccountedFlag(AtomicU32);

#[derive(Clone)]
pub(crate) enum ControlFlag {
    Unaccounted(Arc<UnaccountedFlag>),
    Accounted(Arc<AccountedFlag>),
}

pub(crate) struct AccountedFlag {
    value: AtomicU32,
    _charge: Reservation,
}

impl Default for ControlFlag {
    fn default() -> Self {
        Self::Unaccounted(Arc::default())
    }
}

impl ControlFlag {
    pub(crate) const BYTES: u64 =
        (std::mem::size_of::<AccountedFlag>() + 2 * std::mem::size_of::<usize>()) as u64;
    #[cfg(test)]
    pub(crate) const UNACCOUNTED_BYTES: usize =
        std::mem::size_of::<UnaccountedFlag>() + 2 * std::mem::size_of::<usize>();

    pub(crate) fn new_accounted(account: &ResourceAccount) -> Result<Self, String> {
        let charge = account.reserve(Resources {
            bytes: Self::BYTES,
            ..Resources::default()
        })?;
        Ok(Self::Accounted(Arc::new(AccountedFlag {
            value: AtomicU32::new(0),
            _charge: charge,
        })))
    }
}

impl Deref for ControlFlag {
    type Target = AtomicU32;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Unaccounted(flag) => &flag.0,
            Self::Accounted(flag) => &flag.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, PreemptionToken};
    use std::sync::atomic::Ordering;

    fn bytes(amount: u64) -> Resources {
        Resources {
            bytes: amount,
            ..Resources::default()
        }
    }

    #[test]
    fn shared_flag_cost_includes_storage_and_arc_headers() {
        assert_eq!(
            ControlFlag::BYTES as usize,
            std::mem::size_of::<AccountedFlag>() + 2 * std::mem::size_of::<usize>()
        );
        assert!(ControlFlag::BYTES as usize >= ControlFlag::UNACCOUNTED_BYTES);
        assert_eq!(
            std::mem::align_of::<AccountedFlag>(),
            std::mem::align_of::<usize>()
        );
    }

    #[test]
    fn admission_at_each_level_checks_exact_cost_and_rolls_back() {
        for cancelled in [false, true] {
            for restricted in 0..3 {
                for remaining in [0, ControlFlag::BYTES - 1, ControlFlag::BYTES] {
                    let limits = bytes(1 << 20);
                    let vm = ResourceAccount::vm_accounted(limits).unwrap();
                    let baseline = vm.used();
                    let tenant = vm.tenant_accounted(bytes(1 << 19)).unwrap();
                    let account = tenant.process_accounted(bytes(1 << 18)).unwrap();
                    let accounts = [&account, &tenant, &vm];
                    let owner = accounts[restricted];
                    let fill = owner
                        .reserve(bytes(owner.limit().bytes - owner.used().bytes - remaining))
                        .unwrap();
                    let before = accounts.map(ResourceAccount::used);
                    if cancelled {
                        let result = CancellationToken::new_accounted(&account);
                        assert_eq!(result.is_ok(), remaining == ControlFlag::BYTES);
                        if let Ok(token) = result {
                            let cloned = token.clone();
                            cloned.cancel();
                            assert_eq!(token.flag.load(Ordering::SeqCst), 1);
                            assert_eq!(owner.used().bytes, owner.limit().bytes);
                        }
                    } else {
                        let result = PreemptionToken::new_accounted(&account);
                        assert_eq!(result.is_ok(), remaining == ControlFlag::BYTES);
                        if let Ok(token) = result {
                            let cloned = token.clone();
                            cloned.request();
                            assert_eq!(token.flag.load(Ordering::SeqCst), 1);
                            assert_eq!(token.observation(), None);
                            assert_eq!(owner.used().bytes, owner.limit().bytes);
                        }
                    }
                    assert_eq!(accounts.map(ResourceAccount::used), before);
                    drop(fill);
                    drop((account, tenant));
                    assert_eq!(vm.used(), baseline);
                }
            }
        }
    }

    #[test]
    fn clones_retain_accounted_flags_until_cross_thread_final_release() {
        let limits = bytes(1 << 20);
        let vm = ResourceAccount::vm_accounted(limits).unwrap();
        let baseline = vm.used();
        let tenant = vm.tenant_accounted(limits).unwrap();
        let account = tenant.process_accounted(limits).unwrap();
        let cancellation = CancellationToken::new_accounted(&account).unwrap();
        let preemption = PreemptionToken::new_accounted(&account).unwrap();
        let cancelled = cancellation.clone();
        let yielded = preemption.clone();
        let charged = vm.used();
        assert_eq!(account.used(), bytes(2 * ControlFlag::BYTES));
        drop((cancellation, preemption, account, tenant));
        assert_eq!(vm.used(), charged);
        std::thread::spawn(move || {
            cancelled.cancel();
            yielded.request();
            assert_eq!(cancelled.flag.load(Ordering::SeqCst), 1);
            assert_eq!(yielded.flag.load(Ordering::SeqCst), 1);
        })
        .join()
        .unwrap();
        assert_eq!(vm.used(), baseline);
    }
}
