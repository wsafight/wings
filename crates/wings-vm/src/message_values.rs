use crate::{Reservation, ResourceAccount, Resources};

pub(crate) const MAX_LIST_ELEMENTS: usize = 64;
pub(crate) const MAX_MESSAGE_BYTES: usize = 65536;

#[derive(Default)]
pub(crate) struct BytePayload {
    pub mask: u64,
    pub data: Vec<u8>,
    charge: Option<Reservation>,
    reply: Option<crate::BytesReply>,
    completion: bool,
}

impl BytePayload {
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
            data,
            charge: Some(charge),
            reply: None,
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

    pub(crate) fn as_slice(&self) -> &[u8] {
        self.reply
            .as_ref()
            .map_or(&self.data, crate::BytesReply::as_slice)
    }

    pub(crate) fn take_charge(&mut self) -> Option<Reservation> {
        self.charge.take()
    }

    pub(crate) fn valid(&self, arguments: &[i64], expected_mask: u64) -> bool {
        if arguments.len() > 64
            || self.mask != expected_mask
            || self.as_slice().len() > MAX_MESSAGE_BYTES
            || self.data.capacity() != self.data.len()
            || (arguments.len() < 64 && self.mask >> arguments.len() != 0)
            || (self.completion
                && (self.mask != 4 || !self.data.is_empty() || self.charge.is_some()))
            || (!self.completion && self.reply.is_some())
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

    pub(crate) fn transfer(&mut self, target: &ResourceAccount) -> Result<(), String> {
        if self.completion {
            return Err("shared byte completion cannot be transferred as a message".into());
        }
        if let Some(charge) = &mut self.charge {
            charge.transfer(target)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ListPayload {
    pub mask: u64,
    pub count: usize,
    pub values: [i64; MAX_LIST_ELEMENTS],
}

impl Default for ListPayload {
    fn default() -> Self {
        Self {
            mask: 0,
            count: 0,
            values: [0; MAX_LIST_ELEMENTS],
        }
    }
}

impl ListPayload {
    pub(crate) fn valid(&self, arguments: &[i64], expected_mask: u64) -> bool {
        if arguments.len() > 64
            || self.mask != expected_mask
            || self.count > MAX_LIST_ELEMENTS
            || (arguments.len() < 64 && self.mask >> arguments.len() != 0)
        {
            return false;
        }
        let mut cursor = 0;
        for (index, argument) in arguments.iter().enumerate() {
            if self.mask & (1u64 << index) != 0 {
                let (start, length) = Self::segment(*argument);
                if start != cursor || length > self.count - cursor {
                    return false;
                }
                cursor += length;
            }
        }
        cursor == self.count
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
            assert!(payload.transfer(rejected).is_err());
            assert_eq!(source.used().bytes, 8);
            assert_eq!(source_tenant.used().bytes, 8);
            assert_eq!(target_tenant.used().bytes, 0);
            assert_eq!(vm.used().bytes, 8);
        }
        payload.transfer(&target).unwrap();
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
            count: 64,
            ..ListPayload::default()
        };
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
        payload.count = 65;
        assert!(!payload.valid(&arguments, 5));
        payload.mask = 1 << 63;
        payload.count = 0;
        assert!(payload.valid(&[0; 64], 1 << 63));
        assert!(!payload.valid(&[0; 63], 1 << 63));
        payload.mask = 0;
        assert!(payload.valid(&[], 0));
        payload.count = 1;
        assert!(!payload.valid(&[], 0));
    }
}
