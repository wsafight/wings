use std::path::Path;
use std::sync::Arc;

use libloading::Library;
use wings_abi::ScalarEntry;
use wings_artifact::{
    Handler, INITIALIZER_SYMBOL, MAX_LIBRARY_BYTES, Manifest, handler_symbol, library_name,
};

use crate::{Reservation, ResourceAccount, ResourceLevel, Resources, ScalarProcess, check_status};

const MODULE_CONTROL_BYTES: u64 = 2 << 20;

#[derive(Clone)]
pub struct ScalarModule {
    pub(crate) inner: Arc<ModuleData>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct DirectHandlerMeta {
    pub base_steps: u64,
    pub flags: u64,
}

impl DirectHandlerMeta {
    pub(crate) fn max_steps(self, heap: Option<&crate::aot_heap::AotHeap>) -> Option<u64> {
        if self.base_steps == 0 {
            return None;
        }
        let uses_heap = (self.flags & 1) != 0;
        if !uses_heap {
            return Some(self.base_steps);
        }
        let heap = heap?;
        let stats = heap.stats();
        let chunk_scan = (stats.capacity.div_ceil(16) as u64).max(1);
        let gc_steps = 3 * chunk_scan + (stats.capacity as u64) + 2;
        let state_byte_copies = (self.flags >> 1) & 0xFFFF;
        let record_allocs = (self.flags >> 17) & 0xFFFF;
        let copy_steps = if state_byte_copies != 0 {
            let state_len = heap.committed_bytes_len()?;
            let per_copy = (state_len.div_ceil(256) as u64).max(1) + 2 * chunk_scan + gc_steps;
            state_byte_copies.checked_mul(per_copy)?
        } else {
            0
        };
        let alloc_steps = record_allocs.checked_mul(2 * chunk_scan + gc_steps)?;
        self.base_steps
            .checked_add(copy_steps)?
            .checked_add(alloc_steps)
    }
}

pub(crate) struct ModuleData {
    pub manifest: Manifest,
    pub initializer: ScalarEntry,
    pub entries: Box<[ScalarEntry]>,
    pub direct_meta: Box<[DirectHandlerMeta]>,
    pub _library: Option<Library>,
    pub lease: Option<ModuleLease>,
}

pub(crate) struct ModuleLease {
    vm: ResourceAccount,
    _charge: Reservation,
    _library_charge: Option<Reservation>,
    _image_charge: Option<Reservation>,
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

    /// # Safety
    /// Every actor artifact in `directory/actors/<name>` must satisfy the same
    /// trusted native code and lifetime requirements as `load_trusted`.
    pub unsafe fn load_bundle_trusted(directory: &Path) -> Result<(Vec<Self>, usize), String> {
        unsafe { Self::load_bundle(directory, None) }
    }

    /// # Safety
    /// Every actor artifact in `directory/actors/<name>` must satisfy the same
    /// trusted native code and lifetime requirements as `load_accounted`.
    pub unsafe fn load_bundle_accounted(
        directory: &Path,
        vm: &ResourceAccount,
    ) -> Result<(Vec<Self>, usize), String> {
        if vm.level() != ResourceLevel::Vm {
            return Err("bundle loading requires a VM resource account".into());
        }
        unsafe { Self::load_bundle(directory, Some(vm)) }
    }

    unsafe fn load_bundle(
        directory: &Path,
        vm: Option<&ResourceAccount>,
    ) -> Result<(Vec<Self>, usize), String> {
        let resolved = if vm.is_some() {
            crate::module_path::resolve(directory)?
        } else {
            directory
                .canonicalize()
                .map_err(|error| error.to_string())?
        };
        let bundle = wings_artifact::BundleManifest::read(&resolved)?;
        let actors_dir = resolved.join("actors");
        let mut modules = Vec::new();
        modules
            .try_reserve_exact(bundle.actors.len())
            .map_err(|error| error.to_string())?;
        for actor_name in &bundle.actors {
            let module = unsafe { Self::load(&actors_dir.join(actor_name), vm) }?;
            if &module.manifest().process != actor_name {
                return Err(format!(
                    "bundle actor '{}' manifest declares '{}'",
                    actor_name,
                    module.manifest().process
                ));
            }
            modules.push(module);
        }
        let entry_idx = bundle
            .actors
            .iter()
            .position(|name| name == &bundle.entry)
            .ok_or("bundle entry actor missing")?;
        Ok((modules, entry_idx))
    }

    unsafe fn load(directory: &Path, vm: Option<&ResourceAccount>) -> Result<Self, String> {
        let trace = crate::startup_trace::begin_load();
        crate::startup_trace::mark(trace, "module_load_begin");
        let mut lease = vm
            .map(|vm| {
                let charge = vm.reserve(Resources {
                    bytes: MODULE_CONTROL_BYTES,
                    ..Resources::default()
                })?;
                Ok::<_, String>(ModuleLease {
                    vm: vm.clone(),
                    _charge: charge,
                    _library_charge: None,
                    _image_charge: None,
                })
            })
            .transpose()?;
        let directory = if lease.is_some() {
            crate::module_path::resolve(directory)?
        } else {
            directory
                .canonicalize()
                .map_err(|error| error.to_string())?
        };
        crate::startup_trace::mark(trace, "module_path_ready");
        let library_path = directory.join(library_name());
        let metadata = library_path
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES {
            return Err("artifact library must be a regular file of at most 64 MiB".into());
        }
        crate::startup_trace::mark(trace, "module_metadata_ready");
        if let Some(lease) = lease.as_mut() {
            lease._library_charge = Some(lease.vm.reserve(Resources {
                bytes: metadata.len().div_ceil(4096) * 4096,
                ..Resources::default()
            })?);
        }
        let manifest = Manifest::read(&directory)?;
        crate::startup_trace::mark(trace, "module_manifest_ready");
        if let Some(lease) = lease.as_mut() {
            crate::startup_trace::mark(trace, "module_image_begin");
            let file_bytes = metadata.len().div_ceil(4096) * 4096;
            let image_bytes = crate::module_image::read_image_bytes(&library_path, metadata.len())?;
            lease._image_charge = Some(lease.vm.reserve(Resources {
                bytes: image_bytes.saturating_sub(file_bytes),
                ..Resources::default()
            })?);
            crate::startup_trace::mark(trace, "module_image_ready");
        }
        crate::startup_trace::mark(trace, "module_dlopen_begin");
        let library = unsafe { Library::new(&library_path) }.map_err(|error| error.to_string())?;
        crate::startup_trace::mark(trace, "module_dlopen_ready");
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
        let mut direct_meta = Vec::new();
        if let Ok(raw_meta) = unsafe { library.get::<*const u64>(b"wings_scalar_direct_meta\0") } {
            let base_ptr = *raw_meta;
            if !base_ptr.is_null()
                && direct_meta
                    .try_reserve_exact(manifest.handlers.len())
                    .is_ok()
            {
                for index in 0..manifest.handlers.len() {
                    let word0 = unsafe { base_ptr.add(index * 2).read_unaligned() };
                    let word1 = unsafe { base_ptr.add(index * 2 + 1).read_unaligned() };
                    direct_meta.push(DirectHandlerMeta {
                        base_steps: word0,
                        flags: word1,
                    });
                }
            }
        }
        crate::startup_trace::mark(trace, "module_symbols_ready");
        Ok(Self {
            inner: Arc::new(ModuleData {
                manifest,
                initializer,
                entries: entries.into_boxed_slice(),
                direct_meta: direct_meta.into_boxed_slice(),
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
        if self.inner.lease.is_none() && account.has_accounted_root() {
            return Err("accounted VM requires an accounted module".into());
        }
        Ok(())
    }

    pub(crate) fn validate_pool(&self, pool: &crate::StackPool) -> Result<(), String> {
        if let Some(account) = pool.account() {
            self.validate_vm(account)?;
        }
        if self.inner.lease.is_some()
            && !pool
                .account()
                .is_some_and(|account| account.level() == ResourceLevel::Process)
        {
            return Err("accounted module requires a process-accounted stack pool".into());
        }
        Ok(())
    }

    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    pub fn handler(&self, name: &str) -> Result<HandlerId, String> {
        if !wings_artifact::valid_name(name) {
            return Err("invalid handler name".into());
        }
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
        if self.inner.lease.is_some() {
            return Err(
                "accounted module requires an explicitly configured process-accounted stack pool"
                    .into(),
            );
        }
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
            account: pool
                .account()
                .filter(|account| account.level() == ResourceLevel::Process)
                .cloned(),
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
        let heap = crate::aot_heap::AotHeap::new_with_records(
            account,
            capacity,
            &self.inner.manifest.records,
        )?;
        self.instantiate_owned(pool, Some(heap))
    }
}

impl HandlerId {
    pub fn schema(&self) -> &Handler {
        &self.module.inner.manifest.handlers[self.index]
    }

    pub(crate) fn direct_max_steps(&self, heap: Option<&crate::aot_heap::AotHeap>) -> Option<u64> {
        self.module
            .inner
            .direct_meta
            .get(self.index)
            .copied()
            .and_then(|meta| meta.max_steps(heap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_lookup_rejects_invalid_external_names_without_entry() {
        let module = crate::tests::module();
        let known = module.handler("Get").unwrap();
        assert_eq!(known.schema().name, "Get");
        for name in [
            String::new(),
            "x".repeat(257),
            "界".repeat(86),
            "x\ny".into(),
            "x\0y".into(),
            "x\u{7f}y".into(),
        ] {
            assert_eq!(module.handler(&name).err().unwrap(), "invalid handler name");
        }
        for name in ["missing".into(), "x".repeat(256), "界".repeat(85) + "x"] {
            assert_eq!(
                module.handler(&name).err().unwrap(),
                format!("unknown handler {name}")
            );
        }
    }

    #[test]
    fn module_control_admission_precedes_path_resolution_and_rolls_back() {
        let control = 2 << 20;
        let missing = Path::new("");
        for accounted in [false, true] {
            let bootstrap = if accounted {
                ResourceAccount::vm_accounted(Resources {
                    bytes: control,
                    ..Resources::default()
                })
                .unwrap()
                .used()
                .bytes
            } else {
                0
            };
            for remaining in [0, control - 1, control] {
                let limits = Resources {
                    bytes: bootstrap + remaining,
                    ..Resources::default()
                };
                let vm = if accounted {
                    ResourceAccount::vm_accounted(limits).unwrap()
                } else {
                    ResourceAccount::vm(limits)
                };
                let baseline = vm.used();
                for _ in 0..2 {
                    let error = unsafe { ScalarModule::load_accounted(missing, &vm) }
                        .err()
                        .unwrap();
                    assert_eq!(
                        error.contains("resource_exhausted at Vm account"),
                        remaining < control
                    );
                    assert_eq!(vm.used(), baseline);
                }
            }
        }
    }

    #[test]
    fn accounted_roots_reject_tool_modules_without_entering_or_consuming_images() {
        use crate::{CancellationToken, PreemptionToken, ScalarTask, StackPool};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static ENTRIES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn entry(
            state: i64,
            _arguments: *const i64,
            output: *mut wings_abi::ScalarOutput,
            _context: *mut wings_abi::ScalarContext,
        ) -> u32 {
            ENTRIES.fetch_add(1, Ordering::SeqCst);
            unsafe {
                *output = wings_abi::ScalarOutput {
                    state,
                    reply: state,
                }
            };
            wings_abi::STATUS_OK
        }
        let limits = Resources {
            bytes: 8 << 20,
            ..Resources::default()
        };
        let module = ScalarModule {
            inner: Arc::new(ModuleData {
                manifest: Manifest {
                    records: Vec::new(),
                    format: wings_artifact::FORMAT_VERSION,
                    abi: wings_abi::SCALAR_ABI_VERSION,
                    target: wings_abi::host_target().unwrap().into(),
                    process: "Tool".into(),
                    state: wings_abi::ValueType::Int,
                    handlers: vec![Handler {
                        name: "Get".into(),
                        parameters: vec![],
                        reply: Some(wings_abi::ValueType::Int),
                        completion: None,
                    }],
                },
                initializer: entry,
                entries: vec![entry as ScalarEntry].into_boxed_slice(),
                direct_meta: Box::new([]),
                _library: None,
                lease: None,
            }),
        };
        let vm = ResourceAccount::vm_accounted(limits).unwrap();
        let bootstrap = vm.used();
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let baseline = vm.used();
        let slots = bound.stats();
        assert!(module.instantiate_in_pool(&bound).is_err());
        assert!(module.instantiate_with_heap(&bound, &account, 4).is_err());
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 0);
        assert_eq!(vm.used(), baseline);
        assert_eq!(bound.stats(), slots);
        let tools = StackPool::new(1, 0).unwrap();
        let mut process = module.instantiate_in_pool(&tools).unwrap();
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 1);
        let cancellation = CancellationToken::default();
        let preemption = PreemptionToken::default();
        assert!(
            process
                .start_in_pool("Get", &[], 64, &cancellation, &preemption, &bound)
                .is_err()
        );
        let mut task = ScalarTask::new(process);
        let handler = module.handler("Get").unwrap();
        assert!(
            task.start(&handler, &[], 64, &cancellation, &preemption, &bound)
                .is_err()
        );
        assert!(!task.is_active());
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 1);
        assert_eq!(vm.used(), baseline);
        assert_eq!(bound.stats(), slots);
        let mut image = task.into_process().detach();
        assert!(image.attach(&bound).is_err());
        assert_eq!(vm.used(), baseline);
        assert_eq!(bound.stats(), slots);
        let mut process = image.attach(&tools).unwrap();
        assert!(image.attach(&tools).is_err());
        assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 2);
        drop((
            process, image, handler, module, tools, bound, pool, account, tenant,
        ));
        assert_eq!(vm.used(), bootstrap);
    }

    #[test]
    fn accounted_modules_reject_tool_pools_before_entering_or_consuming_images() {
        use crate::{CancellationToken, PreemptionToken, ScalarTask, StackPool};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static ENTRIES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn entry(
            state: i64,
            _arguments: *const i64,
            output: *mut wings_abi::ScalarOutput,
            _context: *mut wings_abi::ScalarContext,
        ) -> u32 {
            ENTRIES.fetch_add(1, Ordering::SeqCst);
            unsafe {
                *output = wings_abi::ScalarOutput {
                    state,
                    reply: state,
                }
            };
            wings_abi::STATUS_OK
        }
        let limits = Resources {
            bytes: 8 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm_accounted(limits).unwrap();
        let bootstrap = vm.used();
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        let module = ScalarModule {
            inner: Arc::new(ModuleData {
                manifest: Manifest {
                    format: wings_artifact::FORMAT_VERSION,
                    abi: wings_abi::SCALAR_ABI_VERSION,
                    target: wings_abi::host_target().unwrap().into(),
                    process: "Bounded".into(),
                    records: Vec::new(),
                    state: wings_abi::ValueType::Int,
                    handlers: vec![Handler {
                        name: "Get".into(),
                        parameters: vec![],
                        reply: Some(wings_abi::ValueType::Int),
                        completion: None,
                    }],
                },
                initializer: entry,
                entries: vec![entry as ScalarEntry].into_boxed_slice(),
                direct_meta: Box::new([]),
                _library: None,
                lease: Some(ModuleLease {
                    vm: vm.clone(),
                    _library_charge: None,
                    _image_charge: None,
                    _charge: vm
                        .reserve(Resources {
                            bytes: 1024,
                            ..Resources::default()
                        })
                        .unwrap(),
                }),
            }),
        };
        let baseline = vm.used();
        assert!(module.instantiate().is_err());
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 0);
        assert_eq!(vm.used(), baseline);
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let tools = StackPool::new(1, 0).unwrap();
        let baseline = vm.used();
        for invalid in [&tools, &pool] {
            let slots = invalid.stats();
            assert!(module.instantiate_in_pool(invalid).is_err());
            assert_eq!(ENTRIES.load(Ordering::SeqCst), 0);
            assert_eq!(vm.used(), baseline);
            assert_eq!(invalid.stats(), slots);
        }
        let mut process = module.instantiate_in_pool(&bound).unwrap();
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 1);
        let cancellation = CancellationToken::default();
        let preemption = PreemptionToken::default();
        for invalid in [&tools, &pool] {
            let slots = invalid.stats();
            assert!(
                process
                    .start_in_pool("Get", &[], 64, &cancellation, &preemption, invalid)
                    .is_err()
            );
            assert_eq!(ENTRIES.load(Ordering::SeqCst), 1);
            assert_eq!(vm.used(), baseline);
            assert_eq!(invalid.stats(), slots);
        }
        let mut task = ScalarTask::new(process);
        let handler = module.handler("Get").unwrap();
        assert!(
            task.start(&handler, &[], 64, &cancellation, &preemption, &tools)
                .is_err()
        );
        assert!(!task.is_active());
        assert_eq!(vm.used(), baseline);
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 1);
        let mut image = task.into_process().detach();
        for invalid in [&tools, &pool] {
            assert!(image.attach(invalid).is_err());
            assert_eq!(vm.used(), baseline);
        }
        let mut process = image.attach(&bound).unwrap();
        assert!(image.attach(&bound).is_err());
        assert_eq!(process.invoke("Get", &[]).unwrap(), Some(0));
        assert_eq!(ENTRIES.load(Ordering::SeqCst), 2);
        assert_eq!(vm.used(), baseline);
        drop((
            process, image, handler, module, bound, pool, tools, account, tenant,
        ));
        assert_eq!(vm.used(), bootstrap);
    }

    #[test]
    fn module_data_and_charge_share_one_last_owner_even_across_threads() {
        let vm = ResourceAccount::vm(Resources {
            bytes: 4096,
            ..Resources::default()
        });
        let charge = vm
            .reserve(Resources {
                bytes: 512,
                ..Resources::default()
            })
            .unwrap();
        let library_charge = vm
            .reserve(Resources {
                bytes: 256,
                ..Resources::default()
            })
            .unwrap();
        let image_charge = vm
            .reserve(Resources {
                bytes: 256,
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
                    records: Vec::new(),
                    state: wings_abi::ValueType::Int,
                    handlers: vec![],
                },
                initializer: entry,
                entries: Box::new([]),
                direct_meta: Box::new([]),
                _library: None,
                lease: Some(ModuleLease {
                    vm: vm.clone(),
                    _charge: charge,
                    _library_charge: Some(library_charge),
                    _image_charge: Some(image_charge),
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
