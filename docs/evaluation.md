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

## Current Measured Status (Dual-Platform Paired CI & CPU-Quota Runs)

Automated paired runs in `.github/workflows/benchmark.yml` compare Wings and Erlang/OTP 28 across `hot-queue`, `sparse-256` (10,000 actors), `mixed-4k` (1,000 actors), and `fairness` (101 actors) on both **Linux x86-64 under `cgroup v2` hard CPU quotas (`cpu.max = 20000 10000`, `cpu.max.burst = 0`, 3s warmup / 5s sample × 3 alternating pairs)** and **macOS AArch64 (`macos-15`, 5s warmup / 5s sample × 3 alternating pairs)**:

- **Linux x86-64 (`cgroup v2` 2.0-vCPU Hard Quota, OTP 28)**:
  - **Open-Loop Suites**: Wings passes 3/3 clean runs with zero rejections/timeouts across `hot-queue` (`10,000` on-time RPS, median service p99 `~16.2 µs` vs. BEAM `~5.1 µs`, peak PSS `~41.0 MB` vs. `~70.2 MB`), `sparse-256` (`1,000` RPS, 10,000 actors, service p99 `~22.8 µs`, PSS `~61.7 MB` vs. `~98.5 MB`), and `mixed-4k` (`1,000` RPS, service p99 `~24.6 µs`, PSS `~33.4 MB` vs. `~66.9 MB`).
  - **Preemptive Fairness**: Wings passes 3/3 clean runs with zero rejections/timeouts, higher CPU completion rate (`~102.2/s` vs. BEAM `~88.2/s`), comparable median heartbeat p99 (`~445 µs` vs. `~444 µs`), and `~2.8×` lower PSS (`~22.3 MB` vs. `~63.1 MB`).
- **macOS AArch64 (`macos-15` Virtual M1 & Local Apple Silicon)**:
  - Passes all four paired suites with lower physical footprint than BEAM across `hot-queue` (`~45.0 MB` vs. `~70.7 MB`, median service p99 `~130 µs` vs. `~226 µs`), `sparse-256` (`~64.0–64.7 MB` vs. `~96.5–98.0 MB`), `mixed-4k` (`~36.1–37.3 MB` vs. `~65.3–65.8 MB`), and `fairness` (`~22.9–23.1 MB` vs. `~59.1–59.6 MB`). Earlier 30s/60s 5-pair bare-metal Apple Silicon runs similarly passed 5/5 fairness runs (`~645 µs` median heartbeat p99).
- **Remaining Boundaries & Caveats**:
  - While `hot-queue` passes the sub-`100 µs` service p99 gate on Linux `cgroup v2`, single-actor contended service p99 (`~16 µs`) remains ~3× higher than BEAM's in-process reduction loop (`~5 µs`).
  - CI runs use bounded 3–5s windows (`WINGS_BENCH_WINDOW`) on shared cloud VMs so pull requests stay fast (`ci.yml` skips multi-minute benchmarks; `.github/workflows/benchmark.yml` runs on benchmark changes or manual `workflow_dispatch`); default 30s warmup + 60s sample × 5-pair scripts remain available for dedicated bare-metal sign-off.
  - `WingsAllocator` and `native_memory_ledger` report exact Rust heap requested/usable/fragmentation bytes alongside OS PSS/physical footprint, while OS dynamic-loader (`dyld`/`ld.so`) internal metadata outside the Rust allocator is observed at whole-process granularity.

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
