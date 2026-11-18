use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use wings_vm::{ResourceAccount, Resources, ScalarModule};

struct Allocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static LARGEST: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn record(size: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
    TOTAL.fetch_add(size, Ordering::SeqCst);
    LARGEST.fetch_max(size, Ordering::SeqCst);
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, size) };
        if !resized.is_null() {
            record(size);
        }
        resized
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        ..Resources::default()
    }
}

fn main() {
    let vm = ResourceAccount::vm_accounted(limits(16 << 20)).unwrap();
    let bootstrap = vm.used();
    let paths = [
        ("ascii-large", vec![b'x'; 3 << 20]),
        ("non-utf8-large", vec![0xff; 3 << 20]),
        ("nul-large", vec![0; 3 << 20]),
        ("one-past", vec![b'x'; (16 << 10) + 1]),
        ("exact", vec![b'x'; 16 << 10]),
        ("empty", Vec::new()),
    ];
    let mut cases = Vec::with_capacity(paths.len() * 2);
    for (name, bytes) in paths {
        let input_bytes = bytes.len();
        let directory = PathBuf::from(OsString::from_vec(bytes));
        for available in [2 << 20, 0] {
            let held = vm
                .reserve(limits(vm.limit().bytes - vm.used().bytes - available))
                .unwrap();
            let baseline = vm.used();
            ALLOCATIONS.store(0, Ordering::SeqCst);
            TOTAL.store(0, Ordering::SeqCst);
            LARGEST.store(0, Ordering::SeqCst);
            let result = unsafe { ScalarModule::load_accounted(&directory, &vm) };
            let allocations = ALLOCATIONS.load(Ordering::SeqCst);
            let total = TOTAL.load(Ordering::SeqCst);
            let largest = LARGEST.load(Ordering::SeqCst);
            let error = match result {
                Ok(_) => panic!("invalid probe path unexpectedly loaded a module"),
                Err(error) => error,
            };
            assert_eq!(vm.used(), baseline);
            cases.push(json!({"case": name, "input_bytes": input_bytes, "available_bytes": available,
                "allocations": allocations, "total_requested_bytes": total, "largest_requested_bytes": largest,
                "error_bytes": error.len(), "error_prefix": error.chars().take(128).collect::<String>(),
                "resources_released": true}));
            drop(held);
            assert_eq!(vm.used(), bootstrap);
        }
    }
    println!(
        "{}",
        json!({"status": "ok", "version": 1,
        "metric": "rust_allocation_requests_not_live_bytes_or_rss", "cases": cases,
        "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        "resources_released": true})
    );
}
