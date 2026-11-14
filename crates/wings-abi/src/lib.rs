use std::ffi::c_void;
use std::sync::atomic::AtomicU32;

use serde::{Deserialize, Serialize};

pub const SCALAR_ABI_VERSION: u32 = 15;
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
pub const EFFECT_SEND_SELF: u32 = 0;
pub const EFFECT_SELF_PID: u32 = 1;
pub const EFFECT_SPAWN_SELF: u32 = 2;
pub const EFFECT_SEND: u32 = 3;
pub const EFFECT_REQUEST: u32 = 4;
pub const EFFECT_CANCEL_REQUEST: u32 = 5;
pub const HEAP_RECORD_INT: u32 = 0;
pub const HEAP_READ_INT: u32 = 1;
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
pub const MAX_SHADOW_ROOTS: usize = 64;
pub const DEFAULT_MAX_STEPS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Int,
    Bool,
    Unit,
    Ref,
    ListRef,
    BytesRef,
    Pid,
    RequestId,
}

impl ValueType {
    pub fn accepts(self, value: i64) -> bool {
        match self {
            Self::Int => true,
            Self::Bool => matches!(value, 0 | 1),
            Self::Unit => value == 0,
            Self::Ref | Self::ListRef | Self::BytesRef | Self::Pid | Self::RequestId => false,
        }
    }

    pub fn is_reference(self) -> bool {
        matches!(self, Self::Ref | Self::ListRef | Self::BytesRef)
    }

    pub fn is_capability(self) -> bool {
        matches!(self, Self::Pid | Self::RequestId)
    }
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
    pub record_mask: u64,
    pub list_mask: u64,
    pub bytes_mask: u64,
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
