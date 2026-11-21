use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionPhase, PreemptionToken, ResourceAccount, Resources,
    ScalarModule, ScalarTask, StackPool,
};

struct Allocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, size) };
        if !resized.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        resized
    }
}

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        ..Resources::default()
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits(16 << 20)).unwrap();
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits(12 << 20)).unwrap();
    let baseline = vm.used();
    for restricted in 0..3 {
        let account = tenant.process_accounted(limits(8 << 20)).unwrap();
        let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&account).unwrap();
        let cancellation = CancellationToken::new_accounted(&account).unwrap();
        let preemption = PreemptionToken::new_accounted(&account).unwrap();
        let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
        assert_eq!(process.invoke("read", &[]).unwrap(), Some(7));
        let mut task = ScalarTask::new(process);
        task.start(
            &module.handler("value").unwrap(),
            &[],
            4096,
            &cancellation,
            &preemption,
            &bound,
        )
        .unwrap();
        let owner = [&account, &tenant, &vm][restricted];
        let held = owner
            .reserve(limits(owner.limit().bytes - owner.used().bytes))
            .unwrap();
        let before = ALLOCATIONS.load(Ordering::SeqCst);
        let error = task.resume(4096).unwrap_err();
        let allocations = ALLOCATIONS.load(Ordering::SeqCst) - before;
        println!("restricted={restricted}, allocations={allocations}, error={error}");
        assert_eq!(error, "managed record heap exhausted");
        let snapshot = task.snapshot().unwrap();
        assert_eq!(snapshot.phase, ExecutionPhase::Failed);
        assert_eq!(snapshot.fault, Some(ExecutionFault::HeapExhausted));
        assert!(!task.is_active());
        assert_eq!(
            allocations, 1,
            "managed quota rejection constructs a discarded error String"
        );
        drop((held, task, bound, pool, preemption, cancellation, account));
        assert_eq!(vm.used(), baseline);
    }
    let account = tenant.process_accounted(limits(8 << 20)).unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 0).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let mut process = module.instantiate_with_heap(&bound, &account, 4).unwrap();
    let reply = process
        .invoke_record("value", &[], 4096, &CancellationToken::default())
        .unwrap();
    assert_eq!(reply.schema().name, "Empty");
    assert_eq!(reply.values().count(), 0);
    assert_eq!(process.invoke("read", &[]).unwrap(), Some(99));
    drop((reply, process, bound, pool, account));
    assert_eq!(vm.used(), baseline);
    drop((module, tenant));
    assert_eq!(vm.used(), bootstrap);
    println!(
        "Quota reply verified: three-level managed rejection, one final host diagnostic allocation and cleanup; not complete resource acceptance"
    );
}
