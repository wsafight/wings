# Risks & Roadmap

**English** | [简体中文](zh/roadmap.md) · [Back to Overview](../README.md) · [Product Positioning](product.md) · [Architecture](architecture.md) · [Evaluation](evaluation.md)

## Evolution Policy

Wings is in its `0.0.x` early stage with **zero legacy compatibility baggage**: language syntax, built-ins, IR instructions, the managed C ABI, artifact manifests, and Rust APIs may undergo direct breaking changes to converge rapidly on architectural clarity and performance targets.

## MVP Technical Slices (1–7)

Engineering progresses sequentially across seven slices; slices 1–6 form a feature-complete developer preview, while slice 7 closes the formal dual-platform performance and memory MVP gate:

| # | Technical Slice | Core Acceptance Criteria | Status |
|---|---|---|---|
| 1 | Same-Thread Live-Stack Yield & Resume | Suspend/resume at mandatory checkpoints preserving native stack & live values; cancel/drop without cross-ABI unwinding | Completed |
| 2 | Managed Execution & Language Core | Precise CFG shadow stack, native stack-limit checks, branches/loops/bounded recursion, dual-platform wakeups | Completed |
| 3 | Multi-Actor & Bounded Mailboxes | `spawn`, one-way casts, async `ask` replies & timeouts, per-sender FIFO, explicit `mailbox_full` backpressure | Completed |
| 4 | Rich Values, GC & 3-Level Ledger | `str`/`bytes`/`[int]`/named `struct`, nullable `pid?<Actor>`, chunked precise GC, 3-level reservation & rollback | Core complete (loader/allocator native overhead gap remaining) |
| 5 | Fair Preemption & Network Serving | M:N preemptive scheduler, `send_after` timers, `wings-vm serve` (`stdio`/`--tcp`/`--http`) | Completed |
| 6 | Fault Isolation, Monitors & Supervision | Uncommitted state rollback on fault, `monitor` down notifications, bounded host restart intensity, exit snapshots | Completed |
| 7 | Dual-Platform Workload & Performance Gate | Beat BEAM simultaneously on sustained throughput and whole-machine memory under hard CPU quotas on macOS ARM64 & Linux x86-64 | **In Progress** (hot-queue service p99 tuning & native Linux cgroup v2 runs pending) |

Deferred beyond MVP: hot code swapping, transparent hibernation compaction, multi-node distributed clustering, untrusted native plugin sandboxes, and full IDE refactoring suites.

## Key Risks & Mitigations

1. **Hot-Queue Tail Latency vs. BEAM**: Multi-worker wakeups, cross-thread notifications, and request-table synchronization amplify service p99 on heavily contended single actors. We continue profiling lock intervals and lock-free batching paths without claiming gate completion prematurely.
2. **Native Memory Ledger Blind Spots**: While managed heaps, stack pools, mailboxes, and Mach-O/ELF image spans are admitted into the 3-level ledger, `libc` / `dlopen` / global allocator fragmentation can still widen resident OS memory. Benchmarks report both 3-level ledger totals and OS process-tree RSS / physical footprint / PSS.
3. **Domain Language Adoption Cost**: `wings-lang` does not transparently embed arbitrary Rust crates. We mitigate adoption friction with expressive v2 syntax, zero-compiler-bloat `wings-lsp` tooling, and turnkey `--http` / `--tcp` JSON-RPC serving.

## Post-MVP Milestones (M1–M5)

- **M1–M2**: Close Linux x86-64 cgroup v2 quota benchmarks and hot-queue service p99 optimization to pass the formal BEAM throughput + memory gate; add multi-file module imports and declarative supervision specs.
- **M3**: Evaluate adaptive cold/warm actor hibernation compaction and lower-overhead stack-root tables on million-session sparse workloads.
- **M4**: Pilot production observability metrics and diagnostic tooling on real AI agent orchestration or session-gateway workloads.
- **M5+**: Independently evaluate idle-boundary static code reload, bounded external I/O pools, Linux AArch64 support, and isolated out-of-process extension pools.
