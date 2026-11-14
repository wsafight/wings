use std::fmt;
use std::sync::Arc;

use crate::message_values::MAX_LIST_ELEMENTS;
use crate::{Reservation, ResourceAccount, ResourceLevel, Resources};

#[derive(Clone)]
pub struct IntListReply {
    inner: Arc<Contents>,
}

struct Contents {
    values: Vec<i64>,
    _charge: Reservation,
}

impl IntListReply {
    pub(crate) fn copy(account: &ResourceAccount, values: &[i64]) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process || values.len() > MAX_LIST_ELEMENTS {
            return Err("list reply requires a process account and at most 64 integers".into());
        }
        let charge = account.reserve(Resources {
            bytes: (std::mem::size_of::<Contents>() + 128 + std::mem::size_of_val(values)) as u64,
            ..Resources::default()
        })?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(values.len())
            .map_err(|_| "list reply storage allocation failed")?;
        if owned.capacity() != values.len() {
            return Err("allocator changed list reply capacity".into());
        }
        owned.extend_from_slice(values);
        Ok(Self {
            inner: Arc::new(Contents {
                values: owned,
                _charge: charge,
            }),
        })
    }

    pub fn as_slice(&self) -> &[i64] {
        &self.inner.values
    }
}

impl PartialEq for IntListReply {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for IntListReply {}

impl fmt::Debug for IntListReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("IntListReply")
            .field(&self.as_slice())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(bytes: u64) -> Resources {
        Resources {
            bytes,
            ..Resources::default()
        }
    }

    #[test]
    fn list_reply_reserves_exact_bounded_storage_and_keeps_charge_through_clones() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<IntListReply>();
        let vm = ResourceAccount::vm(limits(1 << 20));
        let tenant = vm.tenant(limits(1 << 20)).unwrap();
        let process = tenant.process(limits(1 << 20)).unwrap();
        for count in [0, 1, 64] {
            let source = [i64::MIN; 64];
            let reply = IntListReply::copy(&process, &source[..count]).unwrap();
            assert_eq!(reply.as_slice(), &source[..count]);
            let expected = (std::mem::size_of::<Contents>() + 128 + count * 8) as u64;
            assert_eq!(process.used(), limits(expected));
            assert_eq!(tenant.used(), limits(expected));
            assert_eq!(vm.used(), limits(expected));
            let cloned = reply.clone();
            assert_eq!(reply, cloned);
            assert_eq!(reply.as_slice().as_ptr(), cloned.as_slice().as_ptr());
            drop(reply);
            assert_eq!(process.used(), limits(expected));
            drop(cloned);
            assert_eq!(vm.used(), Resources::default());
        }
        assert!(IntListReply::copy(&process, &[0; 65]).is_err());
        assert!(IntListReply::copy(&tenant, &[]).is_err());
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn list_reply_checks_each_account_level_before_copy_and_does_not_retain_owners() {
        let required = (std::mem::size_of::<Contents>() + 128 + 64 * 8) as u64;
        for denied in 0..3 {
            let vm = ResourceAccount::vm(limits(if denied == 0 { required - 1 } else { required }));
            let tenant = vm
                .tenant(limits(if denied == 1 { required - 1 } else { required }))
                .unwrap();
            let process = tenant
                .process(limits(if denied == 2 { required - 1 } else { required }))
                .unwrap();
            assert!(IntListReply::copy(&process, &[7; 64]).is_err());
            assert_eq!(vm.used(), Resources::default());
        }
        let vm = ResourceAccount::vm(limits(required));
        let tenant = vm.tenant(limits(required)).unwrap();
        let process = tenant.process(limits(required)).unwrap();
        let mut source = [7; 64];
        let reply = IntListReply::copy(&process, &source).unwrap();
        source.fill(99);
        drop((process, tenant));
        assert_eq!(reply.as_slice(), &[7; 64]);
        assert_eq!(vm.used(), limits(required));
        drop(reply);
        assert_eq!(vm.used(), Resources::default());
    }
}
