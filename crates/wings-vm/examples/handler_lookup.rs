use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

struct Allocator;

static MAX_ALLOCATION: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        MAX_ALLOCATION.fetch_max(layout.size(), Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        MAX_ALLOCATION.fetch_max(layout.size(), Ordering::SeqCst);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        MAX_ALLOCATION.fetch_max(size, Ordering::SeqCst);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let limits = Resources {
        bytes: 16 << 20,
        processes: 4,
        messages: 4,
        requests: 4,
    };
    let vm = ResourceAccount::vm_accounted(limits).unwrap();
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm
        .tenant_accounted(Resources {
            bytes: 12 << 20,
            ..limits
        })
        .unwrap();
    let account = tenant
        .process_accounted(Resources {
            bytes: 8 << 20,
            ..limits
        })
        .unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let mut process = module.instantiate_in_pool(&bound).unwrap();
    assert_eq!(process.invoke("Inc", &[7]).unwrap(), Some(7));
    MAX_ALLOCATION.store(0, Ordering::SeqCst);
    let handler = module.handler("Get").unwrap();
    assert_eq!(MAX_ALLOCATION.load(Ordering::SeqCst), 0);
    drop(handler);
    let names = [
        "界".repeat(1 << 20),
        String::new(),
        "missing".into(),
        "x".repeat(256),
        "x".repeat(257),
        "界".repeat(85) + "x",
        "界".repeat(86),
        "x\ny".into(),
        "x\0y".into(),
        "x\u{7f}y".into(),
    ];
    let baseline = vm.used();
    for name in &names {
        for owner in [&account, &tenant, &vm] {
            let held = owner
                .reserve(Resources {
                    bytes: owner.limit().bytes - owner.used().bytes,
                    ..Resources::default()
                })
                .unwrap();
            let before = vm.used();
            for invoke in [false, true] {
                MAX_ALLOCATION.store(0, Ordering::SeqCst);
                let error = if invoke {
                    process.invoke(name, &[]).unwrap_err()
                } else {
                    module.handler(name).err().unwrap()
                };
                let maximum = MAX_ALLOCATION.load(Ordering::SeqCst);
                println!(
                    "input_bytes={}, invoke={invoke}, error_bytes={}, max_allocation={maximum}",
                    name.len(),
                    error.len()
                );
                assert!(error.len() <= 272, "handler diagnostic amplifies input");
                assert!(
                    maximum <= 1024,
                    "handler lookup allocates from external name"
                );
                assert!(!error.chars().any(char::is_control));
                if wings_artifact::valid_name(name) {
                    assert_eq!(error, format!("unknown handler {name}"));
                } else {
                    assert_eq!(error, "invalid handler name");
                }
                assert_eq!(vm.used(), before);
            }
            drop(held);
            assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
        }
        assert_eq!(vm.used(), baseline);
    }
    drop((process, bound, pool, account, tenant, module));
    assert_eq!(vm.used(), bootstrap);
    println!(
        "Handler lookup verified: bounded UTF-8 names and diagnostics, full-account rejection, unchanged state and cleanup; not complete resource acceptance"
    );
}
