use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig};

fn limits() -> Resources {
    Resources {
        bytes: 64 << 20,
        processes: 4,
        messages: 8,
        requests: 8,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let deadline = Instant::now() + Duration::from_secs(3);
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    let scheduler = Scheduler::new(
        &tenant,
        SchedulerConfig {
            workers: 2,
            capacity: 2,
            requests: 2,
            slice_steps: u64::MAX,
            quantum: Duration::from_millis(1),
            yield_samples: 1,
            slice_samples: 0,
            lock_statistics: false,
        },
    )
    .unwrap();
    let handle = scheduler.handle();
    let actors: Vec<_> = (0..2)
        .map(|_| handle.spawn_with_heap(&module, limits(), 1, 4).unwrap())
        .collect();
    for pid in &actors {
        handle
            .send(*pid, &module.handler("Hold").unwrap(), &[], u64::MAX)
            .unwrap();
    }
    for worker in 0..2 {
        while handle.yield_statistics(worker).unwrap().unwrap().dropped == 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    for pid in &actors {
        handle.stop(*pid).unwrap();
    }
    drop(scheduler);
    assert!(Instant::now() < deadline);
    for (worker, pid) in actors.iter().enumerate() {
        let sample = handle.take_yield_sample(worker).unwrap().unwrap();
        assert_eq!(sample.pid, *pid);
        assert_eq!(sample.worker, worker);
        assert_eq!(sample.sequence, 1);
        assert!(sample.observation.yielded_at >= sample.observation.requested_at);
        assert!(handle.take_yield_sample(worker).unwrap().is_none());
        let statistics = handle.yield_statistics(worker).unwrap().unwrap();
        let counters = statistics.preemption.counters;
        assert_eq!(statistics.retained, 0);
        assert_eq!(statistics.taken, 1);
        assert!(statistics.dropped >= 1);
        assert_eq!(statistics.published, statistics.taken + statistics.dropped);
        assert_eq!(counters.confirmed, statistics.published);
        assert_eq!(counters.consumed, counters.confirmed);
        assert_eq!(
            counters.requests,
            counters.coalesced + counters.consumed + counters.cleared
        );
        assert!(statistics.preemption.pending_since.is_none());
        assert!(!statistics.overflowed);
    }
    assert!(handle.last_exit().is_none());
    drop((handle, module, tenant));
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Scheduler atomic yield verified: two worker/Pid identities, bounded FIFO loss, timer accounting and cancellation cleanup; not fairness acceptance"
    );
}
