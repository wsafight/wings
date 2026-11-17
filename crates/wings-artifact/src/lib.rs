use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wings_abi::{SCALAR_ABI_VERSION, ValueType, host_target};

mod preflight;

pub const MAX_NAME_BYTES: usize = 256;
pub const FORMAT_VERSION: u32 = 2;
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
pub const INITIALIZER_SYMBOL: &str = "wings_scalar_init";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handler {
    pub name: String,
    pub parameters: Vec<(String, ValueType)>,
    pub reply: Option<ValueType>,
    #[serde(default)]
    pub completion: Option<ValueType>,
}

impl Handler {
    pub fn is_completion(&self) -> bool {
        self.completion_reply().is_some()
    }

    pub fn completion_reply(&self) -> Option<ValueType> {
        self.completion
            .or(match (self.reply, self.parameters.as_slice()) {
                (
                    None,
                    [
                        (_, ValueType::RequestId),
                        (_, ValueType::Int),
                        (
                            _,
                            ty @ (ValueType::Int
                            | ValueType::Bool
                            | ValueType::Unit
                            | ValueType::Ref
                            | ValueType::ListRef
                            | ValueType::BytesRef
                            | ValueType::StringRef),
                        ),
                    ],
                ) => Some(*ty),
                _ => None,
            })
    }
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
    #[serde(default)]
    pub records: Vec<wings_abi::RecordSchema>,
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
        wings_abi::validate_records(&self.records)?;
        if matches!(self.state, ValueType::RecordRef(index) if usize::from(index) >= self.records.len())
            || self.handlers.iter().any(|handler| {
                handler
                    .parameters
                    .iter()
                    .any(|(_, ty)| matches!(ty, ValueType::RecordRef(_)))
                    || matches!(handler.reply, Some(ValueType::RecordRef(index)) if usize::from(index) >= self.records.len())
                    || matches!(handler.completion, Some(ValueType::RecordRef(index)) if usize::from(index) >= self.records.len())
            })
        {
            return Err(
                "invalid named record state or external record signature".into(),
            );
        }
        if self.format != FORMAT_VERSION || self.abi != SCALAR_ABI_VERSION {
            return Err("unsupported artifact format or scalar ABI version".into());
        }
        if self.target != host_target()? {
            return Err("artifact target does not match this VM host".into());
        }
        if !valid_name(&self.process) || self.handlers.len() > 128 {
            return Err("invalid process name or handler count".into());
        }
        if self.state.is_capability() {
            return Err("Pid/RequestId state is not supported".into());
        }
        if self.handlers.iter().any(|handler| {
            handler.reply.is_some_and(ValueType::is_capability)
                || handler.parameters.iter().any(|(_, ty)| {
                    *ty == ValueType::Pid
                        || (*ty == ValueType::RequestId && !handler.is_completion())
                })
        }) {
            return Err("Pid cannot cross scalar artifact boundaries; RequestId is only allowed in completion events".into());
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
            if let Some(ValueType::RecordRef(schema)) = handler.completion {
                if handler.reply.is_some()
                    || handler.parameters.len() < 2
                    || handler.parameters[0].1 != ValueType::RequestId
                    || handler.parameters[1].1 != ValueType::Int
                {
                    return Err("invalid named record completion signature".into());
                }
                let expansion = wings_abi::expand_record_schema(&self.records, schema)?;
                if handler.parameters.len() != expansion.count + 2
                    || handler.parameters[2..]
                        .iter()
                        .zip(&expansion.fields[..expansion.count])
                        .any(|((_, actual), expected)| actual != expected)
                {
                    return Err("named record completion fields do not match its schema".into());
                }
            } else if handler.completion.is_some() {
                return Err("explicit completion metadata is reserved for named records".into());
            }
            if let Some(ValueType::RecordRef(schema)) = handler.reply {
                wings_abi::expand_record_schema(&self.records, schema)?;
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
        preflight::check(&bytes)?;
        let manifest: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control)
}
