# Evaluation & Acceptance

**English** | [简体中文](zh/evaluation.md) · [Back to Overview](../README.md) · [Product Positioning](product.md) · [Runtime](runtime.md) · [Risks & Roadmap](roadmap.md)

Wings aims to **achieve both higher sustained throughput and lower whole-machine memory footprint than BEAM under identical stateful workloads**. Functional correctness does not imply benchmark completion; every claim must report workload parameters, host environment, and tail-latency bounds.

## Reference Workloads & Correctness Suites

The repository maintains semantically equivalent Wings and Erlang/OTP workloads under `scripts/` and `crates/wings-vm/examples/`:

1. **State & Protocol Equivalence**: Covers single/multi-actor counters, legal two-sender interleavings, 64 KiB `bytes` TCP framing & pipelining (`verify_tcp_bytes.py`), concurrent request cancellation, divide-by-zero faults, and out-of-process supervision soak tests (`verify_recovery_soak.py`, `verify_failure_soak.py`).
2. **Scheduled Open-Loop Throughput & Service Latency (`measure_open_loop.py`)**:
   - Uses a fixed-schedule open-loop generator (preventing closed-loop clients from masking queueing delay), reporting end-to-end latency (`observed - planned`), true service latency (`observed - sent`), and generator schedule slip alongside `mailbox_full` rejections, timeouts, and process-tree memory samples.
   - Evaluates `mixed` (sparse active sessions) and `hot-queue` (single-recipient high contention) across 5 alternating paired runs (30s warmup + 60s measurement each).
3. **Preemptive Fairness & Heartbeat Tail Latency (`measure_fairness.py`)**: Injects bursty lightweight heartbeat requests alongside compute-saturated actors, verifying p99 / p999 heartbeat latency, zero timeouts, and full resource reclamation.
4. **Bounded Fault Injection**: Runs real system-allocator `null` injection (`verify_allocation_failures.py`) and infinite-loop atomic yield regressions (`verify_atomic_progress.py`) inside isolated child processes bounded by `RLIMIT_CPU`, wall-clock timeouts, and disabled core dumps.

## Current Measured Status (macOS AArch64)

From recent 5-pair alternating long runs (30s warmup / 60s measurement, Wings vs. Erlang/OTP 29):

- **Preemptive Fairness & Mixed Sessions**: Both Wings and BEAM pass 5/5 runs with zero rejections and zero timeouts; Wings shows exploratory advantages in compute completion rate and process-tree RSS / physical footprint, with median heartbeat p99 around `645 µs` (BEAM ~`441 µs`) and lower median p999 than BEAM.
- **Hot-Queue Open-Loop Service Latency**: Wings completes 5/5 runs with zero rejections, but single-actor contended service latency p99 (`observed - sent`, ~`35.9–46.2 µs`) remains higher than BEAM (~`5.3–10.0 µs`).
- **Tested & Reverted Candidates**: Two notification-pruning experiments (eliding unawaited result wakeups and broadcasting scheduler finish only on newly ready transitions) increased `mailbox_full` rejections or tail jitter in paired runs and were reverted per our experimental discipline.
- **Gate Verdict**: **The formal throughput-and-memory dual MVP gate is not yet closed** (pending hot-queue p99 optimization and paired validation under native Linux x86-64 cgroup v2 hard CPU quotas).

## Optimization Discipline

Aggressive throughput or memory optimizations (superblock check coalescing, fiber reuse, lock-free mailbox batching, buffer pooling, compact object layouts, or future generational/compacting GC) must follow three rules:

1. **Never Trade Away Safety Boundaries**: Mandatory safepoints, native stack bounds checks, and 3-level resource admission must never be disabled to inflate benchmark numbers.
2. **Attribute Before Changing**: Use built-in optional lock statistics (`--observe-locks`) and per-thread CPU attribution to separate OS generator scheduling slip from internal VM contention.
3. **Paired A/B Validation & Decisive Rollback**: Evaluate each candidate across 5 alternating paired runs measuring throughput, p99/p999, rejections, and platform memory (Linux RSS/PSS, macOS RSS/physical footprint); revert any change that adds complexity without durable gains.

## Standard Verification Commands

```sh
# 1. Formatting, lints, and workspace unit/integration tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# 2. Resource-bounded workspace test runner
python3 scripts/verify_workspace.py --output target/workspace-validation/debug

# 3. Full end-to-end native AOT verification (Debug & Release)
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_scalar_aot.py

cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py
```
