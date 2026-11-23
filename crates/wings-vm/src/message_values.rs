use wings_abi::ValueType;

use crate::{Reservation, ResourceAccount, Resources};

pub(crate) const MAX_ARGUMENTS: usize = 64;
pub(crate) const MAX_LIST_ELEMENTS: usize = 64;
pub(crate) const MAX_MESSAGE_BYTES: usize = 65536;
pub(crate) const INLINE_MESSAGE_ARGUMENTS: usize = 4;

pub(crate) struct MessageArguments {
    inline: [i64; INLINE_MESSAGE_ARGUMENTS],
    overflow: Option<Box<[i64]>>,
    len: usize,
}

impl MessageArguments {
    pub(crate) fn from_inline(arguments: &[i64]) -> Self {
        assert!(arguments.len() <= INLINE_MESSAGE_ARGUMENTS);
        let mut inline = [0; INLINE_MESSAGE_ARGUMENTS];
        inline[..arguments.len()].copy_from_slice(arguments);
        Self {
            inline,
            overflow: None,
            len: arguments.len(),
        }
    }

    pub(crate) fn try_from_slice(arguments: &[i64]) -> Result<Self, &'static str> {
        if arguments.len() > MAX_ARGUMENTS {
            return Err("message argument limit exceeded");
        }
        if arguments.len() <= INLINE_MESSAGE_ARGUMENTS {
            return Ok(Self::from_inline(arguments));
        }
        let mut overflow = Vec::new();
        overflow
            .try_reserve_exact(arguments.len())
            .map_err(|_| "message argument allocation failed")?;
        if overflow.capacity() != arguments.len() {
            return Err("allocator changed message argument capacity");
        }
        overflow.extend_from_slice(arguments);
        Ok(Self {
            inline: [0; INLINE_MESSAGE_ARGUMENTS],
            overflow: Some(overflow.into_boxed_slice()),
            len: arguments.len(),
        })
    }

    pub(crate) fn overflow_bytes(argument_count: usize) -> u64 {
        if argument_count <= INLINE_MESSAGE_ARGUMENTS {
            0
        } else {
            argument_count.saturating_mul(std::mem::size_of::<i64>()) as u64
        }
    }

    pub(crate) fn as_slice(&self) -> &[i64] {
        self.overflow.as_deref().unwrap_or(&self.inline[..self.len])
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [i64] {
        if let Some(overflow) = self.overflow.as_deref_mut() {
            overflow
        } else {
            &mut self.inline[..self.len]
        }
    }
}

impl Default for MessageArguments {
    fn default() -> Self {
        Self::from_inline(&[])
    }
}

impl std::ops::Deref for MessageArguments {
    type Target = [i64];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

pub(crate) fn validate_scalar_types(
    types: &[ValueType],
    arguments: &[i64],
) -> Result<(), &'static str> {
    if arguments.len() != types.len() || arguments.len() > MAX_ARGUMENTS {
        return Err("handler argument count mismatch");
    }
    for (value, ty) in arguments.iter().zip(types) {
        if ty.is_reference() {
            return Err("reference payload mismatch");
        }
        if !ty.accepts(*value) {
            return Err("scalar argument mismatch");
        }
    }
    Ok(())
}

pub(crate) fn validate_scalar_arguments(
    parameters: &[(String, ValueType)],
    arguments: &[i64],
) -> Result<(), &'static str> {
    if arguments.len() != parameters.len() || arguments.len() > MAX_ARGUMENTS {
        return Err("handler argument count mismatch");
    }
    for (value, (_, ty)) in arguments.iter().zip(parameters) {
        if ty.is_reference() {
            return Err("reference payload mismatch");
        }
        if !ty.accepts(*value) {
            return Err("scalar argument mismatch");
        }
    }
    Ok(())
}

pub(crate) fn validate_message_payload(
    parameters: &[(String, ValueType)],
    arguments: &[i64],
    system: bool,
    allow_pid: bool,
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), &'static str> {
    if arguments.len() != parameters.len() || arguments.len() > MAX_ARGUMENTS {
        return Err("handler argument count mismatch");
    }
    let mut expected_list = 0u64;
    let mut expected_bytes = 0u64;
    let mut expected_strings = 0u64;
    for (index, (value, (_, ty))) in arguments.iter().zip(parameters).enumerate() {
        match ty {
            ValueType::ListRef => expected_list |= 1u64 << index,
            ValueType::BytesRef => expected_bytes |= 1u64 << index,
            ValueType::StringRef => {
                expected_bytes |= 1u64 << index;
                expected_strings |= 1u64 << index;
            }
            ValueType::RequestId if system && *value > 0 => {}
            ValueType::Pid if allow_pid && *value >= 0 => {}
            _ if ty.accepts(*value) => {}
            _ => return Err("scalar argument mismatch"),
        }
    }
    if !lists.valid(arguments, expected_list)
        || !bytes.valid(arguments, expected_bytes)
        || bytes.string_mask != expected_strings
    {
        return Err("reference payload mismatch");
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct BytePayload {
    pub mask: u64,
    pub string_mask: u64,
    pub data: Vec<u8>,
    charge: Option<Reservation>,
    reply: Option<crate::BytesReply>,
    record_reply: Option<crate::RecordReply>,
    completion: bool,
}

impl BytePayload {
    /// Shared empty payload for read-only access; callers that mutate must
    /// materialize their own value instead.
    #[cfg(test)]
    pub(crate) fn empty_ref() -> &'static Self {
        static EMPTY: BytePayload = BytePayload {
            mask: 0,
            string_mask: 0,
            data: Vec::new(),
            charge: None,
            reply: None,
            record_reply: None,
            completion: false,
        };
        &EMPTY
    }

    pub(crate) fn new(account: &ResourceAccount, mask: u64, length: usize) -> Result<Self, String> {
        if mask == 0 || length > MAX_MESSAGE_BYTES {
            return Err("invalid byte message capacity or mask".into());
        }
        let charge = account.reserve(Resources {
            bytes: length as u64,
            ..Resources::default()
        })?;
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .map_err(|error| error.to_string())?;
        if data.capacity() != length {
            return Err("allocator changed byte message capacity".into());
        }
        Ok(Self {
            mask,
            string_mask: 0,
            data,
            charge: Some(charge),
            reply: None,
            record_reply: None,
            completion: false,
        })
    }

    pub(crate) fn completion(reply: Option<crate::BytesReply>) -> Self {
        Self {
            mask: 4,
            reply,
            completion: true,
            ..Self::default()
        }
    }

    pub(crate) fn empty_completion(mask: u64, string_mask: u64) -> Self {
        Self {
            mask,
            string_mask,
            completion: true,
            ..Self::default()
        }
    }

    pub(crate) fn record_completion(
        reply: crate::RecordReply,
        mask: u64,
        string_mask: u64,
    ) -> Self {
        Self {
            mask,
            string_mask,
            record_reply: Some(reply),
            completion: true,
            ..Self::default()
        }
    }

    pub(crate) fn string_completion(reply: Option<crate::StringReply>) -> Self {
        let mut payload = Self::completion(reply.map(|reply| reply.storage()));
        payload.string_mask = 4;
        payload
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        self.reply
            .as_ref()
            .map(crate::BytesReply::as_slice)
            .or_else(|| self.record_reply.as_ref().map(crate::RecordReply::bytes))
            .unwrap_or(&self.data)
    }

    pub(crate) fn take_charge(&mut self) -> Option<Reservation> {
        self.charge.take()
    }

    pub(crate) fn is_trivially_empty(&self) -> bool {
        self.mask == 0
            && self.string_mask == 0
            && self.data.is_empty()
            && self.data.capacity() == 0
            && self.charge.is_none()
            && self.reply.is_none()
            && self.record_reply.is_none()
            && !self.completion
    }

    pub(crate) fn valid(&self, arguments: &[i64], expected_mask: u64) -> bool {
        if arguments.len() > 64
            || self.mask != expected_mask
            || self.string_mask & !self.mask != 0
            || self.as_slice().len() > MAX_MESSAGE_BYTES
            || self.data.capacity() != self.data.len()
            || (arguments.len() < 64 && self.mask >> arguments.len() != 0)
            || (self.completion
                && (!self.data.is_empty()
                    || self.charge.is_some()
                    || (self.reply.is_some()
                        && (self.record_reply.is_some()
                            || self.mask != 4
                            || !matches!(self.string_mask, 0 | 4)))
                    || self.record_reply.as_ref().is_some_and(|reply| {
                        self.mask != reply.bytes_mask() << 2
                            || self.string_mask != reply.string_mask() << 2
                    })))
            || (!self.completion && self.reply.is_some())
            || (!self.completion && self.record_reply.is_some())
            || (!self.completion
                && self.charge.as_ref().map(|charge| charge.amount().bytes)
                    != (self.mask != 0).then_some(self.data.len() as u64))
        {
            return false;
        }
        let mut cursor = 0;
        for (index, argument) in arguments.iter().enumerate() {
            if self.mask & (1u64 << index) != 0 {
                let (start, length) = ListPayload::segment(*argument);
                if start != cursor || length > self.as_slice().len() - cursor {
                    return false;
                }
                cursor += length;
            }
        }
        cursor == self.as_slice().len()
    }

    pub(crate) fn transfer_checked(
        &mut self,
        target: &ResourceAccount,
    ) -> Result<(), crate::VmError> {
        if self.completion {
            return Err(crate::VmError::Other(
                "shared byte completion cannot be transferred as a message",
            ));
        }
        if let Some(charge) = &mut self.charge {
            charge.transfer_checked(target)?;
        }
        Ok(())
    }
}

pub(crate) const INLINE_LIST_ELEMENTS: usize = 8;

#[derive(Clone)]
pub(crate) struct ListValues {
    inline: [i64; INLINE_LIST_ELEMENTS],
    overflow: Option<Vec<i64>>,
    pub(crate) len: usize,
}

impl ListValues {
    fn try_reserve(&mut self, additional: usize) -> Result<(), &'static str> {
        if self.len + additional > MAX_LIST_ELEMENTS {
            return Err("list element limit exceeded");
        }
        if let Some(overflow) = self.overflow.as_mut() {
            overflow
                .try_reserve(additional)
                .map_err(|_| "list element allocation failed")
        } else if self.len + additional <= INLINE_LIST_ELEMENTS {
            Ok(())
        } else {
            let mut overflow = Vec::new();
            overflow
                .try_reserve_exact(self.len + additional)
                .map_err(|_| "list element allocation failed")?;
            if overflow.capacity() != self.len + additional {
                return Err("allocator changed list element capacity");
            }
            overflow.extend_from_slice(&self.inline[..self.len]);
            self.overflow = Some(overflow);
            Ok(())
        }
    }

    pub(crate) fn try_push(&mut self, value: i64) -> Result<(), &'static str> {
        self.try_reserve(1)?;
        if let Some(overflow) = self.overflow.as_mut() {
            overflow.push(value);
        } else {
            self.inline[self.len] = value;
        }
        self.len += 1;
        Ok(())
    }

    pub(crate) fn try_extend(&mut self, values: &[i64]) -> Result<(), &'static str> {
        self.try_reserve(values.len())?;
        if let Some(overflow) = self.overflow.as_mut() {
            overflow.extend_from_slice(values);
        } else {
            self.inline[self.len..self.len + values.len()].copy_from_slice(values);
        }
        self.len += values.len();
        Ok(())
    }

    pub(crate) fn as_slice(&self) -> &[i64] {
        match &self.overflow {
            Some(overflow) => overflow.as_slice(),
            None => &self.inline[..self.len],
        }
    }

    fn overflow_bytes(&self) -> u64 {
        if self.len > INLINE_LIST_ELEMENTS {
            self.len as u64 * std::mem::size_of::<i64>() as u64
        } else {
            0
        }
    }
}

impl Default for ListValues {
    fn default() -> Self {
        Self {
            inline: [0; INLINE_LIST_ELEMENTS],
            overflow: None,
            len: 0,
        }
    }
}

impl std::ops::Deref for ListValues {
    type Target = [i64];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ListPayload {
    pub mask: u64,
    pub values: ListValues,
}

impl ListPayload {
    /// Shared empty payload for read-only access; callers that mutate must
    /// materialize their own value instead.
    pub(crate) fn empty_ref() -> &'static Self {
        static EMPTY: ListPayload = ListPayload {
            mask: 0,
            values: ListValues {
                inline: [0; INLINE_LIST_ELEMENTS],
                overflow: None,
                len: 0,
            },
        };
        &EMPTY
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.values.len == 0
    }

    pub(crate) fn overflow_bytes(&self) -> u64 {
        self.values.overflow_bytes()
    }

    pub(crate) fn max_overflow_bytes() -> u64 {
        MAX_LIST_ELEMENTS as u64 * std::mem::size_of::<i64>() as u64
    }
}

impl ListPayload {
    pub(crate) fn valid(&self, arguments: &[i64], expected_mask: u64) -> bool {
        if arguments.len() > 64
            || self.mask != expected_mask
            || self.len() > MAX_LIST_ELEMENTS
            || (arguments.len() < 64 && self.mask >> arguments.len() != 0)
        {
            return false;
        }
        let mut cursor = 0;
        for (index, argument) in arguments.iter().enumerate() {
            if self.mask & (1u64 << index) != 0 {
                let (start, length) = Self::segment(*argument);
                if start != cursor || length > self.len() - cursor {
                    return false;
                }
                cursor += length;
            }
        }
        cursor == self.len()
    }

    pub(crate) fn segment(encoded: i64) -> (usize, usize) {
        (encoded as u32 as usize, ((encoded as u64) >> 32) as usize)
    }

    pub(crate) fn encode(start: usize, length: usize) -> i64 {
        ((length as u64) << 32 | start as u64) as i64
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
    fn byte_segments_require_canonical_schema_capacity_and_a_live_charge() {
        let vm = ResourceAccount::vm(limits(1 << 20));
        let mut payload = BytePayload::new(&vm, 5, MAX_MESSAGE_BYTES).unwrap();
        payload.data.resize(MAX_MESSAGE_BYTES, 17);
        let arguments = [
            ListPayload::encode(0, 32768),
            1,
            ListPayload::encode(32768, 32768),
        ];
        assert!(payload.valid(&arguments, 5));
        assert_eq!(vm.used().bytes, MAX_MESSAGE_BYTES as u64);
        for mask in [0, 1, 4, 7, 8, u64::MAX] {
            assert!(!payload.valid(&arguments, mask));
        }
        for invalid in [
            [
                ListPayload::encode(1, 32768),
                1,
                ListPayload::encode(32768, 32768),
            ],
            [
                ListPayload::encode(0, 32768),
                1,
                ListPayload::encode(32767, 32769),
            ],
            [
                ListPayload::encode(0, 32768),
                1,
                ListPayload::encode(32769, 32767),
            ],
            [
                ListPayload::encode(0, 32768),
                1,
                ListPayload::encode(32768, 32769),
            ],
            [ListPayload::encode(0, 32768), 1, -1],
        ] {
            assert!(!payload.valid(&invalid, 5));
        }
        payload.data.pop();
        assert!(!payload.valid(&arguments, 5));
        assert!(BytePayload::new(&vm, 1, MAX_MESSAGE_BYTES + 1).is_err());
        assert!(BytePayload::new(&vm, 0, 0).is_err());
        let empty = BytePayload::new(&vm, 1 << 63, 0).unwrap();
        assert!(empty.valid(&[0; 64], 1 << 63));
        assert!(!empty.valid(&[0; 63], 1 << 63));
        assert!(BytePayload::default().valid(&[], 0));
        drop((empty, payload));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn byte_payload_admission_rejects_each_level_before_allocation() {
        for restricted in 0..3 {
            let vm = ResourceAccount::vm(limits(if restricted == 0 { 7 } else { 16 }));
            let tenant = vm
                .tenant(limits(if restricted == 1 { 7 } else { 16 }))
                .unwrap();
            let process = tenant
                .process(limits(if restricted == 2 { 7 } else { 16 }))
                .unwrap();
            assert!(BytePayload::new(&process, 1, 8).is_err());
            assert_eq!(process.used(), Resources::default());
            assert_eq!(tenant.used(), Resources::default());
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn byte_payload_transfers_only_net_capacity_and_failure_preserves_source() {
        let vm = ResourceAccount::vm(limits(8));
        let source_tenant = vm.tenant(limits(8)).unwrap();
        let source = source_tenant.process(limits(8)).unwrap();
        let target_tenant = vm.tenant(limits(8)).unwrap();
        let target = target_tenant.process(limits(8)).unwrap();
        let restricted = target_tenant.process(limits(7)).unwrap();
        let other_vm = ResourceAccount::vm(limits(8));
        let mut payload = BytePayload::new(&source, 1, 8).unwrap();
        payload.data.extend_from_slice(b"abcdefgh");
        for rejected in [&restricted, &other_vm] {
            assert!(payload.transfer_checked(rejected).is_err());
            assert_eq!(source.used().bytes, 8);
            assert_eq!(source_tenant.used().bytes, 8);
            assert_eq!(target_tenant.used().bytes, 0);
            assert_eq!(vm.used().bytes, 8);
        }
        payload.transfer_checked(&target).unwrap();
        assert_eq!(source.used().bytes, 0);
        assert_eq!(source_tenant.used().bytes, 0);
        assert_eq!(target.used().bytes, 8);
        assert_eq!(target_tenant.used().bytes, 8);
        assert_eq!(vm.used().bytes, 8);
        assert!(payload.valid(&[ListPayload::encode(0, 8)], 1));
        drop((source, source_tenant));
        assert_eq!(vm.used().bytes, 8);
        drop(payload);
        assert_eq!(vm.used(), Resources::default());
        assert_eq!(other_vm.used(), Resources::default());
    }

    #[test]
    fn list_segments_are_canonical_bounded_and_match_the_schema() {
        let mut payload = ListPayload {
            mask: 5,
            ..ListPayload::default()
        };
        payload.values.len = 64;
        let arguments = [ListPayload::encode(0, 40), 9, ListPayload::encode(40, 24)];
        assert!(payload.valid(&arguments, 5));
        for mask in [0, 1, 4, 7, 8, u64::MAX] {
            assert!(!payload.valid(&arguments, mask));
        }
        for malformed in [
            [ListPayload::encode(1, 40), 9, ListPayload::encode(40, 24)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(39, 25)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(41, 23)],
            [ListPayload::encode(0, 40), 9, ListPayload::encode(40, 25)],
            [ListPayload::encode(0, 40), 9, -1],
        ] {
            assert!(!payload.valid(&malformed, 5));
        }
        payload.values.len = 65;
        assert!(!payload.valid(&arguments, 5));
        payload.mask = 1 << 63;
        payload.values.len = 0;
        assert!(payload.valid(&[0; 64], 1 << 63));
        assert!(!payload.valid(&[0; 63], 1 << 63));
        payload.mask = 0;
        assert!(payload.valid(&[], 0));
        payload.values.len = 1;
        assert!(!payload.valid(&[], 0));
    }
}
