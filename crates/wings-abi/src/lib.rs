use serde::{Deserialize, Serialize};

pub const SCALAR_ABI_VERSION: u32 = 1;
pub const STATUS_OK: u32 = 0;
pub const STATUS_OVERFLOW: u32 = 1;
pub const STATUS_DIVISION_BY_ZERO: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Int,
    Bool,
    Unit,
}

impl ValueType {
    pub fn accepts(self, value: i64) -> bool {
        match self {
            Self::Int => true,
            Self::Bool => matches!(value, 0 | 1),
            Self::Unit => value == 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScalarOutput {
    pub state: i64,
    pub reply: i64,
}

pub type ScalarEntry = unsafe extern "C" fn(i64, *const i64, *mut ScalarOutput) -> u32;

pub fn host_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err("scalar AOT requires macOS AArch64 or Linux x86-64".into()),
    }
}
