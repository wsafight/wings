use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use wings_vm::{RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker};

const STACK_BYTES: usize = 2 << 20;
const THREAD_BYTES: u64 = STACK_BYTES as u64 + 4096;

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 16,
        messages: 32,
        requests: 32,
    }
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm(limits());
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let module_bytes = vm.used().bytes;
    let increment = module.handler("Inc").unwrap();
    let get = module.handler("Get").unwrap();
    let threads_charge = vm
        .reserve(Resources {
            bytes: 4 * THREAD_BYTES,
            ..Resources::default()
        })
        .unwrap();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(4);
    let mut threads = Vec::new();
    let mut starts = Vec::new();
    for index in 0..4 {
        let module = module.clone();
        let increment = increment.clone();
        let get = get.clone();
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let foreign = ResourceAccount::vm(limits())
            .tenant_accounted(limits())
            .unwrap();
        let (start_sender, start_receiver) = mpsc::sync_channel(1);
        starts.push(start_sender);
        let ready = ready_sender.clone();
        threads.push(
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
                    let mut foreign_worker = ScalarWorker::new(&foreign, 1).unwrap();
                    let baseline = foreign.used();
                    assert!(worker.pids().next().is_none());
                    assert!(foreign_worker.spawn(&module, limits(), 1).is_err());
                    assert_eq!(foreign.used(), baseline);
                    drop(foreign_worker);
                    let pid = worker.spawn(&module, limits(), 1).unwrap();
                    drop(module);
                    ready.send(()).unwrap();
                    start_receiver
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap();
                    for expected in 1..=16 {
                        let request = worker
                            .request(pid, &increment, &[index + 1], 128, Duration::from_secs(60))
                            .unwrap();
                        let mut suspended = false;
                        for _ in 0..32 {
                            let event = worker.tick(1).unwrap();
                            suspended |= matches!(event, wings_vm::WorkerEvent::Suspended(_));
                        }
                        assert!(suspended);
                        assert_eq!(
                            worker.take_reply(request).unwrap(),
                            Some(RequestOutcome::Reply(expected * (index + 1)))
                        );
                    }
                    let request = worker
                        .request(pid, &get, &[], 128, Duration::from_secs(60))
                        .unwrap();
                    for _ in 0..16 {
                        worker.tick(1).unwrap();
                    }
                    let expected = 16 * (index + 1);
                    assert_eq!(
                        worker.take_reply(request).unwrap(),
                        Some(RequestOutcome::Reply(expected))
                    );
                    drop(worker);
                    assert_eq!(tenant.used(), Resources::default());
                    expected
                })
                .unwrap(),
        );
    }
    drop(ready_sender);
    for _ in 0..4 {
        ready_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    }
    drop(module);
    drop(increment);
    for start in starts {
        start.send(()).unwrap();
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results, [16, 32, 48, 64]);
    assert_eq!(
        vm.used(),
        Resources {
            bytes: module_bytes + 4 * THREAD_BYTES,
            ..Resources::default()
        }
    );
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            assert_eq!(get.schema().name, "Get");
            drop(get);
        })
        .unwrap()
        .join()
        .unwrap();
    drop(threads_charge);
    assert_eq!(vm.used(), Resources::default());
    println!(
        "Parallel modules verified: four independent worker threads, shared code lease, VM identity and cross-thread final release"
    );
}
