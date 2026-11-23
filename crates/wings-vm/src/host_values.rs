use wings_abi::ValueType;

use crate::message_values::{BytePayload, ListPayload, MAX_LIST_ELEMENTS, MAX_MESSAGE_BYTES};
use crate::{HandlerId, ResourceAccount};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageValue<'value> {
    Int(i64),
    Bool(bool),
    Unit,
    Pid(i64),
    IntList(&'value [i64]),
    Bytes(&'value [u8]),
    String(&'value str),
}

pub(crate) struct HostMessage {
    pub(crate) arguments: [i64; 64],
    pub(crate) count: usize,
    pub(crate) lists: ListPayload,
    pub(crate) bytes: BytePayload,
}

impl HostMessage {
    pub(crate) fn validate(
        handler: &HandlerId,
        values: &[MessageValue<'_>],
    ) -> Result<(u64, usize), String> {
        Self::validate_types(
            handler.schema().parameters.iter().map(|(_, ty)| *ty),
            values,
        )
    }

    fn validate_types(
        types: impl ExactSizeIterator<Item = ValueType>,
        values: &[MessageValue<'_>],
    ) -> Result<(u64, usize), String> {
        if values.len() > 64 || values.len() != types.len() {
            return Err("host message requires matching parameters and at most 64 values".into());
        }
        let mut list_count = 0;
        let mut byte_count = 0;
        let mut byte_mask = 0;
        for (index, (value, expected)) in values.iter().zip(types).enumerate() {
            let actual = match value {
                MessageValue::Int(_) => ValueType::Int,
                MessageValue::Bool(_) => ValueType::Bool,
                MessageValue::Unit => ValueType::Unit,
                MessageValue::Pid(pid) => {
                    if *pid < 0 {
                        return Err("host message Pid value cannot be negative".into());
                    }
                    ValueType::Pid
                }
                MessageValue::IntList(values) => {
                    if values.len() > MAX_LIST_ELEMENTS - list_count {
                        return Err("host message lists exceed 64 total integers".into());
                    }
                    list_count += values.len();
                    ValueType::ListRef
                }
                MessageValue::Bytes(bytes) => {
                    if bytes.len() > MAX_MESSAGE_BYTES - byte_count {
                        return Err("host message bytes exceed 65536 total bytes".into());
                    }
                    byte_count += bytes.len();
                    byte_mask |= 1u64 << index;
                    ValueType::BytesRef
                }
                MessageValue::String(text) => {
                    if text.len() > MAX_MESSAGE_BYTES - byte_count {
                        return Err(
                            "host message strings and bytes exceed 65536 total bytes".into()
                        );
                    }
                    byte_count += text.len();
                    byte_mask |= 1u64 << index;
                    ValueType::StringRef
                }
            };
            if actual != expected {
                return Err("host message value type does not match handler schema".into());
            }
        }
        Ok((byte_mask, byte_count))
    }

    pub(crate) fn copy(
        handler: &HandlerId,
        values: &[MessageValue<'_>],
        account: &ResourceAccount,
    ) -> Result<Self, String> {
        Self::copy_types(
            handler.schema().parameters.iter().map(|(_, ty)| *ty),
            values,
            account,
        )
    }

    pub(crate) fn copy_types(
        types: impl ExactSizeIterator<Item = ValueType>,
        values: &[MessageValue<'_>],
        account: &ResourceAccount,
    ) -> Result<Self, String> {
        let (byte_mask, byte_count) = Self::validate_types(types, values)?;
        let bytes = if byte_mask == 0 {
            BytePayload::default()
        } else {
            BytePayload::new(account, byte_mask, byte_count)?
        };
        let mut message = Self {
            arguments: [0; 64],
            count: values.len(),
            lists: ListPayload::default(),
            bytes,
        };
        for (index, value) in values.iter().enumerate() {
            message.arguments[index] = match value {
                MessageValue::Int(value) => *value,
                MessageValue::Bool(value) => i64::from(*value),
                MessageValue::Unit => 0,
                MessageValue::Pid(value) => *value,
                MessageValue::IntList(values) => {
                    let start = message.lists.len();
                    message.lists.values.try_extend(values)?;
                    message.lists.mask |= 1u64 << index;
                    ListPayload::encode(start, values.len())
                }
                MessageValue::Bytes(bytes) => {
                    let start = message.bytes.data.len();
                    message.bytes.data.extend_from_slice(bytes);
                    ListPayload::encode(start, bytes.len())
                }
                MessageValue::String(text) => {
                    let start = message.bytes.data.len();
                    message.bytes.data.extend_from_slice(text.as_bytes());
                    message.bytes.string_mask |= 1u64 << index;
                    ListPayload::encode(start, text.len())
                }
            };
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Resources, ScalarModule};
    use std::sync::Arc;

    fn handler(types: &[ValueType]) -> HandlerId {
        let mut module: ScalarModule = crate::tests::module();
        Arc::get_mut(&mut module.inner).unwrap().manifest.handlers[0].parameters = types
            .iter()
            .enumerate()
            .map(|(index, ty)| (format!("value_{index}"), *ty))
            .collect();
        module.handler("Add").unwrap()
    }

    fn limits() -> Resources {
        Resources {
            bytes: 1 << 20,
            ..Resources::default()
        }
    }

    #[test]
    fn string_messages_copy_utf8_and_share_the_byte_capacity_limit() {
        let vm = ResourceAccount::vm(limits());
        let target = handler(&[
            ValueType::StringRef,
            ValueType::BytesRef,
            ValueType::StringRef,
        ]);
        let mut text = String::from("中😀\0");
        let padding = vec![255; MAX_MESSAGE_BYTES - text.len()];
        let payload = HostMessage::copy(
            &target,
            &[
                MessageValue::String(&text),
                MessageValue::Bytes(&padding),
                MessageValue::String(""),
            ],
            &vm,
        )
        .unwrap();
        text.clear();
        assert_eq!(payload.bytes.string_mask, 5);
        assert!(payload.bytes.valid(&payload.arguments[..3], 7));
        assert_eq!(&payload.bytes.data[..8], "中😀\0".as_bytes());
        assert_eq!(vm.used().bytes, MAX_MESSAGE_BYTES as u64);
        drop(payload);
        assert_eq!(vm.used(), Resources::default());
        assert!(
            HostMessage::copy(
                &target,
                &[
                    MessageValue::String("中😀\0"),
                    MessageValue::Bytes(&padding),
                    MessageValue::String("a")
                ],
                &vm
            )
            .is_err()
        );
        let repeated = "😀".repeat(8193);
        assert!(
            HostMessage::copy(
                &target,
                &[
                    MessageValue::String(&repeated),
                    MessageValue::Bytes(&[]),
                    MessageValue::String(&repeated)
                ],
                &vm
            )
            .is_err()
        );
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn host_values_copy_mixed_contents_and_canonical_segments_independently() {
        let vm = ResourceAccount::vm(limits());
        let handler = handler(&[
            ValueType::Int,
            ValueType::Bool,
            ValueType::Unit,
            ValueType::Pid,
            ValueType::ListRef,
            ValueType::BytesRef,
            ValueType::BytesRef,
            ValueType::ListRef,
        ]);
        let mut integers = [i64::MIN, 17, i64::MAX];
        let mut bytes = [0, 255, 17, 99];
        let payload = HostMessage::copy(
            &handler,
            &[
                MessageValue::Int(i64::MIN),
                MessageValue::Bool(true),
                MessageValue::Unit,
                MessageValue::Pid(i64::MAX),
                MessageValue::IntList(&integers),
                MessageValue::Bytes(&bytes),
                MessageValue::Bytes(&[]),
                MessageValue::IntList(&[]),
            ],
            &vm,
        )
        .unwrap();
        integers.fill(0);
        bytes.fill(0);
        assert_eq!(&payload.arguments[..4], &[i64::MIN, 1, 0, i64::MAX]);
        assert_eq!(payload.lists.mask, (1 << 4) | (1 << 7));
        assert_eq!(&payload.lists.values[..3], &[i64::MIN, 17, i64::MAX]);
        assert!(
            payload
                .lists
                .valid(&payload.arguments[..payload.count], payload.lists.mask)
        );
        assert!(
            payload
                .bytes
                .valid(&payload.arguments[..payload.count], (1 << 5) | (1 << 6))
        );
        assert_eq!(payload.bytes.data, [0, 255, 17, 99]);
        assert_eq!(vm.used().bytes, 4);
        drop(payload);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn host_values_require_exact_types_and_validate_every_parameter_before_allocation() {
        let vm = ResourceAccount::vm(limits());
        for ty in [
            ValueType::Int,
            ValueType::Bool,
            ValueType::Unit,
            ValueType::ListRef,
            ValueType::BytesRef,
            ValueType::StringRef,
            ValueType::Pid,
            ValueType::RequestId,
        ] {
            let handler = handler(&[ValueType::BytesRef, ty]);
            for value in [
                MessageValue::Int(1),
                MessageValue::Bool(true),
                MessageValue::Unit,
                MessageValue::Pid(1),
                MessageValue::IntList(&[]),
                MessageValue::Bytes(&[]),
                MessageValue::String(""),
            ] {
                let actual = match value {
                    MessageValue::Int(_) => ValueType::Int,
                    MessageValue::Bool(_) => ValueType::Bool,
                    MessageValue::Unit => ValueType::Unit,
                    MessageValue::Pid(_) => ValueType::Pid,
                    MessageValue::IntList(_) => ValueType::ListRef,
                    MessageValue::Bytes(_) => ValueType::BytesRef,
                    MessageValue::String(_) => ValueType::StringRef,
                };
                let result =
                    HostMessage::copy(&handler, &[MessageValue::Bytes(&[1; 8]), value], &vm);
                assert_eq!(result.is_ok(), actual == ty);
                drop(result);
                assert_eq!(vm.used(), Resources::default());
            }
        }
        let handler = handler(&[ValueType::BytesRef, ValueType::Bool]);
        let blocked = vm.reserve(limits()).unwrap();
        let error = HostMessage::copy(
            &handler,
            &[MessageValue::Bytes(&[1; 8]), MessageValue::Int(1)],
            &vm,
        )
        .err()
        .unwrap();
        assert!(error.contains("value type"));
        assert_eq!(vm.used(), limits());
        assert!(HostMessage::copy(&handler, &[], &vm).is_err());
        drop(blocked);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn host_values_keep_original_combined_limits_and_highest_parameter_mask() {
        let vm = ResourceAccount::vm(limits());
        let bytes = vec![255; 65537];
        let integers = [i64::MAX; 65];
        let mixed = handler(&[
            ValueType::BytesRef,
            ValueType::BytesRef,
            ValueType::ListRef,
            ValueType::ListRef,
        ]);
        for split in [0, 1, 32768, 65536] {
            let payload = HostMessage::copy(
                &mixed,
                &[
                    MessageValue::Bytes(&bytes[..split]),
                    MessageValue::Bytes(&bytes[split..65536]),
                    MessageValue::IntList(&integers[..32]),
                    MessageValue::IntList(&integers[32..64]),
                ],
                &vm,
            )
            .unwrap();
            assert_eq!(payload.bytes.data.len(), 65536);
            assert_eq!(payload.lists.len(), 64);
            drop(payload);
        }
        for (byte_count, list_count) in [(65537, 64), (65536, 65)] {
            assert!(
                HostMessage::copy(
                    &mixed,
                    &[
                        MessageValue::Bytes(&bytes[..byte_count]),
                        MessageValue::Bytes(&[]),
                        MessageValue::IntList(&integers[..list_count]),
                        MessageValue::IntList(&[]),
                    ],
                    &vm
                )
                .is_err()
            );
            assert_eq!(vm.used(), Resources::default());
        }
        for ty in [
            ValueType::ListRef,
            ValueType::BytesRef,
            ValueType::StringRef,
        ] {
            let handler = handler(&[ty; 64]);
            let value = match ty {
                ValueType::ListRef => MessageValue::IntList(&[17]),
                ValueType::BytesRef => MessageValue::Bytes(&[255]),
                _ => MessageValue::String("a"),
            };
            let payload = HostMessage::copy(&handler, &[value; 64], &vm).unwrap();
            assert_eq!(payload.count, 64);
            assert_eq!(payload.lists.mask | payload.bytes.mask, u64::MAX);
            assert!(HostMessage::copy(&handler, &[value; 65], &vm).is_err());
            drop(payload);
        }
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn host_values_reserve_at_each_level_and_roll_back_failed_preparation() {
        for level in 0..3 {
            let vm = ResourceAccount::vm(Resources {
                bytes: 8 << 20,
                ..limits()
            });
            let tenant = vm
                .tenant_accounted(Resources {
                    bytes: 4 << 20,
                    ..limits()
                })
                .unwrap();
            let process = tenant.process_accounted(limits()).unwrap();
            let account = [&vm, &tenant, &process][level];
            let fill = account
                .reserve(Resources {
                    bytes: account.limit().bytes - account.used().bytes - 7,
                    ..Resources::default()
                })
                .unwrap();
            let baseline = [vm.used(), tenant.used(), process.used()];
            let handler = handler(&[ValueType::BytesRef]);
            assert!(
                HostMessage::copy(&handler, &[MessageValue::Bytes(&[1; 8])], &process).is_err()
            );
            assert_eq!([vm.used(), tenant.used(), process.used()], baseline);
            let empty = HostMessage::copy(&handler, &[MessageValue::Bytes(&[])], &process).unwrap();
            assert!(empty.bytes.valid(&empty.arguments[..1], 1));
            drop(empty);
            assert_eq!([vm.used(), tenant.used(), process.used()], baseline);
            drop(fill);
            let payload =
                HostMessage::copy(&handler, &[MessageValue::Bytes(&[17; 8])], &process).unwrap();
            assert_eq!(process.used().bytes, 8);
            drop((payload, process, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }
}
