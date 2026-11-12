use std::path::Path;
use std::sync::Arc;

use libloading::Library;
use wings_abi::ScalarEntry;
use wings_artifact::{
    Handler, INITIALIZER_SYMBOL, MAX_LIBRARY_BYTES, Manifest, handler_symbol, library_name,
};

use crate::{ScalarProcess, check_status};

#[derive(Clone)]
pub struct ScalarModule {
    pub(crate) inner: Arc<ModuleData>,
}

pub(crate) struct ModuleData {
    pub manifest: Manifest,
    pub initializer: ScalarEntry,
    pub entries: Box<[ScalarEntry]>,
}

#[derive(Clone)]
pub struct HandlerId {
    pub(crate) module: ScalarModule,
    pub(crate) index: usize,
}

impl ScalarModule {
    /// # Safety
    /// The library must contain trusted Wings scalar code matching its manifest.
    /// Entries must honor the ABI and execution controls without unwinding,
    /// out-of-bounds access, or exceeding the fixed execution stack. Failed
    /// slow paths must return normally without writing candidate output.
    /// Shadow frames and slots must be valid aligned live storage, with all
    /// live references published and the previous chain restored on every return.
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
        Ok(Self {
            inner: Arc::new(ModuleData {
                manifest,
                initializer,
                entries: entries.into_boxed_slice(),
            }),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    pub fn handler(&self, name: &str) -> Result<HandlerId, String> {
        let index = self
            .inner
            .manifest
            .handlers
            .iter()
            .position(|handler| handler.name == name)
            .ok_or_else(|| format!("unknown handler {name}"))?;
        Ok(HandlerId {
            module: self.clone(),
            index,
        })
    }

    pub fn instantiate(&self) -> Result<ScalarProcess, String> {
        self.instantiate_in_pool(&crate::execution::default_stack_pool()?)
    }

    pub fn instantiate_in_pool(&self, pool: &crate::StackPool) -> Result<ScalarProcess, String> {
        self.instantiate_owned(pool, None)
    }

    fn instantiate_owned(
        &self,
        pool: &crate::StackPool,
        heap: Option<Box<crate::aot_heap::AotHeap>>,
    ) -> Result<ScalarProcess, String> {
        let (status, output, heap) =
            crate::execution::initialize(self.inner.initializer, pool, heap)?;
        check_status(status)?;
        if !self.inner.manifest.state.accepts(output.state) {
            return Err("initializer returned an invalid scalar state".into());
        }
        Ok(ScalarProcess {
            module: self.clone(),
            state: output.state,
            failed: false,
            heap,
        })
    }

    pub fn instantiate_with_heap(
        &self,
        pool: &crate::StackPool,
        account: &crate::ResourceAccount,
        capacity: usize,
    ) -> Result<ScalarProcess, String> {
        let heap = crate::aot_heap::AotHeap::new(account, capacity)?;
        self.instantiate_owned(pool, Some(heap))
    }
}

impl HandlerId {
    pub fn schema(&self) -> &Handler {
        &self.module.inner.manifest.handlers[self.index]
    }
}
