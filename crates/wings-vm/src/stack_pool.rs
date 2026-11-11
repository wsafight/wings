use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use corosensei::stack::{STACK_ALIGNMENT, Stack, StackPointer};

pub const EXECUTION_STACK_BYTES: usize = 256 * 1024;
const MAX_POOL_SLOTS: usize = 2048;

#[derive(Clone)]
pub struct StackPool {
    inner: Rc<PoolInner>,
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
}

struct PoolInner {
    mapping: NonNull<u8>,
    mapping_bytes: usize,
    slot_bytes: usize,
    page_bytes: usize,
    capacity: usize,
    max_cached: usize,
    state: RefCell<PoolState>,
}

struct PoolState {
    free: Vec<usize>,
    cached: Vec<usize>,
    active: usize,
    reset_failures: u64,
}

pub(crate) struct PooledStack {
    pool: Rc<PoolInner>,
    index: usize,
}

impl StackPool {
    pub fn new(capacity: usize, max_cached: usize) -> Result<Self, String> {
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
                }),
            }),
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
                state.free.push(index);
                return Err(format!(
                    "activate stack slot: {}",
                    std::io::Error::last_os_error()
                ));
            }
            index
        };
        state.active += 1;
        Ok(PooledStack {
            pool: self.inner.clone(),
            index,
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
