use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ResourceAccount, Resources, ScalarModule,
    ScalarTask, StackPool,
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

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let limits = Resources {
        bytes: 16 << 20,
        ..Resources::default()
    };
    let vm = ResourceAccount::vm_accounted(limits).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant_accounted(limits).unwrap();
    let account = tenant.process_accounted(limits).unwrap();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let bound = pool.for_process(&account).unwrap();
    let cancellation = CancellationToken::new_accounted(&account).unwrap();
    let preemption = PreemptionToken::new_accounted(&account).unwrap();
    let process = module.instantiate_in_pool(&bound).unwrap();
    let mut task = ScalarTask::new(process);
    let baseline = vm.used();
    let mut expected = 0;
    let mut total_allocations = 0;
    for (name, count) in [
        ("read", 0),
        ("add1", 1),
        ("add8", 8),
        ("add9", 9),
        ("add64", 64),
    ] {
        let handler = module.handler(name).unwrap();
        let mut allocations = 0;
        let mut suspensions = 0;
        for _ in 0..16 {
            let mut values = std::array::from_fn::<_, 64, _>(|index| index as i64 + 1);
            let before = ALLOCATIONS.load(Ordering::SeqCst);
            task.start(
                &handler,
                &values[..count],
                4096,
                &cancellation,
                &preemption,
                &bound,
            )
            .unwrap();
            values.fill(-999);
            expected += (count * (count + 1) / 2) as i64;
            let mut completed = false;
            for _ in 0..4096 {
                match task.resume(1).unwrap() {
                    ExecutionStatus::Suspended => suspensions += 1,
                    ExecutionStatus::Complete(Some(value)) => {
                        assert_eq!(value, expected);
                        completed = true;
                        break;
                    }
                    unexpected => panic!("unexpected scalar completion: {unexpected:?}"),
                }
            }
            assert!(completed);
            allocations += ALLOCATIONS.load(Ordering::SeqCst) - before;
            assert_eq!(vm.used(), baseline);
        }
        if count == 64 {
            assert!(suspensions > 0);
        }
        println!(
            "arguments={count}, calls=16, allocations={allocations}, suspensions={suspensions}, state={expected}"
        );
        assert_eq!(allocations, if count == 0 { 0 } else { 16 });
        total_allocations += allocations;
    }
    let handler = module.handler("add64").unwrap();
    assert!(
        task.start(&handler, &[0; 65], 4096, &cancellation, &preemption, &bound,)
            .is_err()
    );
    assert!(!task.is_active());
    assert_eq!(vm.used(), baseline);
    drop(handler);
    drop((
        task,
        cancellation,
        preemption,
        bound,
        pool,
        module,
        account,
        tenant,
    ));
    assert_eq!(vm.used(), bootstrap);
    assert_eq!(total_allocations, 64);
    println!(
        "Invocation argument baseline: independent caller storage across suspension, no empty-call allocation, one allocation per nonempty call and cleanup; not performance acceptance"
    );
}
