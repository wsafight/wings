use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    IngressRequest, RequestOutcome, ResourceAccount, Resources, ScalarModule, Scheduler,
    SchedulerConfig,
};

fn limits() -> Resources {
    Resources {
        bytes: 128 << 20,
        processes: 32,
        messages: 128,
        requests: 64,
    }
}

fn reply(mut request: IngressRequest) -> i64 {
    match request.wait(Duration::from_secs(5)).unwrap() {
        Some(RequestOutcome::Reply(value)) => value,
        outcome => panic!("unexpected scheduler reply: {outcome:?}"),
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let loops = unsafe { ScalarModule::load_accounted(Path::new(&arguments[2]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    for workers in [1, 2, 3] {
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers,
                capacity: 16,
                requests: 16,
                slice_steps: 1,
                quantum: Duration::from_millis(1),
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        let begin = module.handler("Begin").unwrap();
        let read = module.handler("Read").unwrap();
        let parents: Vec<_> = (0..6)
            .map(|_| handle.spawn_with_heap(&module, limits(), 8, 16).unwrap())
            .collect();
        for (index, pid) in parents.iter().copied().enumerate() {
            assert_eq!(
                reply(
                    handle
                        .request(pid, &begin, &[index as i64], 4096, Duration::from_secs(5))
                        .unwrap()
                ),
                index as i64
            );
        }
        for (index, pid) in parents.iter().copied().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let value = reply(
                    handle
                        .request(pid, &read, &[], 4096, Duration::from_secs(5))
                        .unwrap(),
                );
                if value == 3 * index as i64 + 6 {
                    break;
                }
                assert_eq!(value, -1);
                assert!(Instant::now() < deadline);
            }
            let before = handle.snapshot(pid).unwrap();
            assert!(before.execution.unwrap().heap.unwrap().collections > 0);
            for _ in 0..3 {
                assert_eq!(
                    reply(
                        handle
                            .request(pid, &read, &[], 4096, Duration::from_secs(5))
                            .unwrap()
                    ),
                    3 * index as i64 + 6
                );
            }
            let after = handle.snapshot(pid).unwrap();
            assert_eq!(
                after.migrations - before.migrations,
                if workers == 1 { 0 } else { 3 }
            );
        }
        let busy = handle.spawn(&loops, limits(), 1).unwrap();
        handle
            .send(busy, &loops.handler("Forever").unwrap(), &[], u64::MAX)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.snapshot(busy).unwrap().execution.is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let owner = handle.snapshot(busy).unwrap().owner;
        for pid in &parents {
            reply(
                handle
                    .request(*pid, &read, &[], 4096, Duration::from_secs(5))
                    .unwrap(),
            );
        }
        assert_eq!(handle.snapshot(busy).unwrap().owner, owner);
        handle.stop(busy).unwrap();
        drop((handle, scheduler));
    }
    drop((module, loops, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Multi-worker scheduler verified: N > M actors, live-stack ownership, boundary migration, copied list/record messages, request events, GC and cleanup"
    );
}
