use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 8,
        messages: 256,
        requests: 128,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
            requests: 128,
            slice_steps: 1,
            ..SchedulerConfig::default()
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let busy = handle.spawn_with_heap(&module, limits(), 100, 4).unwrap();
    let healthy = handle.spawn_with_heap(&module, limits(), 1, 4).unwrap();
    let add = module.handler("add").unwrap();
    handle
        .send(busy, &module.handler("hold").unwrap(), &[], u64::MAX)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while handle.snapshot(busy).unwrap().execution.is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let owner = handle.snapshot(busy).unwrap().owner;
    let mut pending = Vec::with_capacity(100);
    for _ in 0..100 {
        pending.push(
            handle
                .request(busy, &add, &[1], 4096, Duration::from_secs(3))
                .unwrap(),
        );
    }
    assert_eq!(handle.snapshot(busy).unwrap().queued, 100);
    let baseline = vm.used();
    assert_eq!(
        handle
            .request(busy, &add, &[1], 4096, Duration::from_secs(3))
            .err()
            .unwrap(),
        "mailbox_full"
    );
    assert_eq!(vm.used(), baseline);
    let mut heartbeat = handle
        .request(healthy, &add, &[7], 4096, Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        heartbeat.wait(Duration::from_secs(3)).unwrap(),
        Some(RequestOutcome::Reply(7))
    );
    drop(heartbeat);
    assert_eq!(handle.snapshot(busy).unwrap().owner, owner);
    handle.stop(busy).unwrap();
    for mut request in pending {
        assert_eq!(
            request.wait(Duration::from_secs(3)).unwrap(),
            Some(RequestOutcome::Failed)
        );
    }
    while handle.snapshot(busy).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let state = handle.byte_state_snapshot(healthy).unwrap().unwrap();
    assert_eq!(state.length, 4096);
    assert_eq!(i64::from_be_bytes(state.prefix[..8].try_into().unwrap()), 7);
    assert!(Instant::now() < deadline);
    assert!(handle.last_exit().is_none());
    drop((scheduler, handle, add, module, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "100-slot full mailbox rejected overflow, preserved healthy Bytes actor and released pending requests; not an open-loop benchmark"
    );
}
