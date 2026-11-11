use std::path::Path;

use libloading::Library;
use wings_abi::{
    STATUS_DIVISION_BY_ZERO, STATUS_OK, STATUS_OVERFLOW, ScalarEntry, ScalarOutput, ValueType,
};
use wings_artifact::{
    INITIALIZER_SYMBOL, MAX_LIBRARY_BYTES, Manifest, handler_symbol, library_name,
};

pub struct ScalarProcess {
    manifest: Manifest,
    entries: Vec<ScalarEntry>,
    state: i64,
    failed: bool,
}

impl ScalarProcess {
    /// # Safety
    /// The library must be trusted Wings scalar AOT code matching its manifest,
    /// with valid entry signatures, bounded pointer access and no unwinding.
    pub unsafe fn load_trusted(directory: &Path) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let manifest = Manifest::read(&directory)?;
        let library_path = directory.join(library_name());
        let metadata = library_path
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES {
            return Err("artifact library must be a regular file of at most 64 MiB".into());
        }
        let library = unsafe { Library::new(&library_path) }.map_err(|error| error.to_string())?;
        let initializer = unsafe {
            *library
                .get::<ScalarEntry>(INITIALIZER_SYMBOL.as_bytes())
                .map_err(|error| error.to_string())?
        };
        let mut entries = Vec::new();
        for index in 0..manifest.handlers.len() {
            entries.push(unsafe {
                *library
                    .get::<ScalarEntry>(handler_symbol(index).as_bytes())
                    .map_err(|error| error.to_string())?
            });
        }
        std::mem::forget(library);
        let mut output = ScalarOutput::default();
        let status = unsafe { initializer(0, [].as_ptr(), &mut output) };
        check_status(status)?;
        if !manifest.state.accepts(output.state) {
            return Err("initializer returned an invalid scalar state".into());
        }
        Ok(Self {
            manifest,
            entries,
            state: output.state,
            failed: false,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn invoke(&mut self, name: &str, arguments: &[i64]) -> Result<Option<i64>, String> {
        if self.failed {
            return Err("scalar process has terminated after a managed failure".into());
        }
        let index = self
            .manifest
            .handlers
            .iter()
            .position(|handler| handler.name == name)
            .ok_or_else(|| format!("unknown handler {name}"))?;
        let handler = &self.manifest.handlers[index];
        if arguments.len() != handler.parameters.len()
            || arguments
                .iter()
                .zip(&handler.parameters)
                .any(|(value, (_, ty))| !ty.accepts(*value))
        {
            return Err("handler argument count or scalar type mismatch".into());
        }
        let mut output = ScalarOutput::default();
        let status = unsafe { (self.entries[index])(self.state, arguments.as_ptr(), &mut output) };
        if let Err(error) = check_status(status) {
            self.failed = true;
            return Err(error);
        }
        if !self.manifest.state.accepts(output.state)
            || !handler
                .reply
                .unwrap_or(ValueType::Unit)
                .accepts(output.reply)
        {
            self.failed = true;
            return Err("entry returned an invalid scalar state or reply".into());
        }
        self.state = output.state;
        Ok(handler.reply.map(|_| output.reply))
    }
}

fn check_status(status: u32) -> Result<(), String> {
    match status {
        STATUS_OK => Ok(()),
        STATUS_OVERFLOW => Err("managed integer overflow".into()),
        STATUS_DIVISION_BY_ZERO => Err("managed division by zero".into()),
        _ => Err(format!("unknown scalar ABI status {status}")),
    }
}
