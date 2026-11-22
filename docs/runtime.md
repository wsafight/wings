# Managed Microprocess Runtime

**English** | [简体中文](zh/runtime.md) · [Back to Overview](../README.md) · [Architecture](architecture.md) · [Language Design](language.md) · [Evaluation](evaluation.md)

`wings-vm` manages native artifact loading, coroutine stack pooling, per-process precise GC, capability addressing, multi-worker M:N scheduling, timers, monitors, three-level resource admission, and network serving.

## Microprocesses & Capability Addressing

- **Zero-Stack Idle Actors**: An actor borrows a native coroutine stack only while actively executing a handler or suspended across a managed checkpoint; once a handler returns, its stack returns to the worker pool immediately so idle sessions hold zero native stack memory.
- **O(1) `slot + generation` Addressing**: Both `WorkerPid` and `SchedulerPid` encode a table slot index plus a monotonically increasing `generation`. Retiring an actor increments its slot generation so stale `pid` sends, requests, or monitors fail immediately in O(1) without ABA misdelivery or linear scans.
- **Exclusive Ownership & Deep-Copy Messaging**: At most one scheduler worker owns an actor at any instant; cross-actor messages and replies deep-copy values into the destination heap so microprocesses never share mutable heap pointers.

## Coroutine Stack Pool & Chunked Precise GC

- **Bounded Reusable Stack Pool (`StackPool` / `WorkerFibers`)**:
  - Each fiber slot provides a fixed usable native stack (256 KiB) guarded by hardware `PROT_NONE` guard pages, reserved under tenant quotas and net-transferred to the target process account while borrowed.
  - Normal returns, managed faults, and cancellations recycle coroutine frames cleanly on the worker thread without per-message `mmap`/`mprotect` syscalls; excess idle stacks remap to `PROT_NONE` to release physical pages.
- **Direct Fast Path vs. Suspendable Fibers**: Short synchronous handlers without suspension points can execute on a direct fast path, while preemptible or effect-yielding handlers run on pooled fibers—sharing identical step budgets, stack-limit checks, and atomic state commit rules.
- **Chunked Per-Process GC (`aot_heap`)**: Non-moving precise mark-sweep GC polls step budgets, cancellation tokens, and time-slice yield flags every 16 roots or heap nodes, while pooling byte allocations within the process to reduce allocator churn.

## Lock-Free Fast Mailboxes & M:N Scheduler

- **Per-Sender FIFO & Explicit Backpressure**: Every actor owns a bounded mailbox; full queues reject immediately with an explicit `mailbox_full` error rather than silently dropping messages or growing memory without bound.
- **Lock-Free Enqueue, Affinity & Bounded Batching**: `Scheduler` maintains generation-checked lock-free fast mailbox slots, per-worker wake bitmasks (`WorkerWakeHub`), sticky worker affinity with idle-worker stealing, and bounded backlog batching (up to 32 fast messages when no sibling work is queued), while interleaving short direct/reusable claims 1-for-1 with suspended long-running fibers.
- **Preemptive Fairness**:
  - Compiled code decrements `slice_remaining` at branches, loop backedges, and calls while a background timer thread atomically sets `yield_request` on quantum expiry.
  - Compute-heavy or infinite-loop handlers suspend their live stack at the next safepoint and re-queue at the tail of the ready list so sibling actors on the same worker continue making progress.
- **Native OS Event Parking**: Idle workers park on platform event primitives (`kqueue` on macOS, `epoll` + `eventfd` on Linux) until woken by new work or the earliest timer deadline (`.ask` timeouts and `send_after` timers), avoiding busy-spin CPU burn.

## Fault Isolation, Monitors & Bounded Supervision

- **Managed Fault Containment**: Integer overflow, division by zero, slice bounds violations, step exhaustion, heap quota errors, and stack-limit faults trap into `ExecutionFault`, roll back uncommitted state mutations, record a `last_exit` diagnostic snapshot, and leave peer actors unaffected.
- **Language-Level `monitor` & Host `Supervisor`**:
  - Actors subscribe to child exit notifications via `monitor(child_pid, callback)` to implement stateful recovery policies in `wings-lang` (see [`examples/session-service.wings`](../examples/session-service.wings)).
  - Host Rust APIs also provide `ScalarSupervisor` and `SchedulerSupervisor` for `one-for-one` restarts bounded by sliding-window restart intensity limits.

## Three-Level Resource Ledger & Network Serving

- **3-Level, 4-Dimension Ledger (`Resources`) & Global Allocator Accounting (`WingsAllocator`)**: Tracks `bytes`, `processes`, `messages`, and `requests` across a `VM -> Tenant -> Process` hierarchy. Reservations charge atomically prior to allocation, roll back immediately if any ancestor exceeds quota, update only non-shared ancestors on transfer, and release automatically on `Drop`. `WingsAllocator` and `native_memory_ledger` additionally track global Rust heap requested/usable bytes, size-class fragmentation, peak usable bytes, optional hard global heap limits, and kernel process footprint (`ri_phys_footprint` on macOS, `/proc/self/smaps_rollup` `Pss` on Linux).
- **CLI Entry Points**:
  - `wings-vm run --trusted <artifact-dir>`: Single-actor `stdio` JSON Lines runner.
  - `wings-vm serve --trusted <dir> [--workers N] [--tcp ADDR | --http ADDR]`: Serves single-actor artifacts or multi-actor bundles (`bundle.json`) with bounded connection/body limits, automatic JSON-to-`struct`/`str`/`bytes`/`[int]` marshaling, and streaming JSON-RPC responses.
