use std::path::Path;
use std::time::{Duration, Instant};

use wings_vm::{
    CancellationToken, ExecutionFault, ExecutionPhase, ExecutionStatus, PreemptionToken,
    ResourceAccount, Resources, ScalarModule, ScalarTask, StackPool,
};

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 2,
        messages: 2,
        requests: 2,
    }
}

fn assert_invalid(
    module: &ScalarModule,
    tenant: &ResourceAccount,
    baseline: Resources,
    handler: &str,
) -> Result<(), String> {
    let account = tenant.process_accounted(limits(64 << 20))?;
    let pool = StackPool::new_accounted(tenant, 1, 0)?.for_process(&account)?;
    let mut process = module.instantiate_with_heap(&pool, &account, 16)?;
    let result = match handler {
        "negative_index" => process.invoke(handler, &[]).map(|_| ()),
        _ => process
            .invoke_string(handler, &[], 1_000_000, &CancellationToken::default())
            .map(|_| ()),
    };
    assert!(result.is_err());
    drop((process, pool, account));
    assert_eq!(tenant.used(), baseline);
    Ok(())
}

fn main() -> Result<(), String> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: string_slicing ARTIFACT")?;
    let vm = ResourceAccount::vm_accounted(limits(64 << 20))?;
    let module = unsafe { ScalarModule::load_accounted(Path::new(&path), &vm) }?;
    let tenant = vm.tenant_accounted(limits(48 << 20))?;
    let baseline = tenant.used();
    let account = tenant.process_accounted(limits(64 << 20))?;
    let pool = StackPool::new_accounted(&tenant, 1, 0)?.for_process(&account)?;
    let mut process = module.instantiate_with_heap(&pool, &account, 1024)?;

    for (index, expected) in [65, 233, 20013, 128512, 90].into_iter().enumerate() {
        assert_eq!(process.invoke("at", &[index as i64])?, Some(expected));
    }
    for (start, length, expected) in [
        (0, 5, "Aé中😀Z"),
        (1, 3, "é中😀"),
        (2, 2, "中😀"),
        (5, 0, ""),
        (0, 0, ""),
    ] {
        assert_eq!(
            process
                .invoke_string(
                    "slice",
                    &[start, length],
                    1_000_000,
                    &CancellationToken::default()
                )?
                .as_str(),
            expected
        );
    }
    assert_eq!(
        process
            .invoke_string("boundary", &[], 1_000_000, &CancellationToken::default())?
            .as_str(),
        "😀"
    );
    assert_eq!(process.invoke("fill", &[])?, Some(65536));
    assert_eq!(
        process
            .invoke_string("last", &[], 1_000_000, &CancellationToken::default())?
            .as_str(),
        "a"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut execution = process.start_controlled(
        "slice",
        &[0, 65536],
        1_000_000,
        &CancellationToken::default(),
        &PreemptionToken::default(),
    )?;
    let mut suspensions = 0;
    loop {
        assert!(Instant::now() < deadline);
        match execution.resume(1)? {
            ExecutionStatus::Suspended => suspensions += 1,
            ExecutionStatus::CompleteString(value) => {
                assert_eq!(value.as_str().len(), 65536);
                break;
            }
            other => return Err(format!("unexpected string slice status: {other:?}")),
        }
    }
    assert!(suspensions >= 256);
    drop(execution);
    process.invoke("clear", &[])?;
    process.invoke("sweep", &[])?;
    assert_eq!(process.heap_stats().unwrap().allocated, 1);
    drop((process, pool, account));
    assert_eq!(tenant.used(), baseline);

    let account = tenant.process_accounted(limits(64 << 20))?;
    let pool = StackPool::new_accounted(&tenant, 1, 0)?.for_process(&account)?;
    let mut process = module.instantiate_with_heap(&pool, &account, 16)?;
    process.invoke("fill", &[])?;
    let cancellation = CancellationToken::default();
    let mut execution = process.start_controlled(
        "slice",
        &[0, 65536],
        1_000_000,
        &cancellation,
        &PreemptionToken::default(),
    )?;
    for _ in 0..64 {
        assert_eq!(execution.resume(1)?, ExecutionStatus::Suspended);
    }
    cancellation.cancel();
    assert!(execution.resume(1).is_err());
    drop(execution);
    drop((process, pool, account));
    assert_eq!(tenant.used(), baseline);

    for restricted in 0..3 {
        let account = tenant.process_accounted(limits(32 << 20))?;
        let pool = StackPool::new_accounted(&tenant, 1, 0)?.for_process(&account)?;
        let mut process = module.instantiate_with_heap(&pool, &account, 16)?;
        process.invoke("fill", &[])?;
        process.invoke("sweep", &[])?;
        assert_eq!(process.invoke("length", &[])?, Some(65536));
        let heap = process.heap_stats().unwrap();
        assert_eq!(heap.allocated, 1);
        let before = account.used();
        let mut task = ScalarTask::new(process);
        task.start(
            &module.handler("slice")?,
            &[0, 65536],
            1_000_000,
            &CancellationToken::default(),
            &PreemptionToken::default(),
            &pool,
        )?;
        let owner = [&account, &tenant, &vm][restricted];
        let held = owner.reserve(Resources {
            bytes: owner.limit().bytes - owner.used().bytes,
            ..Resources::default()
        })?;
        assert_eq!(
            task.resume(1_000_000).unwrap_err(),
            "managed record heap exhausted"
        );
        let snapshot = task.snapshot().unwrap();
        assert_eq!(snapshot.phase, ExecutionPhase::Failed);
        assert_eq!(snapshot.fault, Some(ExecutionFault::HeapExhausted));
        assert!(!task.is_active());
        let after = task.heap_stats().unwrap();
        assert_eq!(after.allocated, heap.allocated);
        assert_eq!(after.charged_bytes, heap.charged_bytes);
        drop(held);
        assert_eq!(account.used(), before);
        drop((task, pool, account));
        assert_eq!(tenant.used(), baseline);
    }

    for handler in ["negative_index", "past_end", "negative_length", "overflow"] {
        assert_invalid(&module, &tenant, baseline, handler)?;
    }
    println!(
        "Unicode scalar indexing/slicing verified: bounds, chunk edges, 64 KiB, suspension, cancellation, three-level quota rejection and cleanup"
    );
    Ok(())
}
