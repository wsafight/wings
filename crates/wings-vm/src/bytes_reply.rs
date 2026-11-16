use std::fmt;
use std::sync::Arc;

use crate::message_values::BytePayload;
use crate::{Reservation, ResourceAccount, ResourceLevel, Resources};

#[derive(Clone)]
pub struct BytesReply {
    inner: Arc<Contents>,
}

struct Contents {
    bytes: Vec<u8>,
    _storage: Reservation,
    _control: Reservation,
}

impl BytesReply {
    pub(crate) fn from_payload(
        account: &ResourceAccount,
        mut payload: BytePayload,
    ) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process
            || payload.mask != 1
            || !payload.valid(
                &[crate::message_values::ListPayload::encode(
                    0,
                    payload.data.len(),
                )],
                1,
            )
        {
            return Err("invalid byte reply payload".into());
        }
        let control = account.reserve(Resources {
            bytes: (std::mem::size_of::<Contents>() + 128) as u64,
            ..Resources::default()
        })?;
        let storage = payload.take_charge().ok_or("unaccounted byte reply")?;
        Ok(Self {
            inner: Arc::new(Contents {
                bytes: payload.data,
                _storage: storage,
                _control: control,
            }),
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.inner.bytes
    }
}

impl PartialEq for BytesReply {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for BytesReply {}

impl fmt::Debug for BytesReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BytesReply")
            .field(&self.as_slice())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_reply_storage_and_control_are_admitted_at_all_three_levels() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<BytesReply>();
        for length in [0, 1, 65536] {
            let required = (length + std::mem::size_of::<Contents>() + 128) as u64;
            for restricted in 0..3 {
                let limits = |level| Resources {
                    bytes: required - u64::from(level == restricted),
                    ..Resources::default()
                };
                let vm = ResourceAccount::vm(limits(0));
                let tenant = vm.tenant(limits(1)).unwrap();
                let process = tenant.process(limits(2)).unwrap();
                let mut payload = BytePayload::new(&process, 1, length).unwrap();
                payload.data.resize(length, 255);
                assert!(BytesReply::from_payload(&process, payload).is_err());
                assert_eq!(vm.used(), Resources::default());
            }
            let vm = ResourceAccount::vm(Resources {
                bytes: required,
                ..Resources::default()
            });
            let tenant = vm.tenant(vm.limit()).unwrap();
            let process = tenant.process(vm.limit()).unwrap();
            let mut payload = BytePayload::new(&process, 1, length).unwrap();
            payload.data.resize(length, 17);
            let reply = BytesReply::from_payload(&process, payload).unwrap();
            let cloned = reply.clone();
            assert_eq!(vm.used().bytes, required);
            assert_eq!(reply.as_slice().as_ptr(), cloned.as_slice().as_ptr());
            drop((reply, process, tenant));
            assert_eq!(cloned.as_slice(), vec![17; length]);
            assert_eq!(vm.used().bytes, required);
            drop(cloned);
            assert_eq!(vm.used(), Resources::default());
        }
    }
}
