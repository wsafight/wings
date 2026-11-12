use std::ffi::c_void;
use std::sync::atomic::AtomicU32;

use serde::{Deserialize, Serialize};

pub const SCALAR_ABI_VERSION: u32 = 5;
pub const STATUS_OK: u32 = 0;
pub const STATUS_OVERFLOW: u32 = 1;
pub const STATUS_DIVISION_BY_ZERO: u32 = 2;
pub const STATUS_CANCELLED: u32 = 3;
pub const STATUS_BUDGET_EXHAUSTED: u32 = 4;
pub const STATUS_INVALID_CONTROL: u32 = 5;
pub const STATUS_STACK_EXHAUSTED: u32 = 6;
pub const STATUS_HEAP_EXHAUSTED: u32 = 7;
pub const STATUS_INVALID_ROOT: u32 = 8;
pub const HEAP_RECORD_INT: u32 = 0;
pub const HEAP_READ_INT: u32 = 1;
pub const HEAP_COLLECT: u32 = 2;
pub const MAX_SHADOW_ROOTS: usize = 64;
pub const DEFAULT_MAX_STEPS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Int,
    Bool,
    Unit,
    Ref,
}

impl ValueType {
    pub fn accepts(self, value: i64) -> bool {
        match self {
            Self::Int => true,
            Self::Bool => matches!(value, 0 | 1),
            Self::Unit => value == 0,
            Self::Ref => false,
        }
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
}

#[repr(C)]
pub struct ShadowFrame {
    pub previous: *const ShadowFrame,
    pub slots: *const u64,
    pub count: usize,
}

pub type HeapCall = unsafe extern "C" fn(*mut ScalarContext, u32, u64, *mut u64) -> u32;

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
