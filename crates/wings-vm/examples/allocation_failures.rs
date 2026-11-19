use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionStatus, FieldType, MessageValue, PreemptionToken,
    RecordHeap, RecordValue, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarTask,
    ScalarWorker, ShadowRoots, StackPool, WorkerEvent,
};

struct Allocator;

static COUNTDOWN: AtomicUsize = AtomicUsize::new(0);
static MATCH_BYTES: AtomicUsize = AtomicUsize::new(0);
static REMAINING: AtomicUsize = AtomicUsize::new(0);
static INJECTED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_INJECTED: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn reject(size: usize) -> bool {
    if REMAINING.load(Ordering::SeqCst) == 0 {
        return false;
    }
    let expected = MATCH_BYTES.load(Ordering::SeqCst);
    if expected != 0 && expected != size {
        return false;
    }
    let count = COUNTDOWN.load(Ordering::SeqCst);
    if count > 1 {
        COUNTDOWN.store(count - 1, Ordering::SeqCst);
        return false;
    }
    REMAINING.fetch_sub(1, Ordering::SeqCst);
    INJECTED.fetch_add(1, Ordering::SeqCst);
    TOTAL_INJECTED.fetch_add(1, Ordering::SeqCst);
    true
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if reject(layout.size()) {
            return std::ptr::null_mut();
        }
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if reject(layout.size()) {
            return std::ptr::null_mut();
        }
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if reject(size) {
            return std::ptr::null_mut();
        }
        let resized = unsafe { System.realloc(pointer, layout, size) };
        if !resized.is_null() {
            LIVE.fetch_add(size as isize - layout.size() as isize, Ordering::SeqCst);
        }
        resized
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) };
    }
}

fn arm(index: usize, size: usize, count: usize) {
    assert_eq!(REMAINING.load(Ordering::SeqCst), 0);
    assert!(index > 0 && count > 0);
    COUNTDOWN.store(index, Ordering::SeqCst);
    MATCH_BYTES.store(size, Ordering::SeqCst);
    INJECTED.store(0, Ordering::SeqCst);
    REMAINING.store(count, Ordering::SeqCst);
}

fn disarm(expected: usize) {
    let remaining = REMAINING.swap(0, Ordering::SeqCst);
    assert_eq!(remaining, 0);
    assert_eq!(INJECTED.load(Ordering::SeqCst), expected);
}

fn require_limits() {
    for (resource, maximum) in [(libc::RLIMIT_CPU, 20), (libc::RLIMIT_CORE, 0)] {
        let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        assert_eq!(unsafe { libc::getrlimit(resource, limits.as_mut_ptr()) }, 0);
        assert!(unsafe { limits.assume_init() }.rlim_cur <= maximum);
    }
}

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 4,
        requests: 4,
    }
}

fn fixed_heap(accounts: [&ResourceAccount; 3]) {
    let before = accounts.map(ResourceAccount::used);
    for index in 1..=4 {
        let live = LIVE.load(Ordering::SeqCst);
        arm(index, 0, 1);
        let result = RecordHeap::new(accounts[2], 8, 4);
        disarm(1);
        let error = result.err().expect("injected heap allocation must fail");
        assert!(!error.is_empty());
        drop(error);
        assert_eq!(accounts.map(ResourceAccount::used), before);
        assert_eq!(LIVE.load(Ordering::SeqCst), live);
    }
    let mut heap = RecordHeap::new(accounts[2], 8, 4).unwrap();
    let layout = heap.register_layout(&[FieldType::Int]).unwrap();
    let reference = heap.allocate(layout, &[RecordValue::Int(7)]).unwrap();
    let before = accounts.map(ResourceAccount::used);
    let live = LIVE.load(Ordering::SeqCst);
    arm(1, 0, 1);
    let result = ShadowRoots::new(&heap, 8);
    disarm(1);
    drop(result.err().expect("injected roots allocation must fail"));
    assert_eq!(accounts.map(ResourceAccount::used), before);
    assert_eq!(LIVE.load(Ordering::SeqCst), live);
    let mut roots = ShadowRoots::new(&heap, 8).unwrap();
    let mut frame = roots.frame();
    frame.push(&heap, reference).unwrap();
    assert_eq!(frame.collect(&mut heap).unwrap().retained, 1);
    assert_eq!(heap.get(reference, 0).unwrap(), RecordValue::Int(7));
}

fn aot_heap(module: &ScalarModule, accounts: [&ResourceAccount; 3], pool: &StackPool) {
    assert_eq!(module.manifest().records.len(), 1);
    let before = accounts.map(ResourceAccount::used);
    let stack = pool.stats();
    for index in 1..=7 {
        let live = LIVE.load(Ordering::SeqCst);
        arm(index, 0, 1);
        let result = module.instantiate_with_heap(pool, accounts[2], 8);
        disarm(1);
        drop(
            result
                .err()
                .expect("injected AOT heap allocation must fail"),
        );
        assert_eq!(accounts.map(ResourceAccount::used), before);
        assert_eq!(pool.stats(), stack);
        assert_eq!(LIVE.load(Ordering::SeqCst), live);
    }
}

fn invocation(module: &ScalarModule, accounts: [&ResourceAccount; 3], pool: &StackPool) {
    let cancellation = CancellationToken::new_accounted(accounts[2]).unwrap();
    let preemption = PreemptionToken::new_accounted(accounts[2]).unwrap();
    let process = module.instantiate_with_heap(pool, accounts[2], 8).unwrap();
    let mut task = ScalarTask::new(process);
    let handler = module.handler("Touch").unwrap();
    let before = accounts.map(ResourceAccount::used);
    let heap = task.heap_stats();
    let stack = pool.stats();
    let live = LIVE.load(Ordering::SeqCst);
    arm(1, std::mem::size_of::<i64>(), 1);
    let result = task.start(&handler, &[1], 4096, &cancellation, &preemption, pool);
    disarm(1);
    drop(result.expect_err("injected invocation argument allocation must fail"));
    assert!(!task.is_active());
    assert_eq!(task.heap_stats(), heap);
    assert_eq!(accounts.map(ResourceAccount::used), before);
    assert_eq!(pool.stats(), stack);
    assert_eq!(LIVE.load(Ordering::SeqCst), live);
    task.start(&handler, &[1], 4096, &cancellation, &preemption, pool)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline);
        match task.resume(64).unwrap() {
            ExecutionStatus::Suspended => {}
            ExecutionStatus::Complete(Some(value)) => {
                assert_eq!(value, 8);
                break;
            }
            unexpected => panic!("unexpected invocation outcome {unexpected:?}"),
        }
    }
}

fn managed_buffer(module: &ScalarModule, tenant: &ResourceAccount, failures: usize) {
    let before = tenant.used();
    let mut worker = ScalarWorker::new(tenant, 2).unwrap();
    let failing = worker.spawn_with_heap(module, limits(), 1, 8).unwrap();
    let healthy = worker.spawn_with_heap(module, limits(), 1, 8).unwrap();
    let old = worker.byte_state_snapshot(failing).unwrap().unwrap();
    assert_eq!(old.length, 256);
    assert_eq!(&old.prefix[..8], &7i64.to_be_bytes());
    let request = worker
        .request(
            failing,
            &module.handler("Replace").unwrap(),
            &[65536],
            4096,
            Duration::from_secs(3),
        )
        .unwrap();
    arm(1, 65536, failures);
    let deadline = Instant::now() + Duration::from_secs(3);
    let terminal = loop {
        assert!(Instant::now() < deadline);
        match worker.tick(64).unwrap() {
            WorkerEvent::Suspended(actual) if actual == failing => {}
            event => break event,
        }
    };
    disarm(failures);
    if failures == 1 {
        assert_eq!(terminal, WorkerEvent::Delivered(failing));
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Reply(65536))
        );
        let state = worker.byte_state_snapshot(failing).unwrap().unwrap();
        assert_eq!(state.length, 65536);
        assert_eq!(state.prefix, [9; 32]);
    } else {
        assert!(matches!(terminal, WorkerEvent::Exited { pid, .. } if pid == failing));
        assert_eq!(
            worker.take_reply(request).unwrap(),
            Some(RequestOutcome::Failed)
        );
        let exit = worker.last_exit().unwrap();
        assert_eq!(exit.pid, failing);
        assert_eq!(exit.execution.fault, Some(ExecutionFault::HeapExhausted));
        let state = exit.byte_state.unwrap();
        assert_eq!(state.length, old.length);
        assert_eq!(state.prefix, old.prefix);
        assert_eq!(worker.process_count(), 1);
    }
    let request = worker
        .request(
            healthy,
            &module.handler("Touch").unwrap(),
            &[1],
            4096,
            Duration::from_secs(3),
        )
        .unwrap();
    loop {
        assert!(Instant::now() < deadline);
        match worker.tick(64).unwrap() {
            WorkerEvent::Suspended(actual) if actual == healthy => {}
            event => {
                assert_eq!(event, WorkerEvent::Delivered(healthy));
                break;
            }
        }
    }
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Reply(8))
    );
    assert_eq!(
        worker.byte_state_snapshot(healthy).unwrap().unwrap().length,
        256
    );
    drop(worker);
    assert_eq!(tenant.used(), before);
}

fn host_payload(module: &ScalarModule, accounts: [&ResourceAccount; 3]) {
    let mut worker = ScalarWorker::new(accounts[1], 1).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 1, 8).unwrap();
    let handler = module.handler("Echo").unwrap();
    let payload = vec![5; 65536];
    let before = accounts.map(ResourceAccount::used);
    let snapshot = worker.byte_state_snapshot(pid).unwrap();
    let live = LIVE.load(Ordering::SeqCst);
    arm(1, 65536, 1);
    let result = worker.request_values(
        pid,
        &handler,
        &[MessageValue::Bytes(&payload)],
        4096,
        Duration::from_secs(3),
    );
    disarm(1);
    drop(result.expect_err("injected host payload copy must fail"));
    assert_eq!(accounts.map(ResourceAccount::used), before);
    assert_eq!(worker.queued(pid).unwrap(), 0);
    assert_eq!(worker.byte_state_snapshot(pid).unwrap(), snapshot);
    assert_eq!(LIVE.load(Ordering::SeqCst), live);
    let request = worker
        .request_values(
            pid,
            &handler,
            &[MessageValue::Bytes(&payload)],
            4096,
            Duration::from_secs(3),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline);
        match worker.tick(64).unwrap() {
            WorkerEvent::Suspended(actual) if actual == pid => {}
            event => {
                assert_eq!(event, WorkerEvent::Delivered(pid));
                break;
            }
        }
    }
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Reply(65536))
    );
    let state = worker.byte_state_snapshot(pid).unwrap().unwrap();
    assert_eq!(state.length, 65536);
    assert_eq!(state.prefix, [5; 32]);
}

fn reply_payload(module: &ScalarModule, tenant: &ResourceAccount) {
    let before = tenant.used();
    let mut worker = ScalarWorker::new(tenant, 1).unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 1, 8).unwrap();
    let old = worker.byte_state_snapshot(pid).unwrap();
    let request = worker
        .request(
            pid,
            &module.handler("ReplyCandidate").unwrap(),
            &[],
            4096,
            Duration::from_secs(3),
        )
        .unwrap();
    arm(2, 4096, 1);
    let deadline = Instant::now() + Duration::from_secs(3);
    let event = loop {
        assert!(Instant::now() < deadline);
        match worker.tick(64).unwrap() {
            WorkerEvent::Suspended(actual) if actual == pid => {}
            event => break event,
        }
    };
    disarm(1);
    assert!(matches!(event, WorkerEvent::Exited { pid: actual, .. } if actual == pid));
    assert_eq!(
        worker.take_reply(request).unwrap(),
        Some(RequestOutcome::Failed)
    );
    let exit = worker.last_exit().unwrap();
    assert_eq!(exit.execution.fault, Some(ExecutionFault::HeapExhausted));
    assert_eq!(exit.byte_state, old);
    drop(worker);
    assert_eq!(tenant.used(), before);
}

fn module_loading(directory: &Path, accounts: [&ResourceAccount; 3]) -> ScalarModule {
    let canonical = directory.canonicalize().unwrap();
    let requested = canonical.as_os_str().len();
    assert!(requested > 0);
    drop(canonical);
    let before = accounts.map(ResourceAccount::used);
    let live = LIVE.load(Ordering::SeqCst);
    arm(1, requested, 1);
    let result = unsafe { ScalarModule::load_accounted(directory, accounts[0]) };
    disarm(1);
    let error = result
        .err()
        .expect("injected module path storage allocation must fail");
    assert_eq!(error, "module path storage allocation failed");
    drop(error);
    assert_eq!(accounts.map(ResourceAccount::used), before);
    assert_eq!(LIVE.load(Ordering::SeqCst), live);
    unsafe { ScalarModule::load_accounted(directory, accounts[0]) }.unwrap()
}

fn main() {
    require_limits();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let bootstrap = vm.used();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let account = tenant.process_accounted(limits()).unwrap();
    let accounts = [&vm, &tenant, &account];
    let account_baselines = accounts.map(ResourceAccount::used);
    fixed_heap(accounts);
    let module = module_loading(Path::new(&arguments[1]), accounts);
    let pool = StackPool::new_accounted(&tenant, 1, 1).unwrap();
    let bound = pool.for_process(&account).unwrap();
    aot_heap(&module, accounts, &bound);
    invocation(&module, accounts, &bound);
    managed_buffer(&module, &tenant, 1);
    managed_buffer(&module, &tenant, 2);
    host_payload(&module, accounts);
    reply_payload(&module, &tenant);
    drop((bound, pool, module));
    assert_eq!(accounts.map(ResourceAccount::used), account_baselines);
    drop((account, tenant));
    assert_eq!(vm.used(), bootstrap);
    assert_eq!(TOTAL_INJECTED.load(Ordering::SeqCst), 19);
    println!(
        "{}",
        json!({
            "status": "ok", "fixed_heap_failures": 4, "root_failures": 1,
            "module_path_failures": 1,
            "aot_heap_failures": 7, "invocation_failures": 1, "managed_recovered_failures": 1,
            "managed_terminal_failures": 2, "healthy_replies": 2, "invocation_retry_value": 8,
            "host_payload_failures": 1, "reply_payload_failures": 1,
            "total_injected_failures": TOTAL_INJECTED.load(Ordering::SeqCst),
            "resources_released": true, "bootstrap_bytes": bootstrap.bytes,
            "remaining_bytes": vm.used().bytes, "metric": "rust_requested_storage_not_rss_or_complete_resource_acceptance"
        })
    );
}
