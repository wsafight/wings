use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use wings_vm::{ResourceAccount, Resources};

struct Allocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::SeqCst);
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::SeqCst);
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, size) };
        if !resized.is_null() {
            LIVE.fetch_add(size, Ordering::SeqCst);
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        resized
    }
}

fn main() {
    let (mutex_bytes, before_mutex) = {
        let mutex = Mutex::new(());
        let before = LIVE.load(Ordering::SeqCst);
        drop(mutex.lock().unwrap());
        (LIVE.load(Ordering::SeqCst) - before, before)
    };
    assert_eq!(LIVE.load(Ordering::SeqCst), before_mutex);
    let (condvar_bytes, before_condvar) = {
        let condvar = Condvar::new();
        let before = LIVE.load(Ordering::SeqCst);
        condvar.notify_all();
        (LIVE.load(Ordering::SeqCst) - before, before)
    };
    assert_eq!(LIVE.load(Ordering::SeqCst), before_condvar);
    #[cfg(target_os = "macos")]
    let (mutex_limit, condvar_limit) = (
        std::mem::size_of::<libc::pthread_mutex_t>(),
        std::mem::size_of::<libc::pthread_cond_t>(),
    );
    #[cfg(not(target_os = "macos"))]
    let (mutex_limit, condvar_limit) = (0, 0);
    assert!(mutex_bytes <= mutex_limit);
    assert!(condvar_bytes <= condvar_limit);

    let baseline = LIVE.load(Ordering::SeqCst);
    let vm = ResourceAccount::vm_accounted(Resources {
        bytes: 1 << 20,
        ..Resources::default()
    })
    .unwrap();
    let created = LIVE.load(Ordering::SeqCst) - baseline;
    let before_used = ALLOCATIONS.load(Ordering::SeqCst);
    let charged = vm.used().bytes;
    let first_used_allocations = ALLOCATIONS.load(Ordering::SeqCst) - before_used;
    let observed = LIVE.load(Ordering::SeqCst) - baseline;
    drop(vm);
    let released = LIVE.load(Ordering::SeqCst) == baseline;
    println!(
        "mutex_bytes={mutex_bytes}, condvar_bytes={condvar_bytes}, root_created_bytes={created}, root_used_bytes={charged}, root_live_bytes={observed}, first_used_allocations={first_used_allocations}, released={released}"
    );
    assert_eq!(
        observed as u64, charged,
        "VM bootstrap misses native storage"
    );
    assert_eq!(first_used_allocations, 0, "published gate allocates on use");
    assert!(released);

    let exact = Resources {
        bytes: charged,
        ..Resources::default()
    };
    assert!(
        ResourceAccount::vm_accounted(Resources {
            bytes: charged - 1,
            ..Resources::default()
        })
        .is_err()
    );
    let exact_baseline = LIVE.load(Ordering::SeqCst);
    let vm = ResourceAccount::vm_accounted(exact).unwrap();
    let before_operations = ALLOCATIONS.load(Ordering::SeqCst);
    for _ in 0..1024 {
        let reservation = vm.reserve(Resources::default()).unwrap();
        assert_eq!(vm.used(), exact);
        drop(reservation);
    }
    assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), before_operations);
    assert_eq!(
        LIVE.load(Ordering::SeqCst) - exact_baseline,
        charged as usize
    );
    assert!(
        vm.reserve(Resources {
            bytes: 1,
            ..Resources::default()
        })
        .is_err()
    );
    assert_eq!(vm.used(), exact);
    drop(vm);
    assert_eq!(LIVE.load(Ordering::SeqCst), exact_baseline);
    println!(
        "exact_root_bytes={charged}, zero_reservations=1024, operation_allocations=0, released=true"
    );
}
