use std::ffi::c_void;
use std::sync::atomic::AtomicU32;

use serde::{Deserialize, Serialize};

pub const SCALAR_ABI_VERSION: u32 = 23;
pub const STATUS_OK: u32 = 0;
pub const STATUS_OVERFLOW: u32 = 1;
pub const STATUS_DIVISION_BY_ZERO: u32 = 2;
pub const STATUS_CANCELLED: u32 = 3;
pub const STATUS_BUDGET_EXHAUSTED: u32 = 4;
pub const STATUS_INVALID_CONTROL: u32 = 5;
pub const STATUS_STACK_EXHAUSTED: u32 = 6;
pub const STATUS_HEAP_EXHAUSTED: u32 = 7;
pub const STATUS_INVALID_ROOT: u32 = 8;
pub const STATUS_EMPTY_LIST: u32 = 9;
pub const STATUS_SEND_REJECTED: u32 = 10;
pub const STATUS_SPAWN_REJECTED: u32 = 11;
pub const STATUS_REQUEST_REJECTED: u32 = 12;
pub const STATUS_INVALID_BYTES: u32 = 13;
pub const STATUS_REPLY_REJECTED: u32 = 14;
pub const STATUS_INVALID_STRING: u32 = 15;
pub const STATUS_INVALID_STRING_INDEX: u32 = 16;
pub const EFFECT_SEND_SELF: u32 = 0;
pub const EFFECT_SELF_PID: u32 = 1;
pub const EFFECT_SPAWN_SELF: u32 = 2;
pub const EFFECT_SEND: u32 = 3;
pub const EFFECT_REQUEST: u32 = 4;
pub const EFFECT_CANCEL_REQUEST: u32 = 5;
pub const EFFECT_SPAWN_ACTOR: u32 = 6;
pub const EFFECT_SEND_ACTOR: u32 = 7;
pub const EFFECT_REQUEST_ACTOR: u32 = 8;
pub const EFFECT_SEND_AFTER_SELF: u32 = 9;
pub const EFFECT_SEND_AFTER: u32 = 10;
pub const EFFECT_SEND_AFTER_ACTOR: u32 = 11;
pub const EFFECT_MONITOR: u32 = 12;
pub const HEAP_COLLECT: u32 = 2;
pub const HEAP_EMPTY_INTS: u32 = 3;
pub const HEAP_PREPEND_INT: u32 = 4;
pub const HEAP_HEAD_INT: u32 = 5;
pub const HEAP_TAIL_INTS: u32 = 6;
pub const HEAP_IS_EMPTY_INTS: u32 = 7;
pub const HEAP_FILL_BYTES: u32 = 8;
pub const HEAP_BYTES_LEN: u32 = 9;
pub const HEAP_BYTE_AT: u32 = 10;
pub const HEAP_REPLACE_BYTE: u32 = 11;
pub const HEAP_READ_I64_BE: u32 = 12;
pub const HEAP_REPLACE_I64_BE: u32 = 13;
pub const HEAP_STRING_FROM_BYTES: u32 = 14;
pub const HEAP_STRING_BYTES: u32 = 15;
pub const HEAP_STRING_LEN: u32 = 16;
pub const HEAP_CONCAT_STRINGS: u32 = 17;
pub const HEAP_STRING_LITERAL: u32 = 18;
pub const HEAP_RECORD_NEW: u32 = 19;
pub const HEAP_RECORD_FIELD: u32 = 20;
pub const HEAP_EQUAL_STRINGS: u32 = 21;
pub const HEAP_STRING_AT: u32 = 22;
pub const HEAP_STRING_SLICE: u32 = 23;
pub const HEAP_INT_TO_STRING: u32 = 24;
pub const HEAP_BOOL_TO_STRING: u32 = 25;
pub const HEAP_BYTES_LITERAL: u32 = 26;
pub const MAX_SHADOW_ROOTS: usize = 64;
pub const MAX_RECORD_REPLY_NODES: usize = 64;
pub const DEFAULT_MAX_STEPS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Int,
    Bool,
    Unit,
    ListRef,
    BytesRef,
    StringRef,
    RecordRef(u16),
    Pid,
    RequestId,
}

impl ValueType {
    pub fn accepts(self, value: i64) -> bool {
        match self {
            Self::Int => true,
            Self::Bool => matches!(value, 0 | 1),
            Self::Unit => value == 0,
            Self::ListRef
            | Self::BytesRef
            | Self::StringRef
            | Self::RecordRef(_)
            | Self::Pid
            | Self::RequestId => false,
        }
    }

    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Self::ListRef | Self::BytesRef | Self::StringRef | Self::RecordRef(_)
        )
    }

    pub fn is_supported_reply(self) -> bool {
        !matches!(self, Self::RequestId)
    }

    pub fn is_capability(self) -> bool {
        matches!(self, Self::Pid | Self::RequestId)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSchema {
    pub name: String,
    pub fields: Vec<(String, ValueType)>,
}

#[derive(Clone, Copy, Debug)]
pub struct RecordExpansion {
    pub fields: [ValueType; MAX_RECORD_REPLY_NODES],
    pub count: usize,
    pub nodes: usize,
}

pub fn expand_record_schema(
    records: &[RecordSchema],
    root: u16,
) -> Result<RecordExpansion, String> {
    fn visit(
        records: &[RecordSchema],
        schema: u16,
        expansion: &mut RecordExpansion,
    ) -> Result<(), String> {
        expansion.nodes += 1;
        if expansion.nodes > MAX_RECORD_REPLY_NODES {
            return Err("record reply expansion exceeds 64 total nodes".into());
        }
        let record = records
            .get(usize::from(schema))
            .ok_or("record reply references an unknown schema")?;
        for (_, ty) in &record.fields {
            if let ValueType::RecordRef(nested) = ty {
                visit(records, *nested, expansion)?;
            } else {
                expansion.nodes += 1;
                if expansion.nodes > MAX_RECORD_REPLY_NODES
                    || expansion.count == MAX_RECORD_REPLY_NODES
                {
                    return Err("record reply expansion exceeds 64 total nodes".into());
                }
                if *ty == ValueType::RequestId {
                    return Err("record reply cannot contain RequestId capabilities".into());
                }
                expansion.fields[expansion.count] = *ty;
                expansion.count += 1;
            }
        }
        Ok(())
    }

    let mut expansion = RecordExpansion {
        fields: [ValueType::Unit; MAX_RECORD_REPLY_NODES],
        count: 0,
        nodes: 0,
    };
    visit(records, root, &mut expansion)?;
    Ok(expansion)
}

pub fn equivalent_types(
    left_records: &[RecordSchema],
    left: ValueType,
    right_records: &[RecordSchema],
    right: ValueType,
) -> bool {
    fn equal(
        left_records: &[RecordSchema],
        left: ValueType,
        right_records: &[RecordSchema],
        right: ValueType,
        remaining: &mut usize,
    ) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match (left, right) {
            (ValueType::RecordRef(left), ValueType::RecordRef(right)) => {
                let (Some(left), Some(right)) = (
                    left_records.get(usize::from(left)),
                    right_records.get(usize::from(right)),
                ) else {
                    return false;
                };
                left.name == right.name
                    && left.fields.len() == right.fields.len()
                    && left.fields.iter().zip(&right.fields).all(
                        |((left_name, left_type), (right_name, right_type))| {
                            left_name == right_name
                                && equal(
                                    left_records,
                                    *left_type,
                                    right_records,
                                    *right_type,
                                    remaining,
                                )
                        },
                    )
            }
            (left, right) => left == right,
        }
    }

    let mut remaining = MAX_RECORD_REPLY_NODES;
    equal(left_records, left, right_records, right, &mut remaining)
}

pub fn validate_records(records: &[RecordSchema]) -> Result<(), String> {
    if records.len() > 32 {
        return Err("record schema count exceeds 32".into());
    }
    let mut names = std::collections::HashSet::new();
    for (index, record) in records.iter().enumerate() {
        let valid_name = |name: &str| {
            !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
        };
        if !valid_name(&record.name) || !names.insert(&record.name) || record.fields.len() > 8 {
            return Err("invalid or duplicate record schema name or field count".into());
        }
        let mut fields = std::collections::HashSet::new();
        for (name, ty) in &record.fields {
            if !valid_name(name)
                || !fields.insert(name)
                || *ty == ValueType::RequestId
                || matches!(ty, ValueType::RecordRef(target) if usize::from(*target) >= index)
            {
                return Err("invalid record field name, type or forward reference".into());
            }
        }
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScalarOutput {
    pub state: i64,
    pub reply: i64,
}

#[repr(C)]
pub struct ScalarContext {
    pub cancellation: *const AtomicU32,
    pub remaining_steps: u64,
    pub yield_request: *const AtomicU32,
    pub slice_remaining: u64,
    pub slow_path: Option<ScalarSlowPath>,
    pub data: *mut c_void,
    pub stack_limit: usize,
    pub roots: *const ShadowFrame,
    pub heap_call: Option<HeapCall>,
    pub heap_data: *mut c_void,
    pub effect_call: Option<EffectCall>,
}

#[repr(C)]
pub struct ShadowFrame {
    pub previous: *const ShadowFrame,
    pub slots: *const u64,
    pub count: usize,
}

pub type HeapCall = unsafe extern "C" fn(*mut ScalarContext, u32, u64, *mut u64) -> u32;

#[repr(C)]
pub struct EffectRequest {
    pub operation: u32,
    pub handler: u32,
    pub target: i64,
    pub arguments: *const i64,
    pub count: usize,
    pub callback: u32,
    pub timeout_ms: i64,
    pub list_mask: u64,
    pub bytes_mask: u64,
    pub string_mask: u64,
}

pub type EffectCall =
    unsafe extern "C" fn(*mut ScalarContext, *const EffectRequest, *mut i64) -> u32;

pub type ScalarSlowPath = unsafe extern "C" fn(*mut ScalarContext) -> u32;

pub type ScalarEntry =
    unsafe extern "C" fn(i64, *const i64, *mut ScalarOutput, *mut ScalarContext) -> u32;

pub fn host_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err("scalar AOT requires macOS AArch64 or Linux x86-64".into()),
    }
}

pub fn encode_actor_handler(actor: usize, handler: usize) -> Result<u32, String> {
    let actor = u16::try_from(actor).map_err(|_| String::from("actor index exceeds u16"))?;
    let handler = u16::try_from(handler).map_err(|_| String::from("handler index exceeds u16"))?;
    Ok((u32::from(actor) << 16) | u32::from(handler))
}

pub const fn decode_actor_handler(encoded: usize) -> (usize, usize) {
    ((encoded >> 16) & 0xFFFF, encoded & 0xFFFF)
}

pub const MAX_CAPABILITY_SLOTS: u32 = 1 << 14;

pub const fn encode_capability(instance: u64, slot: u32, generation: u32) -> Option<i64> {
    if slot >= MAX_CAPABILITY_SLOTS || generation == 0 {
        return None;
    }
    let tag = (instance & 0xFFFF) << 47;
    let raw = tag | ((generation as u64) << 14) | (slot as u64);
    Some(raw as i64)
}

pub const fn encode_request_capability(instance: u64, slot: u32, generation: u64) -> Option<i64> {
    if slot >= MAX_CAPABILITY_SLOTS || generation == 0 {
        return None;
    }
    let folded = (generation ^ (generation >> 32)) as u32;
    let gen32 = if folded == 0 { 1 } else { folded };
    let tag = ((instance & 0xFFFF) << 47) | (1u64 << 46);
    let raw = tag | ((gen32 as u64) << 14) | (slot as u64);
    Some(raw as i64)
}

pub const fn decode_capability(instance: u64, encoded: i64) -> Option<(usize, u32)> {
    if encoded <= 0 {
        return None;
    }
    let raw = encoded as u64;
    if ((raw >> 46) & 1) != 0 || ((raw >> 47) & 0xFFFF) != (instance & 0xFFFF) {
        return None;
    }
    let generation = ((raw >> 14) & 0xFFFF_FFFF) as u32;
    if generation == 0 {
        return None;
    }
    Some(((raw & 0x3FFF) as usize, generation))
}

pub const fn decode_request_capability(instance: u64, encoded: i64) -> Option<(usize, u32)> {
    if encoded <= 0 {
        return None;
    }
    let raw = encoded as u64;
    if ((raw >> 46) & 1) != 1 || ((raw >> 47) & 0xFFFF) != (instance & 0xFFFF) {
        return None;
    }
    let generation = ((raw >> 14) & 0xFFFF_FFFF) as u32;
    if generation == 0 {
        return None;
    }
    Some(((raw & 0x3FFF) as usize, generation))
}
