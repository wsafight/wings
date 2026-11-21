use std::fmt;
use std::sync::Arc;

use wings_abi::{STATUS_HEAP_EXHAUSTED, STATUS_INVALID_STRING, ScalarContext, ValueType};

use crate::message_values::{BytePayload, MAX_LIST_ELEMENTS, MAX_MESSAGE_BYTES};
use crate::{RecordReply, Reservation, ResourceAccount, ResourceLevel, Resources};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyValue {
    Int(i64),
    Bool(bool),
    Unit,
    Pid(i64),
    IntList(IntListReply),
    Bytes(BytesReply),
    String(StringReply),
    Record(RecordReply),
}

impl ReplyValue {
    pub fn from_scalar(reply_type: ValueType, raw: i64) -> Result<Self, &'static str> {
        match reply_type {
            ValueType::Int => Ok(Self::Int(raw)),
            ValueType::Bool if matches!(raw, 0 | 1) => Ok(Self::Bool(raw != 0)),
            ValueType::Unit if raw == 0 => Ok(Self::Unit),
            ValueType::Pid if raw >= 0 => Ok(Self::Pid(raw)),
            _ => Err("invalid scalar reply value"),
        }
    }
}

#[derive(Clone)]
pub struct BytesReply {
    inner: Arc<ByteContents>,
}

struct ByteContents {
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
            || payload.string_mask != 0
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
            bytes: (std::mem::size_of::<ByteContents>() + 128) as u64,
            ..Resources::default()
        })?;
        let storage = payload.take_charge().ok_or("unaccounted byte reply")?;
        Ok(Self {
            inner: Arc::new(ByteContents {
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
        if payload.string_mask != 1 || payload.data.len() > MAX_MESSAGE_BYTES {
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

#[derive(Clone)]
pub struct IntListReply {
    inner: Arc<ListContents>,
}

struct ListContents {
    values: Vec<i64>,
    _charge: Reservation,
}

impl IntListReply {
    pub(crate) fn copy(account: &ResourceAccount, values: &[i64]) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process || values.len() > MAX_LIST_ELEMENTS {
            return Err("list reply requires a process account and at most 64 integers".into());
        }
        let charge = account.reserve(Resources {
            bytes: (std::mem::size_of::<ListContents>() + 128 + std::mem::size_of_val(values))
                as u64,
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
            inner: Arc::new(ListContents {
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
    use std::sync::atomic::AtomicU32;

    fn limits(bytes: u64) -> Resources {
        Resources {
            bytes,
            ..Resources::default()
        }
    }

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
    fn byte_reply_storage_and_control_are_admitted_at_all_three_levels() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<BytesReply>();
        for length in [0, 1, 65536] {
            let required = (length + std::mem::size_of::<ByteContents>() + 128) as u64;
            for restricted in 0..3 {
                let level_limits = |level| Resources {
                    bytes: required - u64::from(level == restricted),
                    ..Resources::default()
                };
                let vm = ResourceAccount::vm(level_limits(0));
                let tenant = vm.tenant(level_limits(1)).unwrap();
                let process = tenant.process(level_limits(2)).unwrap();
                let mut payload = BytePayload::new(&process, 1, length).unwrap();
                payload.data.resize(length, 255);
                assert!(BytesReply::from_payload(&process, payload).is_err());
                assert_eq!(vm.used(), Resources::default());
            }
            let vm = ResourceAccount::vm(limits(required));
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
            let vm = ResourceAccount::vm(limits(1 << 20));
            let tenant = vm.tenant(limits(1 << 20)).unwrap();
            let process = tenant.process(limits(1 << 20)).unwrap();
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
        let cap = limits(1 << 20);
        let vm = ResourceAccount::vm(cap);
        let tenant = vm.tenant(cap).unwrap();
        let process = tenant.process(cap).unwrap();
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
                    bytes: cap.bytes - 5,
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
            let expected = (std::mem::size_of::<ListContents>() + 128 + count * 8) as u64;
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
        let required = (std::mem::size_of::<ListContents>() + 128 + 64 * 8) as u64;
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
        source.fill(0);
        drop((process, tenant));
        assert_eq!(reply.as_slice(), &[7; 64]);
        assert_eq!(vm.used(), limits(required));
        drop(reply);
        assert_eq!(vm.used(), Resources::default());
    }
}
