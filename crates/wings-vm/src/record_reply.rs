use std::fmt;
use std::sync::Arc;

use wings_abi::{
    RecordExpansion, RecordSchema, STATUS_HEAP_EXHAUSTED, STATUS_REPLY_REJECTED, ScalarContext,
    ValueType,
};

use crate::host_values::HostMessage;
use crate::message_values::{BytePayload, ListPayload};
use crate::{MessageValue, Reservation, ResourceAccount, Resources};

#[derive(Clone)]
pub struct RecordReply {
    inner: Arc<Contents>,
}

struct Contents {
    root: u16,
    records: Box<[RecordSchema]>,
    expansion: RecordExpansion,
    payload: HostMessage,
    _charge: Reservation,
}

pub struct RecordReplyValues<'reply> {
    reply: &'reply RecordReply,
    index: usize,
}

pub(crate) struct RecordReplyLease {
    root: u16,
    records: Box<[RecordSchema]>,
    expansion: RecordExpansion,
    charge: Reservation,
}

pub(crate) struct RecordCompletion {
    pub(crate) arguments: [i64; 64],
    pub(crate) count: usize,
    pub(crate) record_mask: u64,
    pub(crate) lists: ListPayload,
    pub(crate) bytes: BytePayload,
}

impl RecordReply {
    pub(crate) fn reserve(
        account: &ResourceAccount,
        root: u16,
        records: &[RecordSchema],
        context: Option<&mut ScalarContext>,
    ) -> Result<RecordReplyLease, u32> {
        let expansion =
            wings_abi::expand_record_schema(records, root).map_err(|_| STATUS_REPLY_REJECTED)?;
        let schema_bytes = records.iter().try_fold(
            records
                .len()
                .checked_mul(std::mem::size_of::<RecordSchema>())
                .ok_or(STATUS_REPLY_REJECTED)?,
            |total, record| {
                let field_bytes = record
                    .fields
                    .len()
                    .checked_mul(std::mem::size_of::<(String, ValueType)>())
                    .ok_or(STATUS_REPLY_REJECTED)?;
                let names = record
                    .fields
                    .iter()
                    .try_fold(record.name.len(), |size, (name, _)| {
                        size.checked_add(name.len()).ok_or(STATUS_REPLY_REJECTED)
                    })?;
                total
                    .checked_add(field_bytes)
                    .and_then(|size| size.checked_add(names))
                    .ok_or(STATUS_REPLY_REJECTED)
            },
        )?;
        let bytes = std::mem::size_of::<Contents>()
            .checked_add(128)
            .and_then(|size| size.checked_add(schema_bytes))
            .ok_or(STATUS_REPLY_REJECTED)?;
        let charge = account
            .reserve_status(Resources {
                bytes: bytes as u64,
                ..Resources::default()
            })
            .map_err(|_| STATUS_HEAP_EXHAUSTED)?;
        let records = copy_schemas(records, context)?;
        Ok(RecordReplyLease {
            root,
            records,
            expansion,
            charge,
        })
    }

    pub(crate) fn from_parts(
        lease: RecordReplyLease,
        payload: HostMessage,
    ) -> Result<Self, String> {
        validate_payload(&lease.expansion, &payload)?;
        Ok(Self {
            inner: Arc::new(Contents {
                root: lease.root,
                records: lease.records,
                expansion: lease.expansion,
                payload,
                _charge: lease.charge,
            }),
        })
    }

    pub fn schema(&self) -> &RecordSchema {
        &self.inner.records[usize::from(self.inner.root)]
    }

    pub fn schemas(&self) -> &[RecordSchema] {
        &self.inner.records
    }

    pub fn leaf_types(&self) -> &[ValueType] {
        &self.inner.expansion.fields[..self.inner.expansion.count]
    }

    pub fn values(&self) -> RecordReplyValues<'_> {
        RecordReplyValues {
            reply: self,
            index: 0,
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.inner.payload.bytes.as_slice()
    }

    pub(crate) fn bytes_mask(&self) -> u64 {
        self.inner.payload.bytes.mask
    }

    pub(crate) fn string_mask(&self) -> u64 {
        self.inner.payload.bytes.string_mask
    }

    pub(crate) fn completion(
        reply: Option<&RecordReply>,
        schema: u16,
        records: &[RecordSchema],
        capability: i64,
        status: i64,
    ) -> Result<RecordCompletion, String> {
        let expansion = wings_abi::expand_record_schema(records, schema)?;
        if expansion.count + 2 > 64 {
            return Err("record completion exceeds 64 message parameters".into());
        }
        let mut result = RecordCompletion {
            arguments: [0; 64],
            count: expansion.count + 2,
            record_mask: 0,
            lists: ListPayload::default(),
            bytes: BytePayload::default(),
        };
        result.arguments[0] = capability;
        result.arguments[1] = status;
        let offset = 2;
        if let Some(reply) = reply {
            if !wings_abi::equivalent_types(
                &reply.inner.records,
                ValueType::RecordRef(reply.inner.root),
                records,
                ValueType::RecordRef(schema),
            ) || reply.leaf_types() != &expansion.fields[..expansion.count]
            {
                return Err("record reply schema does not match completion".into());
            }
            let payload = &reply.inner.payload;
            result.arguments[offset..result.count]
                .copy_from_slice(&payload.arguments[..payload.count]);
            result.record_mask = payload.record_mask << offset;
            result.lists = payload.lists;
            result.lists.mask <<= offset;
            result.bytes = BytePayload::record_completion(
                reply.clone(),
                payload.bytes.mask << offset,
                payload.bytes.string_mask << offset,
            );
        } else {
            for (index, ty) in expansion.fields[..expansion.count].iter().enumerate() {
                let bit = 1u64 << (index + offset);
                match ty {
                    ValueType::Ref => result.record_mask |= bit,
                    ValueType::ListRef => {
                        result.lists.mask |= bit;
                        result.arguments[index + offset] = ListPayload::encode(0, 0);
                    }
                    ValueType::BytesRef => {
                        result.bytes.mask |= bit;
                        result.arguments[index + offset] = ListPayload::encode(0, 0);
                    }
                    ValueType::StringRef => {
                        result.bytes.mask |= bit;
                        result.bytes.string_mask |= bit;
                        result.arguments[index + offset] = ListPayload::encode(0, 0);
                    }
                    ValueType::Int | ValueType::Bool | ValueType::Unit => {}
                    ValueType::RecordRef(_) | ValueType::Pid | ValueType::RequestId => {
                        return Err("invalid record completion leaf type".into());
                    }
                }
            }
            if result.bytes.mask != 0 {
                result.bytes =
                    BytePayload::empty_completion(result.bytes.mask, result.bytes.string_mask);
            }
        }
        Ok(result)
    }
}

impl<'reply> Iterator for RecordReplyValues<'reply> {
    type Item = MessageValue<'reply>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        let ty = *self.reply.leaf_types().get(index)?;
        let encoded = self.reply.inner.payload.arguments[index];
        self.index += 1;
        let value = match ty {
            ValueType::Int => MessageValue::Int(encoded),
            ValueType::Bool => MessageValue::Bool(encoded == 1),
            ValueType::Unit => MessageValue::Unit,
            ValueType::Ref => MessageValue::RecordInt(encoded),
            ValueType::ListRef => {
                let (start, length) = ListPayload::segment(encoded);
                let values = &self.reply.inner.payload.lists.values[start..start + length];
                return Some(MessageValue::IntList(values));
            }
            ValueType::BytesRef => {
                let (start, length) = ListPayload::segment(encoded);
                let values = &self.reply.bytes()[start..start + length];
                return Some(MessageValue::Bytes(values));
            }
            ValueType::StringRef => {
                let (start, length) = ListPayload::segment(encoded);
                let values = &self.reply.bytes()[start..start + length];
                let text = std::str::from_utf8(values).expect("validated record reply string");
                return Some(MessageValue::String(text));
            }
            ValueType::RecordRef(_) | ValueType::Pid | ValueType::RequestId => {
                return None;
            }
        };
        Some(value)
    }
}

impl PartialEq for RecordReply {
    fn eq(&self, other: &Self) -> bool {
        wings_abi::equivalent_types(
            &self.inner.records,
            ValueType::RecordRef(self.inner.root),
            &other.inner.records,
            ValueType::RecordRef(other.inner.root),
        ) && self.leaf_types() == other.leaf_types()
            && self.values().eq(other.values())
    }
}

impl Eq for RecordReply {}

impl fmt::Debug for RecordReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordReply")
            .field("schema", &self.schema().name)
            .field("leaf_types", &self.leaf_types())
            .field("values", &self.values().collect::<Vec<_>>())
            .finish()
    }
}

fn validate_payload(expansion: &RecordExpansion, payload: &HostMessage) -> Result<(), String> {
    if payload.count != expansion.count {
        return Err("record reply leaf count does not match schema".into());
    }
    let mut record_mask = 0;
    let mut list_mask = 0;
    let mut bytes_mask = 0;
    let mut string_mask = 0;
    for (index, ty) in expansion.fields[..expansion.count].iter().enumerate() {
        let bit = 1u64 << index;
        match ty {
            ValueType::Ref => record_mask |= bit,
            ValueType::ListRef => list_mask |= bit,
            ValueType::BytesRef => bytes_mask |= bit,
            ValueType::StringRef => {
                bytes_mask |= bit;
                string_mask |= bit;
            }
            ValueType::Int | ValueType::Bool | ValueType::Unit => {
                if !ty.accepts(payload.arguments[index]) {
                    return Err("record reply scalar leaf is invalid".into());
                }
            }
            ValueType::RecordRef(_) | ValueType::Pid | ValueType::RequestId => {
                return Err("invalid record reply leaf type".into());
            }
        }
    }
    if payload.record_mask != record_mask
        || !payload
            .lists
            .valid(&payload.arguments[..payload.count], list_mask)
        || !payload
            .bytes
            .valid(&payload.arguments[..payload.count], bytes_mask)
        || payload.bytes.string_mask != string_mask
    {
        return Err("record reply payload does not match schema".into());
    }
    Ok(())
}

fn copy_schemas(
    records: &[RecordSchema],
    mut context: Option<&mut ScalarContext>,
) -> Result<Box<[RecordSchema]>, u32> {
    fn copy_string(value: &str) -> Result<String, u32> {
        let mut copy = String::new();
        copy.try_reserve_exact(value.len())
            .map_err(|_| STATUS_HEAP_EXHAUSTED)?;
        if copy.capacity() != value.len() {
            return Err(STATUS_HEAP_EXHAUSTED);
        }
        copy.push_str(value);
        Ok(copy)
    }

    let mut owned = Vec::new();
    owned
        .try_reserve_exact(records.len())
        .map_err(|_| STATUS_HEAP_EXHAUSTED)?;
    if owned.capacity() != records.len() {
        return Err(STATUS_HEAP_EXHAUSTED);
    }
    for record in records {
        if let Some(context) = context.as_deref_mut() {
            crate::execution::runtime_checkpoint(context, true)?;
        }
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(record.fields.len())
            .map_err(|_| STATUS_HEAP_EXHAUSTED)?;
        if fields.capacity() != record.fields.len() {
            return Err(STATUS_HEAP_EXHAUSTED);
        }
        for (name, ty) in &record.fields {
            if let Some(context) = context.as_deref_mut() {
                crate::execution::runtime_checkpoint(context, true)?;
            }
            fields.push((copy_string(name)?, *ty));
        }
        owned.push(RecordSchema {
            name: copy_string(&record.name)?,
            fields,
        });
    }
    Ok(owned.into_boxed_slice())
}
