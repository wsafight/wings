# Product Positioning

**English** | [简体中文](zh/product.md) · [Back to Overview](../README.md) · [Architecture](architecture.md) · [Risks & Roadmap](roadmap.md)

## One-Sentence Summary

Wings is a microprocess virtual machine built for stateful, high-concurrency services, aiming to **run faster and consume less memory than BEAM under identical workloads** while preserving message isolation, fair preemptive scheduling, hierarchical resource quotas, and fault recovery.

The project is currently in its `0.0.x` early evolution phase, prioritizing execution-model soundness, runtime efficiency, and code simplicity **without promising backward compatibility for syntax, ABI, artifact manifests, or host APIs**.

## Baseline Comparison

High-density stateful session workloads require both lightweight per-session fault isolation and tight control over scheduling tail latency and memory slope:

| Approach | Execution Model | Fault & Resource Boundary | Primary Trade-off |
|---|---|---|---|
| Native Rust Actors (Tokio / kameo) | Native machine code | Partial panic catching; no automatic CPU preemption or per-actor memory quotas | High raw throughput ceiling, but infinite loops, memory fairness, and state isolation rely entirely on developer discipline |
| WASM Actors (Wasmtime) | JIT / AOT sandbox | Strong linear-memory & trap isolation for untrusted code | Per-instance linear memory, tables, and host-bridge overhead are heavy for millions of tiny stateful sessions |
| BEAM (Erlang / Elixir) | JIT bytecode VM | Battle-tested process heap isolation, preemptive scheduling, and OTP supervision | Dynamic tagging, message copying, and dispatch overhead leave room for improvement in compute-mixed and dense session workloads |
| **Wings VM** | Managed `wings-lang` AOT | Immutable domain language + compiler-enforced safepoints + 3-level resource admission | Trades arbitrary native Rust library reuse and OTP ecosystem breadth for static layouts, low memory density, and native AOT throughput |

BEAM serves as the primary throughput and memory baseline; native Tokio implementations establish hardware lower bounds, while Wasmtime contrasts an alternative managed runtime boundary.

## Target Workloads & Non-Goals

Wings targets stateful backend services built by trusted engineering teams willing to adopt a focused domain language:

1. **IM / IoT Session Gateways & Routing**: Hundreds of thousands of small-state, low-duty-cycle persistent sessions managed as isolated state-machine actors.
2. **AI Agent Session & Workflow Orchestration**: Long-lived conversational state, asynchronous sub-task fan-out (`spawn` / `ask`), periodic heartbeats (`send_after`), and crash recovery (`monitor`).
3. **Soft-Realtime Rooms & Coordination Shards**: Game rooms, collaborative state shards, or matching coordinators requiring single-actor fault containment without stalling peer actors on shared worker threads.

Explicit non-goals:
- Hard real-time control systems (microsecond deterministic deadlines are not guaranteed).
- Hostile multi-tenant security sandboxing (`--trusted` loads locally trusted build artifacts only; untrusted extensions belong in separate OS subprocesses).
- Running arbitrary third-party Rust crates directly on managed coroutine stacks, or replicating the full Erlang/OTP standard library and distributed clustering protocol.

## Success Criteria

- Under identical workloads on the same macOS AArch64 and Linux x86-64 hosts, achieve **both higher sustained throughput and lower whole-machine memory usage** than BEAM while meeting the same p99 / p999 tail-latency and zero-starvation constraints.
- Report idle, mixed-active, and hot-queue workloads separately without cherry-picking partial metrics across incompatible configurations or disabling mandatory safepoints and resource admission.
