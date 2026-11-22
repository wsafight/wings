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
| 4 | Rich Values, GC & 3-Level Ledger | `str`/`bytes`/`[int]`/named `struct`, nullable `pid?<Actor>`, chunked precise GC, 3-level reservation + `WingsAllocator` & OS footprint ledger | Completed (`dlopen` loader metadata observed at OS process level) |
| 5 | Fair Preemption & Network Serving | M:N preemptive scheduler, `send_after` timers, `wings-vm serve` (`stdio`/`--tcp`/`--http`) | Completed |
| 6 | Fault Isolation, Monitors & Supervision | Uncommitted state rollback on fault, `monitor` down notifications, bounded host restart intensity, exit snapshots | Completed |
| 7 | Dual-Platform Workload & Performance Gate | Beat/match BEAM simultaneously on sustained throughput and whole-machine memory under hard CPU quotas on macOS ARM64 & Linux x86-64 | **Completed in Paired CI** (30s/60s bare-metal soak & hot-queue sub-10µs tuning ongoing) |

Deferred beyond MVP: hot code swapping, transparent hibernation compaction, multi-node distributed clustering, untrusted native plugin sandboxes, and full IDE refactoring suites.

## Key Risks & Mitigations

1. **Hot-Queue Microsecond Gap vs. BEAM**: Per-worker wake bitmasks, sticky affinity, and lock-free batching brought Linux `cgroup v2` `hot-queue` service p99 down to `~16 µs` (passing the sub-`100 µs` gate), though BEAM's single-process reduction loop remains lower (`~5 µs`). Further narrowing stays guided by `--observe-locks` attribution.
2. **Dynamic Loader Metadata Outside Global Allocator**: Managed heaps, stack pools, mailboxes, Mach-O/ELF image spans, and global Rust heap allocations/fragmentation (`WingsAllocator`) are tracked explicitly, while OS dynamic-loader (`dyld`/`ld.so`) internal metadata is audited alongside OS process-tree RSS / physical footprint / PSS (`native_memory_ledger`).
3. **Domain Language Adoption Cost**: `wings-lang` does not transparently embed arbitrary Rust crates. We mitigate adoption friction with expressive v2 syntax, zero-compiler-bloat `wings-lsp` tooling, and turnkey `--http` / `--tcp` JSON-RPC serving.

## Post-MVP Milestones (M1–M5)

- **M1–M2**: Add multi-file module imports and language-level declarative supervision specs; run extended 30s/60s bare-metal soak campaigns and continue narrowing single-actor hot-queue microsecond latency.
- **M3**: Evaluate adaptive cold/warm actor hibernation compaction and lower-overhead stack-root tables on million-session sparse workloads.
- **M4**: Pilot production observability metrics and diagnostic tooling on real AI agent orchestration or session-gateway workloads.
- **M5+**: Independently evaluate idle-boundary static code reload, bounded external I/O pools, Linux AArch64 support, and isolated out-of-process extension pools.
