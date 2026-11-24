use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeAllocatorStats {
    pub requested_bytes: u64,
    pub usable_bytes: u64,
    pub fragmentation_bytes: u64,
    pub peak_usable_bytes: u64,
    pub limit_bytes: u64,
    pub rejected_allocations: u64,
}

/// Global Rust heap ledger and optional hard-ceiling enforcement wrapper over [`System`].
///
/// Tracks exact requested [`Layout`] bytes alongside size-class-rounded usable bytes
/// (`usable_bytes - requested_bytes = fragmentation_bytes`) using deterministic
/// layout-only size-class rounding so `alloc`, `realloc`, and `dealloc` stay symmetric
/// without touching freed pointers.
pub struct WingsAllocator {
    requested: AtomicU64,
    usable: AtomicU64,
    peak_usable: AtomicU64,
    limit: AtomicU64,
    rejected: AtomicU64,
}

impl Default for WingsAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub const fn allocator_usable_size(layout: Layout) -> usize {
    let min_bytes = if layout.size() > layout.align() {
        layout.size()
    } else {
        layout.align()
    };
    if min_bytes == 0 {
        return 0;
    }
    // Standard system allocator (macOS libmalloc tiny/small & glibc ptmalloc) quantum rounding:
    // 16-byte quantum up to 512 B, 64-byte quantum up to 4 KiB, 256-byte quantum up to 16 KiB,
    // and 4 KiB page rounding above 16 KiB.
    let quantum = if min_bytes <= 512 {
        16
    } else if min_bytes <= 4096 {
        64
    } else if min_bytes <= 16384 {
        256
    } else {
        4096
    };
    match min_bytes.checked_add(quantum - 1) {
        Some(rounded) => rounded & !(quantum - 1),
        None => min_bytes,
    }
}

static ACTIVE_REQUESTED: AtomicU64 = AtomicU64::new(0);
static ACTIVE_USABLE: AtomicU64 = AtomicU64::new(0);
static ACTIVE_PEAK_USABLE: AtomicU64 = AtomicU64::new(0);
static ACTIVE_LIMIT: AtomicU64 = AtomicU64::new(0);
static ACTIVE_REJECTED: AtomicU64 = AtomicU64::new(0);

// Global-side deltas are accumulated per thread and flushed in batches so a
// `alloc`/`dealloc` on the hot path does not pay a global RMW every time.
// Instance-level counters stay exact; the global ledger lags by at most
// `FLUSH_AT` bytes per live thread. The global hard-limit check flushes
// first, so enforcement never overshoots beyond a single flush window.
const FLUSH_AT: i64 = 1 << 16;

#[derive(Clone, Copy)]
struct ActivePending {
    requested: i64,
    usable: i64,
}

struct ActiveLedger {
    pending: Cell<ActivePending>,
}

impl Drop for ActiveLedger {
    fn drop(&mut self) {
        flush_active(self.pending.get());
    }
}

thread_local! {
    static ACTIVE_LEDGER: ActiveLedger = const {
        ActiveLedger {
            pending: Cell::new(ActivePending {
                requested: 0,
                usable: 0,
            }),
        }
    };
}

fn flush_active(pending: ActivePending) {
    if pending.requested == 0 && pending.usable == 0 {
        return;
    }
    if pending.requested > 0 {
        ACTIVE_REQUESTED.fetch_add(pending.requested as u64, Ordering::Relaxed);
    } else if pending.requested < 0 {
        ACTIVE_REQUESTED.fetch_sub(pending.requested.unsigned_abs(), Ordering::Relaxed);
    }
    if pending.usable > 0 {
        // `wrapping_add` inside a TLS destructor: a panic while a
        // `#[thread_local]` value is dropped aborts the process, so the peak
        // update saturates instead of checking overflow.
        let next = ACTIVE_USABLE
            .fetch_add(pending.usable as u64, Ordering::Relaxed)
            .saturating_add(pending.usable as u64);
        ACTIVE_PEAK_USABLE.fetch_max(next, Ordering::Relaxed);
    } else if pending.usable < 0 {
        ACTIVE_USABLE.fetch_sub(pending.usable.unsigned_abs(), Ordering::Relaxed);
    }
}

#[inline]
fn active_add(req_delta: i64, usable_delta: i64) {
    // `try_with`, not `with`: allocations can outlive thread-local teardown
    // (TLS destructors, `pthread` internals freeing after `main` TLS dies),
    // and panicking inside those paths aborts the process. When the ledger is
    // gone the delta flushes straight to the atomics instead.
    let applied = ACTIVE_LEDGER.try_with(|ledger| {
        let mut pending = ledger.pending.get();
        pending.requested = pending.requested.saturating_add(req_delta);
        pending.usable = pending.usable.saturating_add(usable_delta);
        if pending.usable.unsigned_abs() >= FLUSH_AT as u64
            || pending.requested.unsigned_abs() >= FLUSH_AT as u64
        {
            flush_active(pending);
            pending = ActivePending {
                requested: 0,
                usable: 0,
            };
        }
        ledger.pending.set(pending);
    });
    if applied.is_err() {
        flush_active(ActivePending {
            requested: req_delta,
            usable: usable_delta,
        });
    }
}

/// Flush this thread's pending global deltas so the ledger below reports a
/// value this thread's own allocations already reflect.
fn active_sync() {
    let _ = ACTIVE_LEDGER.try_with(|ledger| {
        let pending = ledger.pending.get();
        flush_active(pending);
        ledger.pending.set(ActivePending {
            requested: 0,
            usable: 0,
        });
    });
}

pub fn global_allocator_stats() -> NativeAllocatorStats {
    active_sync();
    let requested = ACTIVE_REQUESTED.load(Ordering::Acquire);
    let usable = ACTIVE_USABLE.load(Ordering::Acquire);
    NativeAllocatorStats {
        requested_bytes: requested,
        usable_bytes: usable,
        fragmentation_bytes: usable.saturating_sub(requested),
        peak_usable_bytes: ACTIVE_PEAK_USABLE.load(Ordering::Acquire),
        limit_bytes: ACTIVE_LIMIT.load(Ordering::Acquire),
        rejected_allocations: ACTIVE_REJECTED.load(Ordering::Acquire),
    }
}

pub fn set_global_allocator_limit(limit_bytes: u64) {
    ACTIVE_LIMIT.store(limit_bytes, Ordering::Release);
}

impl WingsAllocator {
    pub const fn new() -> Self {
        Self {
            requested: AtomicU64::new(0),
            usable: AtomicU64::new(0),
            peak_usable: AtomicU64::new(0),
            limit: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    pub fn set_limit_bytes(&self, limit_bytes: u64) {
        self.limit.store(limit_bytes, Ordering::Release);
    }

    pub fn stats(&self) -> NativeAllocatorStats {
        let requested = self.requested.load(Ordering::Acquire);
        let usable = self.usable.load(Ordering::Acquire);
        NativeAllocatorStats {
            requested_bytes: requested,
            usable_bytes: usable,
            fragmentation_bytes: usable.saturating_sub(requested),
            peak_usable_bytes: self.peak_usable.load(Ordering::Acquire),
            limit_bytes: self.limit.load(Ordering::Acquire),
            rejected_allocations: self.rejected.load(Ordering::Acquire),
        }
    }

    #[inline]
    fn try_reserve(&self, req_delta: u64, usable_delta: u64) -> bool {
        let local_limit = self.limit.load(Ordering::Relaxed);
        let global_limit = ACTIVE_LIMIT.load(Ordering::Relaxed);
        let effective_limit = match (local_limit, global_limit) {
            (0, 0) => 0,
            (a, 0) => a,
            (0, b) => b,
            (a, b) => a.min(b),
        };
        if effective_limit != 0 {
            let current = self.usable.load(Ordering::Relaxed);
            if current
                .checked_add(usable_delta)
                .is_none_or(|next| next > effective_limit)
            {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                ACTIVE_REJECTED.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        self.requested.fetch_add(req_delta, Ordering::Relaxed);
        let next_usable = self.usable.fetch_add(usable_delta, Ordering::Relaxed) + usable_delta;
        self.peak_usable.fetch_max(next_usable, Ordering::Relaxed);

        active_add(req_delta as i64, usable_delta as i64);
        true
    }

    #[inline]
    fn release(&self, req_delta: u64, usable_delta: u64) {
        self.requested.fetch_sub(req_delta, Ordering::Relaxed);
        self.usable.fetch_sub(usable_delta, Ordering::Relaxed);
        active_add(-(req_delta as i64), -(usable_delta as i64));
    }
}

unsafe impl GlobalAlloc for WingsAllocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let req = layout.size() as u64;
        let usable = allocator_usable_size(layout) as u64;
        if !self.try_reserve(req, usable) {
            return std::ptr::null_mut();
        }
        let ptr = unsafe { System.alloc(layout) };
        if ptr.is_null() {
            self.release(req, usable);
        }
        ptr
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let req = layout.size() as u64;
        let usable = allocator_usable_size(layout) as u64;
        if !self.try_reserve(req, usable) {
            return std::ptr::null_mut();
        }
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if ptr.is_null() {
            self.release(req, usable);
        }
        ptr
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, old_layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(new_layout) = Layout::from_size_align(new_size, old_layout.align()) else {
            return std::ptr::null_mut();
        };
        let old_req = old_layout.size() as u64;
        let old_usable = allocator_usable_size(old_layout) as u64;
        let new_req = new_layout.size() as u64;
        let new_usable = allocator_usable_size(new_layout) as u64;
        if new_usable > old_usable {
            let add_req = new_req.saturating_sub(old_req);
            let add_usable = new_usable - old_usable;
            if !self.try_reserve(add_req, add_usable) {
                return std::ptr::null_mut();
            }
            let resized = unsafe { System.realloc(ptr, old_layout, new_size) };
            if resized.is_null() {
                self.release(add_req, add_usable);
            } else if new_req < old_req {
                let sub_req = old_req - new_req;
                self.requested.fetch_sub(sub_req, Ordering::Relaxed);
                active_add(-(sub_req as i64), 0);
            }
            resized
        } else {
            let resized = unsafe { System.realloc(ptr, old_layout, new_size) };
            if !resized.is_null() {
                if new_req >= old_req {
                    let add_req = new_req - old_req;
                    self.requested.fetch_add(add_req, Ordering::Relaxed);
                    active_add(add_req as i64, 0);
                } else {
                    let sub_req = old_req - new_req;
                    self.requested.fetch_sub(sub_req, Ordering::Relaxed);
                    active_add(-(sub_req as i64), 0);
                }
                let sub_usable = old_usable - new_usable;
                self.usable.fetch_sub(sub_usable, Ordering::Relaxed);
                active_add(0, -(sub_usable as i64));
            }
            resized
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let req = layout.size() as u64;
        let usable = allocator_usable_size(layout) as u64;
        unsafe { System.dealloc(ptr, layout) };
        self.release(req, usable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_tracks_requested_usable_slack_and_hard_limit() {
        let alloc = WingsAllocator::new();
        let layout = Layout::from_size_align(24, 8).unwrap();
        assert_eq!(allocator_usable_size(layout), 32);
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(!ptr.is_null());
        let stats = alloc.stats();
        assert_eq!(stats.requested_bytes, 24);
        assert_eq!(stats.usable_bytes, 32);
        assert_eq!(stats.fragmentation_bytes, 8);
        assert_eq!(stats.peak_usable_bytes, 32);

        alloc.set_limit_bytes(48);
        let denied = unsafe { alloc.alloc(Layout::from_size_align(32, 8).unwrap()) };
        assert!(denied.is_null());
        assert_eq!(alloc.stats().rejected_allocations, 1);
        assert_eq!(alloc.stats().usable_bytes, 32);

        alloc.set_limit_bytes(0);
        let grown = unsafe { alloc.realloc(ptr, layout, 100) };
        assert!(!grown.is_null());
        let after_grow = alloc.stats();
        assert_eq!(after_grow.requested_bytes, 100);
        assert_eq!(after_grow.usable_bytes, 112);
        assert_eq!(after_grow.fragmentation_bytes, 12);

        let grown_layout = Layout::from_size_align(100, 8).unwrap();
        unsafe { alloc.dealloc(grown, grown_layout) };
        let final_stats = alloc.stats();
        assert_eq!(final_stats.requested_bytes, 0);
        assert_eq!(final_stats.usable_bytes, 0);
        assert_eq!(final_stats.fragmentation_bytes, 0);
    }
}
