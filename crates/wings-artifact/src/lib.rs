use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wings_abi::{SCALAR_ABI_VERSION, ValueType, host_target};

mod preflight;

pub const MAX_NAME_BYTES: usize = 256;
pub const FORMAT_VERSION: u32 = 2;
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
pub const MAX_BUNDLE_ACTORS: usize = 32;
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
pub const INITIALIZER_SYMBOL: &str = "wings_scalar_init";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub format: u32,
    pub abi: u32,
    pub target: String,
    pub entry: String,
    pub actors: Vec<String>,
}

impl BundleManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.format != BUNDLE_FORMAT_VERSION || self.abi != SCALAR_ABI_VERSION {
            return Err("unsupported bundle format or scalar ABI version".into());
        }
        if self.target != host_target()? {
            return Err("bundle target does not match this VM host".into());
        }
        if self.actors.is_empty() || self.actors.len() > MAX_BUNDLE_ACTORS {
            return Err("bundle requires 1..=32 actors".into());
        }
        let mut seen = HashSet::new();
        for actor in &self.actors {
            if !valid_name(actor) || actor.contains(['/', '\\', '.']) || !seen.insert(actor) {
                return Err("invalid or duplicate actor name in bundle".into());
            }
        }
        if !seen.contains(&self.entry) {
            return Err("bundle entry actor is not listed in bundle actors".into());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("bundle manifest exceeds 64 KiB".into());
        }
        Ok(bytes)
    }

    pub fn read(directory: &Path) -> Result<Self, String> {
        let path = directory.join("bundle.json");
        manifest_file_size(&std::fs::metadata(&path).map_err(|error| error.to_string())?)?;
        let mut file = File::open(path).map_err(|error| error.to_string())?;
        let length = manifest_file_size(&file.metadata().map_err(|error| error.to_string())?)?;
        let bytes = read_manifest_bytes(&mut file, length)?;
        preflight::check(&bytes)?;
        let manifest: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
}

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
                            | ValueType::StringRef
                            | ValueType::Pid),
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
        if self.state == ValueType::RequestId {
            return Err("RequestId state is not supported".into());
        }
        if self.handlers.iter().any(|handler| {
            handler.reply == Some(ValueType::RequestId)
                || handler
                    .parameters
                    .iter()
                    .any(|(_, ty)| *ty == ValueType::RequestId && !handler.is_completion())
        }) {
            return Err("RequestId is only allowed in completion events".into());
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
        let path = directory.join("manifest.json");
        manifest_file_size(&std::fs::metadata(&path).map_err(|error| error.to_string())?)?;
        let mut file = File::open(path).map_err(|error| error.to_string())?;
        let length = manifest_file_size(&file.metadata().map_err(|error| error.to_string())?)?;
        let bytes = read_manifest_bytes(&mut file, length)?;
        preflight::check(&bytes)?;
        let manifest: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
}

fn read_manifest_bytes(reader: &mut impl Read, length: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|error| error.to_string())?;
    bytes.resize(length, 0);
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    if reader.read(&mut [0]).map_err(|error| error.to_string())? != 0 {
        return Err("artifact manifest changed while reading".into());
    }
    Ok(bytes)
}

fn manifest_file_size(metadata: &std::fs::Metadata) -> Result<usize, String> {
    if !metadata.is_file() {
        return Err("artifact manifest must be a regular file".into());
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err("artifact manifest exceeds 64 KiB".into());
    }
    usize::try_from(metadata.len()).map_err(|error| error.to_string())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    #[test]
    fn fixed_manifest_read_accepts_exact_lengths_and_rejects_growth() {
        for length in [0, 1, MAX_MANIFEST_BYTES as usize] {
            let bytes = vec![b' '; length];
            assert_eq!(
                read_manifest_bytes(&mut Cursor::new(&bytes), length).unwrap(),
                bytes
            );
            let longer = vec![b' '; length + 1];
            let mut reader = Cursor::new(&longer);
            assert_eq!(
                read_manifest_bytes(&mut reader, length).unwrap_err(),
                "artifact manifest changed while reading"
            );
            assert_eq!(reader.position(), length as u64 + 1);
        }
    }

    #[test]
    fn fixed_manifest_read_rejects_truncation_and_read_errors() {
        for length in [1, MAX_MANIFEST_BYTES as usize] {
            let bytes = vec![b' '; length - 1];
            assert!(read_manifest_bytes(&mut Cursor::new(&bytes), length).is_err());
        }
        struct Unavailable;
        impl Read for Unavailable {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unavailable",
                ))
            }
        }
        for length in [0, 1] {
            assert_eq!(
                read_manifest_bytes(&mut Unavailable, length).unwrap_err(),
                "unavailable"
            );
        }
    }
}
