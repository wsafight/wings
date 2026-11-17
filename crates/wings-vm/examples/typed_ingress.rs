use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    IngressWake, MessageValue, RequestOutcome, ResourceAccount, Resources, ScalarModule,
    ScalarWorker, WorkerEvent,
};

fn limits() -> Resources {
    Resources {
        bytes: 32 << 20,
        processes: 8,
        messages: 16,
        requests: 8,
    }
}

fn values<'value>(
    text: &'value str,
    blob: &'value [u8],
    numbers: &'value [i64],
) -> [MessageValue<'value>; 6] {
    [
        MessageValue::String(text),
        MessageValue::Bytes(blob),
        MessageValue::IntList(numbers),
        MessageValue::RecordInt(7),
        MessageValue::Bool(true),
        MessageValue::Unit,
    ]
}

fn drain(worker: &mut ScalarWorker, deadline: Instant) {
    loop {
        assert!(Instant::now() < deadline, "typed ingress stage deadline");
        if worker.tick(1).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
}

fn verify(module: &ScalarModule, tenant: &ResourceAccount, request_mode: bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let baseline = tenant.used();
    let mut worker = ScalarWorker::with_request_capacity(tenant, 1, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn_with_heap(module, limits(), 1, 72).unwrap();
    let handler = module
        .handler(if request_mode { "Compute" } else { "Store" })
        .unwrap();
    let sender = if request_mode {
        worker
            .open_request_ingress(pid, &handler, 1, 32768)
            .unwrap()
    } else {
        worker.open_ingress(pid, &handler, 1, 32768).unwrap()
    };
    let registered = tenant.used();
    for terminal in 0..4 {
        let producer = sender.clone();
        let query = std::thread::spawn(move || {
            let mut text = String::from("中😀\0");
            let mut blob = vec![17; 65536 - text.len()];
            let mut numbers = [3; 64];
            let query = if request_mode {
                let (query, wake) = producer
                    .request_values(
                        &values(&text, &blob, &numbers),
                        if terminal == 0 {
                            Duration::ZERO
                        } else {
                            Duration::from_secs(2)
                        },
                    )
                    .unwrap();
                assert_eq!(wake, IngressWake::Notified);
                if terminal == 1 {
                    query.cancel().unwrap();
                }
                Some(query)
            } else {
                assert_eq!(
                    producer
                        .send_values(&values(&text, &blob, &numbers))
                        .unwrap(),
                    IngressWake::Notified
                );
                None
            };
            text.clear();
            blob.fill(99);
            numbers.fill(99);
            query
        })
        .join()
        .unwrap();
        let accepted = tenant.used();
        assert_eq!(accepted.messages, registered.messages + 1);
        assert!(accepted.bytes >= registered.bytes + 65536);
        assert!(sender.send(&[0; 6]).is_err());
        if request_mode {
            assert!(
                sender
                    .request_values(&values("", &[], &[]), Duration::ZERO)
                    .is_err()
            );
        } else {
            assert!(sender.send_values(&values("", &[], &[])).is_err());
        }
        assert_eq!(tenant.used(), accepted);
        let mut query = if terminal == 2 {
            drop(query);
            None
        } else {
            query
        };
        if request_mode && terminal < 2 {
            assert_eq!(
                query.as_mut().unwrap().try_take().unwrap(),
                Some(if terminal == 0 {
                    RequestOutcome::TimedOut
                } else {
                    RequestOutcome::Cancelled
                })
            );
        }
        drain(&mut worker, deadline);
        assert_eq!(worker.committed_state(pid).unwrap(), 3_065_539);
        if request_mode && terminal == 3 {
            assert_eq!(
                query.as_mut().unwrap().try_take().unwrap(),
                Some(RequestOutcome::Reply(3_065_539))
            );
        }
        drop(query);
        assert_eq!(tenant.used().messages, registered.messages);
        assert_eq!(tenant.used().requests, registered.requests);
    }
    if request_mode {
        drop(
            sender
                .request_values(&values("x", &[1; 32], &[1]), Duration::from_secs(1))
                .unwrap()
                .0,
        );
    } else {
        sender.send_values(&values("x", &[1; 32], &[1])).unwrap();
    }
    worker.stop(pid).unwrap();
    let closed = tenant.used();
    assert!(sender.send_values(&values("", &[], &[])).is_err());
    assert!(
        sender
            .request_values(&values("", &[], &[]), Duration::ZERO)
            .is_err()
    );
    assert_eq!(tenant.used(), closed);
    drop((sender, worker));
    assert_eq!(tenant.used(), baseline);
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let vm = ResourceAccount::vm_accounted(limits()).unwrap();
    let baseline = vm.used();
    let module = unsafe { ScalarModule::load_accounted(Path::new(&arguments[1]), &vm) }.unwrap();
    let tenant = vm.tenant_accounted(limits()).unwrap();
    for request_mode in [false, true] {
        verify(&module, &tenant, request_mode);
    }
    verify_import_failure(&module, &tenant);
    drop((module, tenant));
    assert_eq!(vm.used(), baseline);
    println!(
        "Typed ingress verified: cross-thread mixed copies, exact limits, GC, retained timeout/cancel/drop, full/closed rejection and cleanup; not MVP acceptance"
    );
}

fn verify_import_failure(module: &ScalarModule, tenant: &ResourceAccount) {
    for stop in [false, true] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let baseline = tenant.used();
        let mut worker = ScalarWorker::new(tenant, 1).unwrap();
        worker.enable_wait().unwrap();
        let pid = worker
            .spawn_with_heap(module, limits(), 1, if stop { 72 } else { 2 })
            .unwrap();
        let sender = worker
            .open_request_ingress(pid, &module.handler("Compute").unwrap(), 1, 32768)
            .unwrap();
        let (mut request, _) = sender
            .request_values(
                &values("中😀", &[17; 65529], &[3; 64]),
                Duration::from_secs(1),
            )
            .unwrap();
        if stop {
            assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
            assert_eq!(worker.committed_state(pid).unwrap(), 0);
            worker.stop(pid).unwrap();
        } else {
            loop {
                assert!(Instant::now() < deadline);
                match worker.tick(1).unwrap() {
                    WorkerEvent::Suspended(actual) => assert_eq!(actual, pid),
                    WorkerEvent::Exited { pid: actual, .. } => {
                        assert_eq!(actual, pid);
                        break;
                    }
                    other => panic!("unexpected typed import failure event: {other:?}"),
                }
            }
            assert_eq!(worker.last_exit().unwrap().committed_state, Some(0));
        }
        assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Failed));
        let closed = tenant.used();
        assert!(
            sender
                .request_values(&values("", &[], &[]), Duration::ZERO)
                .is_err()
        );
        assert_eq!(tenant.used(), closed);
        drop((request, sender, worker));
        assert_eq!(tenant.used(), baseline);
    }
}
