use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use corosensei::stack::{STACK_ALIGNMENT, Stack, StackPointer};

use crate::{Reservation, ResourceAccount, ResourceLevel, Resources};

pub const EXECUTION_STACK_BYTES: usize = 256 * 1024;
const MAX_POOL_SLOTS: usize = 2048;

#[derive(Clone)]
pub struct StackPool {
    inner: Rc<PoolInner>,
    process: Option<ResourceAccount>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackPoolStats {
    pub capacity: usize,
    pub active: usize,
    pub cached: usize,
    pub available: usize,
    pub reserved_bytes: usize,
    pub usable_bytes_per_stack: usize,
    pub guard_bytes_per_stack: usize,
    pub reset_failures: u64,
    pub charged_bytes: u64,
}

struct PoolInner {
    mapping: NonNull<u8>,
    mapping_bytes: usize,
    slot_bytes: usize,
    page_bytes: usize,
    capacity: usize,
    max_cached: usize,
    state: RefCell<PoolState>,
    accounting: Option<PoolAccounting>,
}

struct PoolState {
    free: Vec<usize>,
    cached: Vec<usize>,
    active: usize,
    reset_failures: u64,
    charges: Vec<Option<Reservation>>,
}

struct PoolAccounting {
    tenant: ResourceAccount,
    _metadata: Reservation,
    charged_bytes: u64,
}

pub(crate) struct PooledStack {
    pool: Rc<PoolInner>,
    index: usize,
    charge: Option<Reservation>,
}

impl StackPool {
    pub fn new(capacity: usize, max_cached: usize) -> Result<Self, String> {
        Self::create(capacity, max_cached, None)
    }

    pub fn new_accounted(
        tenant: &ResourceAccount,
        capacity: usize,
        max_cached: usize,
    ) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant {
            return Err("accounted stack pool requires a tenant account".into());
        }
        Self::create(capacity, max_cached, Some(tenant))
    }

    pub fn for_process(&self, process: &ResourceAccount) -> Result<Self, String> {
        let accounting = self
            .inner
            .accounting
            .as_ref()
            .ok_or("stack pool is not accounted")?;
        if process.level() != ResourceLevel::Process || !process.is_child_of(&accounting.tenant) {
            return Err("stack pool requires a process in its tenant".into());
        }
        Ok(Self {
            inner: self.inner.clone(),
            process: Some(process.clone()),
        })
    }

    fn create(
        capacity: usize,
        max_cached: usize,
        tenant: Option<&ResourceAccount>,
    ) -> Result<Self, String> {
        wings_abi::host_target()?;
        if !(1..=MAX_POOL_SLOTS).contains(&capacity) || max_cached > capacity {
            return Err("stack pool requires 1..=2048 slots and max_cached <= capacity".into());
        }
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| "failed to query OS page size")?;
        if !page_bytes.is_power_of_two() || page_bytes < STACK_ALIGNMENT {
            return Err("unsupported OS stack page size".into());
        }
        let usable_bytes = EXECUTION_STACK_BYTES
            .checked_add(page_bytes - 1)
            .map(|bytes| bytes & !(page_bytes - 1))
            .ok_or("stack size overflow")?;
        let slot_bytes = usable_bytes
            .checked_add(page_bytes)
            .ok_or("stack size overflow")?;
        let mapping_bytes = slot_bytes
            .checked_mul(capacity)
            .ok_or("stack pool size overflow")?;
        let accounting = tenant
            .map(|tenant| {
                let metadata_bytes = std::mem::size_of::<PoolInner>()
                    + std::mem::size_of::<StackPool>()
                    + 2 * std::mem::size_of::<usize>()
                    + capacity
                        * (2 * std::mem::size_of::<usize>()
                            + std::mem::size_of::<Option<Reservation>>());
                let metadata = tenant.reserve(Resources {
                    bytes: metadata_bytes as u64,
                    ..Resources::default()
                })?;
                Ok::<_, String>(PoolAccounting {
                    tenant: tenant.clone(),
                    _metadata: metadata,
                    charged_bytes: (metadata_bytes + mapping_bytes) as u64,
                })
            })
            .transpose()?;
        let mut charges = Vec::new();
        if let Some(accounting) = &accounting {
            charges
                .try_reserve_exact(capacity)
                .map_err(|error| error.to_string())?;
            for _ in 0..capacity {
                charges.push(Some(accounting.tenant.reserve(Resources {
                    bytes: slot_bytes as u64,
                    ..Resources::default()
                })?));
            }
        }
        let mut free = Vec::new();
        free.try_reserve_exact(capacity)
            .map_err(|error| error.to_string())?;
        free.extend((0..capacity).rev());
        let mut cached = Vec::new();
        cached
            .try_reserve_exact(capacity)
            .map_err(|error| error.to_string())?;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping_bytes,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(format!(
                "reserve stack pool: {}",
                std::io::Error::last_os_error()
            ));
        }
        let Some(mapping) = NonNull::new(mapping.cast::<u8>()) else {
            unsafe { libc::munmap(mapping, mapping_bytes) };
            return Err("stack pool received a null mapping".into());
        };
        Ok(Self {
            inner: Rc::new(PoolInner {
                mapping,
                mapping_bytes,
                slot_bytes,
                page_bytes,
                capacity,
                max_cached,
                state: RefCell::new(PoolState {
                    free,
                    cached,
                    active: 0,
                    reset_failures: 0,
                    charges,
                }),
                accounting,
            }),
            process: None,
        })
    }

    pub fn stats(&self) -> StackPoolStats {
        let state = self.inner.state.borrow();
        StackPoolStats {
            capacity: self.inner.capacity,
            active: state.active,
            cached: state.cached.len(),
            available: self.inner.capacity - state.active,
            reserved_bytes: self.inner.mapping_bytes,
            usable_bytes_per_stack: self.inner.slot_bytes - self.inner.page_bytes,
            guard_bytes_per_stack: self.inner.page_bytes,
            reset_failures: state.reset_failures,
            charged_bytes: self
                .inner
                .accounting
                .as_ref()
                .map_or(0, |accounting| accounting.charged_bytes),
        }
    }

    pub fn trim(&self) -> Result<(), String> {
        let mut state = self.inner.state.borrow_mut();
        while let Some(index) = state.cached.pop() {
            if let Err(error) = self.inner.reset(index) {
                state.cached.push(index);
                state.reset_failures = state.reset_failures.saturating_add(1);
                return Err(error);
            }
            state.free.push(index);
        }
        Ok(())
    }

    pub(crate) fn acquire(&self) -> Result<PooledStack, String> {
        let mut state = self.inner.state.borrow_mut();
        let index = state
            .cached
            .last()
            .or(state.free.last())
            .copied()
            .ok_or("execution stack pool exhausted")?;
        let mut charge = if self.inner.accounting.is_some() {
            let process = self
                .process
                .as_ref()
                .ok_or("accounted stack pool must be bound to a process")?;
            let charge = state.charges[index]
                .as_mut()
                .expect("available stack charge");
            charge.transfer(process)?;
            state.charges[index].take()
        } else {
            None
        };
        let index = if let Some(index) = state.cached.pop() {
            index
        } else {
            let index = state.free.pop().ok_or("execution stack pool exhausted")?;
            let usable = unsafe { self.inner.slot(index).add(self.inner.page_bytes) };
            let status = unsafe {
                libc::mprotect(
                    usable.cast(),
                    self.inner.slot_bytes - self.inner.page_bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            if status != 0 {
                let error = std::io::Error::last_os_error();
                state.free.push(index);
                if let Some(mut ticket) = charge.take() {
                    ticket
                        .transfer(
                            &self
                                .inner
                                .accounting
                                .as_ref()
                                .expect("pool accounting")
                                .tenant,
                        )
                        .expect("return reserved stack capacity");
                    state.charges[index] = Some(ticket);
                }
                return Err(format!("activate stack slot: {error}"));
            }
            index
        };
        state.active += 1;
        Ok(PooledStack {
            pool: self.inner.clone(),
            index,
            charge,
        })
    }
}

impl PoolInner {
    fn slot(&self, index: usize) -> *mut u8 {
        unsafe { self.mapping.as_ptr().add(index * self.slot_bytes) }
    }

    fn reset(&self, index: usize) -> Result<(), String> {
        let address = self.slot(index).cast();
        let replacement = unsafe {
            libc::mmap(
                address,
                self.slot_bytes,
                libc::PROT_NONE,
                libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if replacement == libc::MAP_FAILED {
            return Err(format!(
                "reset stack slot: {}",
                std::io::Error::last_os_error()
            ));
        }
        if replacement != address {
            std::process::abort();
        }
        Ok(())
    }
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.mapping.as_ptr().cast(), self.mapping_bytes) };
    }
}

unsafe impl Stack for PooledStack {
    fn base(&self) -> StackPointer {
        StackPointer::new(unsafe { self.pool.slot(self.index).add(self.pool.slot_bytes) } as usize)
            .expect("nonzero stack base")
    }

    fn limit(&self) -> StackPointer {
        StackPointer::new(self.pool.slot(self.index) as usize).expect("nonzero stack limit")
    }
}

impl Drop for PooledStack {
    fn drop(&mut self) {
        let mut state = self.pool.state.borrow_mut();
        if let Some(mut charge) = self.charge.take() {
            charge
                .transfer(
                    &self
                        .pool
                        .accounting
                        .as_ref()
                        .expect("pool accounting")
                        .tenant,
                )
                .expect("return reserved stack capacity");
            state.charges[self.index] = Some(charge);
        }
        state.active -= 1;
        if state.cached.len() < self.pool.max_cached {
            state.cached.push(self.index);
        } else if self.pool.reset(self.index).is_ok() {
            state.free.push(self.index);
        } else {
            state.reset_failures = state.reset_failures.saturating_add(1);
            state.cached.push(self.index);
        }
    }
}
