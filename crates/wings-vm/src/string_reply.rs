use std::fmt;

use wings_abi::{STATUS_HEAP_EXHAUSTED, STATUS_INVALID_STRING, ScalarContext};

use crate::message_values::BytePayload;
use crate::{BytesReply, ResourceAccount};

#[derive(Clone, PartialEq, Eq)]
pub struct StringReply {
    bytes: BytesReply,
}

pub(crate) enum BufferReply {
    Bytes(BytesReply),
    String(StringReply),
}

impl StringReply {
    pub(crate) fn from_payload(
        account: &ResourceAccount,
        mut payload: BytePayload,
        context: &mut ScalarContext,
    ) -> Result<Self, u32> {
        if payload.string_mask != 1 || payload.data.len() > crate::message_values::MAX_MESSAGE_BYTES
        {
            return Err(STATUS_INVALID_STRING);
        }
        let mut offset = 0;
        while offset < payload.data.len() {
            crate::execution::runtime_checkpoint(context, true)?;
            let end = (offset + 256).min(payload.data.len());
            let chunk = &payload.data[offset..end];
            let length = match std::str::from_utf8(chunk) {
                Ok(text) => text.len(),
                Err(error) if error.error_len().is_none() && end < payload.data.len() => {
                    std::str::from_utf8(&chunk[..error.valid_up_to()])
                        .map_err(|_| STATUS_INVALID_STRING)?
                        .len()
                }
                Err(_) => return Err(STATUS_INVALID_STRING),
            };
            offset += length;
        }
        payload.string_mask = 0;
        Ok(Self {
            bytes: BytesReply::from_payload(account, payload).map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        })
    }

    pub fn as_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(self.bytes.as_slice()) }
    }

    pub(crate) fn storage(&self) -> BytesReply {
        self.bytes.clone()
    }
}

impl fmt::Debug for StringReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StringReply")
            .field(&self.as_str())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResourceLevel, Resources};
    use std::sync::atomic::AtomicU32;

    fn context(flag: &AtomicU32) -> ScalarContext {
        ScalarContext {
            cancellation: flag,
            remaining_steps: 4096,
            yield_request: flag,
            slice_remaining: 4096,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::null_mut(),
            effect_call: None,
        }
    }

    fn payload(account: &ResourceAccount, text: &[u8]) -> BytePayload {
        let mut payload = BytePayload::new(account, 1, text.len()).unwrap();
        payload.string_mask = 1;
        payload.data.extend_from_slice(text);
        payload
    }

    #[test]
    fn string_results_validate_utf8_preserve_types_and_outlive_source_accounts() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<StringReply>();
        for text in [
            String::new(),
            "中😀\0".into(),
            "a".repeat(255) + "中😀",
            "😀".repeat(16384),
        ] {
            let limits = Resources {
                bytes: 1 << 20,
                ..Resources::default()
            };
            let vm = ResourceAccount::vm(limits);
            let tenant = vm.tenant(limits).unwrap();
            let process = tenant.process(limits).unwrap();
            let flag = AtomicU32::new(0);
            let mut context = context(&flag);
            assert!(
                BytesReply::from_payload(&process, payload(&process, text.as_bytes())).is_err()
            );
            assert_eq!(vm.used(), Resources::default());
            let result = StringReply::from_payload(
                &process,
                payload(&process, text.as_bytes()),
                &mut context,
            )
            .unwrap();
            let cloned = result.clone();
            let charged = vm.used();
            assert!(charged.bytes >= text.len() as u64);
            assert_eq!(result.as_str().as_ptr(), cloned.as_str().as_ptr());
            drop((result, process, tenant));
            assert_eq!(cloned.as_str(), text);
            assert_eq!(vm.used(), charged);
            drop(cloned);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn string_result_invalid_encoding_budget_cancel_and_control_rejection_release_storage() {
        let limits = Resources {
            bytes: 1 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant(limits).unwrap();
        let process = tenant.process(limits).unwrap();
        let flag = AtomicU32::new(0);
        let mut context = context(&flag);
        for invalid in [
            vec![255],
            vec![0xf0, 0x9f],
            vec![b'a'; 255].into_iter().chain([0xe4, b'a']).collect(),
        ] {
            assert_eq!(
                StringReply::from_payload(&process, payload(&process, &invalid), &mut context)
                    .err(),
                Some(STATUS_INVALID_STRING)
            );
            assert_eq!(vm.used(), Resources::default());
        }
        let text = vec![97; 65536];
        for budget in [0, 1, 64, 128, 255] {
            context.remaining_steps = budget;
            assert_eq!(
                StringReply::from_payload(&process, payload(&process, &text), &mut context).err(),
                Some(wings_abi::STATUS_BUDGET_EXHAUSTED)
            );
            assert_eq!(vm.used(), Resources::default());
        }
        context.remaining_steps = 4096;
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            StringReply::from_payload(&process, payload(&process, &text), &mut context).err(),
            Some(wings_abi::STATUS_CANCELLED)
        );
        assert_eq!(vm.used(), Resources::default());
        flag.store(0, std::sync::atomic::Ordering::SeqCst);
        for account in [&vm, &tenant] {
            assert_ne!(account.level(), ResourceLevel::Process);
            assert!(
                StringReply::from_payload(account, payload(account, b"hello"), &mut context)
                    .is_err()
            );
            assert_eq!(vm.used(), Resources::default());
        }
        for account in [&vm, &tenant, &process] {
            let data = payload(&process, b"hello");
            let guard = account
                .reserve(Resources {
                    bytes: limits.bytes - 5,
                    ..Resources::default()
                })
                .unwrap();
            assert_eq!(
                StringReply::from_payload(&process, data, &mut context).err(),
                Some(STATUS_HEAP_EXHAUSTED)
            );
            drop(guard);
            assert_eq!(vm.used(), Resources::default());
        }
    }
}
