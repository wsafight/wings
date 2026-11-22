use super::*;

fn finish(worker: &mut ScalarWorker, pid: Pid) {
    for _ in 0..32 {
        match worker.tick(1).unwrap() {
            WorkerEvent::Suspended(actual) => assert_eq!(actual, pid),
            WorkerEvent::Delivered(actual) => {
                assert_eq!(actual, pid);
                return;
            }
            event => panic!("unexpected request delivery: {event:?}"),
        }
    }
    panic!("bounded request did not finish");
}

#[test]
fn result_handle_is_send_sync_and_requests_require_the_correct_protocol() {
    fn assert_send_sync<Value: Send + Sync>() {}
    assert_send_sync::<IngressRequest>();
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let baseline = vm.used();
    assert!(
        worker
            .open_request_ingress(pid, &module.handler("Add").unwrap(), 1, 128)
            .is_err()
    );
    assert_eq!(vm.used(), baseline);
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    let baseline = vm.used();
    assert!(sender.send(&[1]).is_err());
    assert!(sender.request(&[], Duration::from_secs(1)).is_err());
    assert!(sender.request(&[1], Duration::MAX).is_err());
    assert_eq!(vm.used(), baseline);
    drop((sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn reply_follows_state_commit_and_a_taken_result_cannot_be_consumed_again() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 2, 128)
        .unwrap();
    let (mut first, wake) = sender.request(&[7], Duration::from_secs(60)).unwrap();
    assert_eq!(wake, IngressWake::Notified);
    assert_eq!(first.reply_type(), ValueType::Int);
    assert_eq!(first.wait(Duration::ZERO).unwrap(), None);
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    assert_eq!(worker.tick(1).unwrap(), WorkerEvent::Suspended(pid));
    assert_eq!(first.try_take().unwrap(), None);
    finish(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 7);
    assert_eq!(vm.used().requests, 1);
    assert_eq!(first.try_take().unwrap(), Some(RequestOutcome::Reply(7)));
    assert_eq!(vm.used().requests, 0);
    assert!(first.try_take().is_err());
    assert!(first.cancel().is_err());
    assert!(first.wait(Duration::ZERO).is_err());
    drop((first, sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn untaken_results_apply_backpressure_and_slot_reuse_keeps_reply_identity() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    let (mut first, _) = sender.request(&[2], Duration::from_secs(60)).unwrap();
    finish(&mut worker, pid);
    let baseline = vm.used();
    assert!(sender.request(&[5], Duration::from_secs(60)).is_err());
    assert_eq!(vm.used(), baseline);
    assert_eq!(first.try_take().unwrap(), Some(RequestOutcome::Reply(2)));
    let (mut second, _) = sender.request(&[5], Duration::from_secs(60)).unwrap();
    drop(first);
    assert_eq!(second.try_take().unwrap(), None);
    finish(&mut worker, pid);
    assert_eq!(second.try_take().unwrap(), Some(RequestOutcome::Reply(7)));
    drop((second, sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn timeout_and_cancel_do_not_retract_effects_or_release_inflight_capacity() {
    for cancel in [false, true] {
        let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
        worker.enable_wait().unwrap();
        let sender = worker
            .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
            .unwrap();
        let timeout = if cancel {
            Duration::from_secs(60)
        } else {
            Duration::ZERO
        };
        let (mut request, _) = sender.request(&[11], timeout).unwrap();
        if cancel {
            request.cancel().unwrap();
        }
        assert_eq!(
            request.wait(Duration::ZERO).unwrap(),
            Some(if cancel {
                RequestOutcome::Cancelled
            } else {
                RequestOutcome::TimedOut
            })
        );
        assert_eq!(vm.used().requests, 1);
        let baseline = vm.used();
        assert!(sender.request(&[1], Duration::from_secs(60)).is_err());
        assert_eq!(vm.used(), baseline);
        finish(&mut worker, pid);
        assert_eq!(worker.committed_state(pid).unwrap(), 11);
        assert_eq!(vm.used().requests, 0);
        let (mut second, _) = sender.request(&[3], Duration::from_secs(60)).unwrap();
        drop(request);
        assert_eq!(second.try_take().unwrap(), None);
        finish(&mut worker, pid);
        assert_eq!(second.try_take().unwrap(), Some(RequestOutcome::Reply(14)));
        drop((second, sender, worker));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn dropping_caller_before_or_after_completion_keeps_counts_and_control_leases() {
    for complete_first in [false, true] {
        let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
        worker.enable_wait().unwrap();
        let sender = worker
            .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
            .unwrap();
        let (request, _) = sender.request(&[4], Duration::from_secs(60)).unwrap();
        if complete_first {
            finish(&mut worker, pid);
        }
        drop(request);
        assert_eq!(vm.used().requests, u64::from(!complete_first));
        if !complete_first {
            finish(&mut worker, pid);
        }
        assert_eq!(worker.committed_state(pid).unwrap(), 4);
        assert_eq!(vm.used().requests, 0);
        let (mut pending, _) = sender.request(&[9], Duration::from_secs(60)).unwrap();
        drop((sender, worker));
        assert_eq!(vm.used().messages, 0);
        assert_eq!(vm.used().requests, 1);
        assert!(vm.used().bytes > 0);
        assert_eq!(
            pending.wait(Duration::ZERO).unwrap(),
            Some(RequestOutcome::Failed)
        );
        assert_eq!(vm.used().requests, 0);
        drop(pending);
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn stop_and_failure_preserve_first_terminal_result_for_all_queued_messages() {
    for fail in [false, true] {
        let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
        worker.enable_wait().unwrap();
        let handler = module
            .handler(if fail { "FailRequest" } else { "Bump" })
            .unwrap();
        let sender = worker.open_request_ingress(pid, &handler, 2, 128).unwrap();
        let arguments: &[i64] = if fail { &[] } else { &[1] };
        let (mut first, _) = sender.request(arguments, Duration::ZERO).unwrap();
        let (mut second, _) = sender.request(arguments, Duration::from_secs(60)).unwrap();
        if fail {
            assert!(
                matches!(worker.tick(128).unwrap(), WorkerEvent::Exited { pid: actual, .. } if actual == pid)
            );
        } else {
            worker.stop(pid).unwrap();
        }
        assert_eq!(vm.used().messages, 0);
        assert_eq!(first.try_take().unwrap(), Some(RequestOutcome::TimedOut));
        assert_eq!(second.try_take().unwrap(), Some(RequestOutcome::Failed));
        assert!(sender.request(arguments, Duration::from_secs(60)).is_err());
        drop((first, second, sender, worker));
        assert_eq!(vm.used(), Resources::default());
    }
}

#[test]
fn request_quota_rejection_and_registration_failure_roll_back_all_levels() {
    let (vm, tenant, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    for kind in ["requests", "messages", "bytes"] {
        let baseline = vm.used();
        let amount = match kind {
            "requests" => Resources {
                requests: tenant.limit().requests,
                ..Resources::default()
            },
            "messages" => Resources {
                messages: tenant.limit().messages,
                ..Resources::default()
            },
            _ => Resources {
                bytes: tenant.limit().bytes - tenant.used().bytes,
                ..Resources::default()
            },
        };
        let blocker = tenant.reserve(amount).unwrap();
        let blocked = vm.used();
        assert!(sender.request(&[1], Duration::from_secs(60)).is_err());
        assert_eq!(vm.used(), blocked);
        drop(blocker);
        assert_eq!(vm.used(), baseline);
    }
    drop((sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn request_result_retains_account_node_after_actor_and_sender_exit() {
    let vm = ResourceAccount::vm(limits(16 << 20));
    let tenant = vm.tenant_accounted(limits(16 << 20)).unwrap();
    let tenant_metadata = vm.used();
    let probe = tenant.process_accounted(limits(0)).unwrap();
    let node_bytes = tenant.used().bytes;
    drop(probe);
    let module = module();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    let (mut request, _) = sender.request(&[7], Duration::from_secs(60)).unwrap();
    worker.stop(pid).unwrap();
    drop((sender, worker));
    assert_eq!(vm.used().processes, 0);
    assert_eq!(vm.used().messages, 0);
    assert_eq!(vm.used().requests, 1);
    assert!(vm.used().bytes >= tenant_metadata.bytes + node_bytes);
    assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Failed));
    drop(tenant);
    assert!(vm.used().bytes >= tenant_metadata.bytes + node_bytes);
    std::thread::spawn(move || drop(request)).join().unwrap();
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn external_wait_observes_completion_and_honors_its_own_shorter_timeout() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    let (mut request, _) = sender.request(&[13], Duration::from_secs(60)).unwrap();
    assert!(request.wait(Duration::MAX).is_err());
    assert_eq!(request.wait(Duration::from_millis(1)).unwrap(), None);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let signal = barrier.clone();
    let waiter = std::thread::spawn(move || {
        signal.wait();
        request.wait(Duration::from_secs(2)).unwrap()
    });
    barrier.wait();
    finish(&mut worker, pid);
    assert_eq!(waiter.join().unwrap(), Some(RequestOutcome::Reply(13)));
    assert_eq!(worker.committed_state(pid).unwrap(), 13);
    drop((sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn deadline_expires_without_worker_polling_and_late_reply_cannot_overwrite_it() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Bump").unwrap(), 1, 128)
        .unwrap();
    let (mut request, _) = sender.request(&[17], Duration::from_millis(2)).unwrap();
    assert_eq!(
        request.wait(Duration::from_secs(1)).unwrap(),
        Some(RequestOutcome::TimedOut)
    );
    assert_eq!(worker.committed_state(pid).unwrap(), 0);
    finish(&mut worker, pid);
    assert_eq!(worker.committed_state(pid).unwrap(), 17);
    assert_eq!(vm.used().requests, 0);
    drop((request, sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn unit_confirmation_is_a_request_not_a_one_way_send() {
    let (vm, _, mut worker, module, pid) = worker_fixture(1, 1, 1);
    worker.enable_wait().unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Ack").unwrap(), 1, 128)
        .unwrap();
    assert!(sender.send(&[]).is_err());
    let (mut request, _) = sender.request(&[], Duration::from_secs(60)).unwrap();
    assert_eq!(request.reply_type(), ValueType::Unit);
    finish(&mut worker, pid);
    assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Reply(0)));
    drop((request, sender, worker));
    assert_eq!(vm.used(), Resources::default());
}

#[test]
fn bool_reply_keeps_its_schema_and_completed_reply_wins_over_late_cancel() {
    let [vm, tenant, _] = accounts();
    let mut module = module();
    let data = Arc::get_mut(&mut module.inner).unwrap();
    data.manifest.handlers.push(Handler {
        name: "Flag".into(),
        parameters: vec![],
        reply: Some(ValueType::Bool),
        completion: None,
    });
    let mut entries = data.entries.to_vec();
    entries.push(unit as ScalarEntry);
    data.entries = entries.into_boxed_slice();
    let mut worker = ScalarWorker::new(&tenant, 1).unwrap();
    worker.enable_wait().unwrap();
    let pid = worker.spawn(&module, limits(4 << 20), 1).unwrap();
    let sender = worker
        .open_request_ingress(pid, &module.handler("Flag").unwrap(), 1, 128)
        .unwrap();
    let (mut request, _) = sender.request(&[], Duration::from_secs(60)).unwrap();
    finish(&mut worker, pid);
    request.cancel().unwrap();
    assert_eq!(request.reply_type(), ValueType::Bool);
    assert_eq!(request.try_take().unwrap(), Some(RequestOutcome::Reply(0)));
    drop((request, sender, worker));
    assert_eq!(vm.used(), Resources::default());
}
