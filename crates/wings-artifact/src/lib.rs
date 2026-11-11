use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wings_abi::{SCALAR_ABI_VERSION, ValueType, host_target};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
pub const INITIALIZER_SYMBOL: &str = "wings_scalar_init";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handler {
    pub name: String,
    pub parameters: Vec<(String, ValueType)>,
    pub reply: Option<ValueType>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub abi: u32,
    pub target: String,
    pub process: String,
    pub state: ValueType,
    pub handlers: Vec<Handler>,
}

pub fn handler_symbol(index: usize) -> String {
    format!("wings_scalar_handler_{index}")
}

pub fn library_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "program.dylib"
    } else {
        "program.so"
    }
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT_VERSION || self.abi != SCALAR_ABI_VERSION {
            return Err("unsupported artifact format or scalar ABI version".into());
        }
        if self.target != host_target()? {
            return Err("artifact target does not match this VM host".into());
        }
        if !valid_name(&self.process) || self.handlers.len() > 128 {
            return Err("invalid process name or handler count".into());
        }
        let mut names = HashSet::new();
        for handler in &self.handlers {
            if !valid_name(&handler.name)
                || !names.insert(&handler.name)
                || handler.parameters.len() > 64
            {
                return Err("invalid or duplicate handler name, or too many parameters".into());
            }
            let mut parameters = HashSet::new();
            for (name, _) in &handler.parameters {
                if !valid_name(name) || !parameters.insert(name) {
                    return Err("invalid or duplicate parameter name".into());
                }
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("artifact manifest exceeds 64 KiB".into());
        }
        Ok(bytes)
    }

    pub fn read(directory: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(directory.join("manifest.json"))
            .map_err(|error| error.to_string())?
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("artifact manifest exceeds 64 KiB".into());
        }
        let manifest: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}
