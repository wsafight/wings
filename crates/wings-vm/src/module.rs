use std::path::Path;
use std::sync::Arc;

use libloading::Library;
use wings_abi::ScalarEntry;
use wings_artifact::{
    Handler, INITIALIZER_SYMBOL, MAX_LIBRARY_BYTES, Manifest, handler_symbol, library_name,
};

use crate::{Reservation, ResourceAccount, ResourceLevel, Resources, ScalarProcess, check_status};

#[derive(Clone)]
pub struct ScalarModule {
    pub(crate) inner: Arc<ModuleData>,
}

pub(crate) struct ModuleData {
    pub manifest: Manifest,
    pub initializer: ScalarEntry,
    pub entries: Box<[ScalarEntry]>,
    pub _library: Option<Library>,
    pub lease: Option<ModuleLease>,
}

pub(crate) struct ModuleLease {
    vm: ResourceAccount,
    _charge: Reservation,
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
    /// Code must not retain external callbacks, start background native threads,
    /// or run native destructors after its last module owner is dropped.
    /// Different processes may call entries concurrently on different threads.
    /// Code must not depend on loading-thread TLS or mutable native globals,
    /// and the library must permit final unloading on any owner thread.
    pub unsafe fn load_trusted(directory: &Path) -> Result<Self, String> {
        unsafe { Self::load(directory, None) }
    }

    /// # Safety
    /// The same native code and lifetime requirements as `load_trusted` apply.
    /// The trusted artifact must remain unchanged during validation and loading.
    pub unsafe fn load_accounted(directory: &Path, vm: &ResourceAccount) -> Result<Self, String> {
        if vm.level() != ResourceLevel::Vm {
            return Err("module loading requires a VM resource account".into());
        }
        unsafe { Self::load(directory, Some(vm)) }
    }

    unsafe fn load(directory: &Path, vm: Option<&ResourceAccount>) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let library_path = directory.join(library_name());
        let metadata = library_path
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES {
            return Err("artifact library must be a regular file of at most 64 MiB".into());
        }
        let lease = vm
            .map(|vm| {
                let bytes = metadata.len().div_ceil(4096) * 4096 + (2 << 20);
                let charge = vm.reserve(Resources {
                    bytes,
                    ..Resources::default()
                })?;
                Ok::<_, String>(ModuleLease {
                    vm: vm.clone(),
                    _charge: charge,
                })
            })
            .transpose()?;
        let manifest = Manifest::read(&directory)?;
        let library = unsafe { Library::new(&library_path) }.map_err(|error| error.to_string())?;
        let initializer = unsafe {
            *library
                .get::<ScalarEntry>(INITIALIZER_SYMBOL.as_bytes())
                .map_err(|error| error.to_string())?
        };
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(manifest.handlers.len())
            .map_err(|error| error.to_string())?;
        for index in 0..manifest.handlers.len() {
            entries.push(unsafe {
                *library
                    .get::<ScalarEntry>(handler_symbol(index).as_bytes())
                    .map_err(|error| error.to_string())?
            });
        }
        Ok(Self {
            inner: Arc::new(ModuleData {
                manifest,
                initializer,
                entries: entries.into_boxed_slice(),
                _library: Some(library),
                lease,
            }),
        })
    }

    pub(crate) fn validate_vm(&self, account: &ResourceAccount) -> Result<(), String> {
        if self
            .inner
            .lease
            .as_ref()
            .is_some_and(|lease| !lease.vm.same_vm(account))
        {
            return Err("module belongs to a different VM account".into());
        }
        Ok(())
    }

    pub(crate) fn validate_pool(&self, pool: &crate::StackPool) -> Result<(), String> {
        if let Some(account) = pool.account() {
            self.validate_vm(account)?;
        }
        Ok(())
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
        self.validate_pool(pool)?;
        if self.inner.manifest.state.is_reference() && heap.is_none() {
            return Err("record state requires an explicitly configured accounted heap".into());
        }
        let (status, output, mut heap) =
            crate::execution::initialize(self.inner.initializer, pool, heap)?;
        check_status(status)?;
        crate::commit_state(self.inner.manifest.state, &mut heap, output.state)?;
        Ok(ScalarProcess {
            module: self.clone(),
            state: output.state,
            failed: false,
            heap,
            pool: Some(pool.clone()),
        })
    }

    pub fn instantiate_with_heap(
        &self,
        pool: &crate::StackPool,
        account: &crate::ResourceAccount,
        capacity: usize,
    ) -> Result<ScalarProcess, String> {
        self.validate_vm(account)?;
        if !pool.is_for_process(account) {
            return Err("AOT heap requires a stack pool bound to the same process account".into());
        }
        let heap = crate::aot_heap::AotHeap::new(account, capacity)?;
        self.instantiate_owned(pool, Some(heap))
    }
}

impl HandlerId {
    pub fn schema(&self) -> &Handler {
        &self.module.inner.manifest.handlers[self.index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_data_and_charge_share_one_last_owner_even_across_threads() {
        let vm = ResourceAccount::vm(Resources {
            bytes: 4096,
            ..Resources::default()
        });
        let charge = vm
            .reserve(Resources {
                bytes: 1024,
                ..Resources::default()
            })
            .unwrap();
        unsafe extern "C" fn entry(
            _state: i64,
            _arguments: *const i64,
            _output: *mut wings_abi::ScalarOutput,
            _context: *mut wings_abi::ScalarContext,
        ) -> u32 {
            wings_abi::STATUS_OK
        }
        let module = ScalarModule {
            inner: Arc::new(ModuleData {
                manifest: Manifest {
                    format: wings_artifact::FORMAT_VERSION,
                    abi: wings_abi::SCALAR_ABI_VERSION,
                    target: wings_abi::host_target().unwrap().into(),
                    process: "Lease".into(),
                    state: wings_abi::ValueType::Int,
                    handlers: vec![],
                },
                initializer: entry,
                entries: Box::new([]),
                _library: None,
                lease: Some(ModuleLease {
                    vm: vm.clone(),
                    _charge: charge,
                }),
            }),
        };
        let data = module.inner.clone();
        let weak = Arc::downgrade(&module.inner);
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let module = module.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                drop(module);
            }));
        }
        drop(module);
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(vm.used().bytes, 1024);
        assert_eq!(weak.strong_count(), 1);
        std::thread::spawn(move || drop(data)).join().unwrap();
        assert!(weak.upgrade().is_none());
        assert_eq!(vm.used(), Resources::default());
    }
}
