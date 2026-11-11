use std::hint::black_box;
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use serde_json::json;
use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ScalarModule, ScalarProcess, StackPool,
};

fn phase(name: &str, elapsed_ns: u128, roundtrip_ns: f64) {
    println!(
        "{}",
        json!({"phase": name, "elapsed_ns": elapsed_ns, "switch_roundtrip_ns": roundtrip_ns})
    );
    io::stdout().flush().expect("flush");
    let mut input = String::new();
    assert_ne!(io::stdin().read_line(&mut input).expect("next phase"), 0);
}

#[inline(never)]
fn hold_stack(yielder: &Yielder<bool, ()>, frames: usize) {
    let mut bytes = [0u8; 4096];
    for index in (0..bytes.len()).step_by(64) {
        unsafe {
            bytes
                .as_mut_ptr()
                .add(index)
                .write_volatile((index + frames) as u8)
        };
    }
    if frames > 1 {
        hold_stack(yielder, frames - 1);
    } else {
        while yielder.suspend(()) {}
    }
    black_box(&bytes);
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(arguments.len(), 5);
    let mode = &arguments[0];
    let count: usize = arguments[1].parse().expect("count");
    let stack_kib: usize = arguments[2].parse().expect("stack KiB");
    let frames: usize = arguments[3].parse().expect("stack frames");
    assert!(count <= 2048);
    assert!(matches!(stack_kib, 32 | 64 | 256));
    assert!(frames <= 16 && frames * 4 + 16 <= stack_kib);
    if mode.starts_with("vm-") || mode.starts_with("pool-") || mode.starts_with("shared-") {
        probe_vm(mode, count, Path::new(&arguments[4]));
        return;
    }
    if mode == "switch" {
        assert_eq!(count, 1);
        probe_switch(stack_kib);
        return;
    }
    assert!(matches!(mode.as_str(), "unstarted" | "suspended"));
    phase("baseline", 0, 0.0);
    let started = Instant::now();
    let mut coroutines: Vec<Coroutine<bool, (), ()>> = (0..count)
        .map(|_| {
            let stack = DefaultStack::new(stack_kib * 1024).expect("allocate probe stack");
            Coroutine::with_stack(stack, move |yielder, run| {
                if run {
                    if frames == 0 {
                        while yielder.suspend(()) {}
                    } else {
                        hold_stack(yielder, frames);
                    }
                }
            })
        })
        .collect();
    if mode != "unstarted" {
        for coroutine in &mut coroutines {
            assert!(matches!(coroutine.resume(true), CoroutineResult::Yield(())));
        }
    }
    phase("allocated", started.elapsed().as_nanos(), 0.0);
    let started = Instant::now();
    for coroutine in &mut coroutines {
        if coroutine.started() && !coroutine.done() {
            assert!(matches!(
                coroutine.resume(false),
                CoroutineResult::Return(())
            ));
        }
    }
    drop(coroutines);
    phase("released", started.elapsed().as_nanos(), 0.0);
}

fn probe_switch(stack_kib: usize) {
    phase("baseline", 0, 0.0);
    let started = Instant::now();
    let stack = DefaultStack::new(stack_kib * 1024).expect("allocate probe stack");
    let mut coroutine = Coroutine::with_stack(stack, |yielder, mut sequence: u64| {
        while sequence != 0 {
            sequence = yielder.suspend(sequence + 1);
        }
    });
    assert!(matches!(coroutine.resume(1), CoroutineResult::Yield(2)));
    phase("allocated", started.elapsed().as_nanos(), 0.0);
    for sequence in 1..=10_000 {
        assert!(
            matches!(coroutine.resume(black_box(sequence)), CoroutineResult::Yield(value) if value == sequence + 1)
        );
    }
    let rounds = 20_000_000;
    let started = Instant::now();
    for sequence in 1..=rounds {
        assert!(
            matches!(coroutine.resume(black_box(sequence)), CoroutineResult::Yield(value) if value == sequence + 1)
        );
    }
    let roundtrip_ns = started.elapsed().as_nanos() as f64 / rounds as f64;
    let started = Instant::now();
    assert!(matches!(coroutine.resume(0), CoroutineResult::Return(())));
    drop(coroutine);
    phase("released", started.elapsed().as_nanos(), roundtrip_ns);
}

fn probe_vm(mode: &str, count: usize, directory: &Path) {
    assert!(matches!(
        mode,
        "vm-idle"
            | "vm-suspended"
            | "pool-uncached"
            | "pool-cached"
            | "pool-warm"
            | "shared-idle"
            | "shared-suspended"
    ));
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    let module = unsafe { ScalarModule::load_trusted(directory) }.expect("trusted probe module");
    let mut warmup = module.instantiate().expect("trusted probe");
    assert_eq!(warmup.invoke("Work", &[7]), Ok(Some(1464)));
    drop(warmup);
    phase("baseline", 0, 0.0);
    let started = Instant::now();
    let max_cached = match mode {
        "pool-uncached" => 0,
        "pool-warm" => count,
        _ => 2.min(count),
    };
    let pool = StackPool::new(count, max_cached).expect("bounded probe pool");
    let mut processes: Vec<_> = (0..count)
        .map(|_| {
            if mode.starts_with("shared-") {
                module.instantiate().expect("shared instance")
            } else {
                unsafe { ScalarProcess::load_trusted(directory) }.expect("trusted probe")
            }
        })
        .collect();
    let mut executions = Vec::new();
    if !mode.ends_with("idle") {
        for process in &mut processes {
            let mut execution = process
                .start_in_pool("Work", &[7], 147, &cancellation, &preemption, &pool)
                .expect("start probe");
            assert_eq!(execution.resume(98), Ok(ExecutionStatus::Suspended));
            executions.push(execution);
        }
    }
    phase("allocated", started.elapsed().as_nanos(), 0.0);
    let started = Instant::now();
    for execution in &mut executions {
        assert_eq!(
            execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(1464)))
        );
    }
    drop(executions);
    for process in &mut processes {
        if !mode.ends_with("idle") {
            assert_eq!(process.invoke("Get", &[]), Ok(Some(1464)));
        }
    }
    drop(processes);
    phase("released", started.elapsed().as_nanos(), 0.0);
    if mode.starts_with("pool-") {
        assert_eq!(pool.stats().active, 0);
        let started = Instant::now();
        pool.trim().expect("trim probe pool");
        assert_eq!(pool.stats().cached, 0);
        assert_eq!(pool.stats().reset_failures, 0);
        phase("trimmed", started.elapsed().as_nanos(), 0.0);
    }
}
