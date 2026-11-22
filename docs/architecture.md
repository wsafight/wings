# Architecture

**English** | [简体中文](zh/architecture.md) · [Back to Overview](../README.md) · [Language Design](language.md) · [Runtime](runtime.md)

## Overview

Wings uses a three-tier architecture combining a **single `wings-lang` frontend, a Cranelift AOT backend, and a Rust managed microprocess runtime**:

```text
┌──────────────────────────────────────────────────────────────┐
│ wings-lang Source (actor / struct / state / on)              │
│   Static types, immutable values, non-blocking messaging     │
├──────────────────────────────────────────────────────────────┤
│ Compiler & Tooling Pipeline (wings-compile / wings-lsp)      │
│   Fault-tolerant CST/AST -> Semantic & protocol checking     │
│   -> Structured register IR + precise CFG live-root analysis │
│   -> Cranelift 0.118 PIC native code -> Shared lib + manifest│
├──────────────────────────────────────────────────────────────┤
│ Managed Microprocess Runtime (wings-vm)                      │
│   3-level resource ledger, pooled coroutine stacks,          │
│   chunked non-moving precise GC, O(1) slot+gen addressing,   │
│   lock-free fast mailboxes, M:N preemptive scheduler,        │
│   timers, monitors, supervision & stdio/TCP/HTTP serving     │
└──────────────────────────────────────────────────────────────┘
```

> **Early Evolution Policy**: During `0.0.x`, the managed ABI, IR instruction set, artifact schemas, and crate interfaces carry no legacy compatibility shims; protocol changes update the implementation directly and require recompiling artifacts.

## Workspace Crates & Dependency Isolation

The workspace is organized into 10 focused crates along a strict Directed Acyclic Graph (DAG), keeping internal subsystems like GC and scheduling cohesive inside `wings-vm`:

| Crate | Kind | Responsibility | Isolation Constraint |
|---|---|---|---|
| `wings-source` | lib | Source file IDs, UTF-8 byte spans, UTF-16 line/column mapping, diagnostics | Zero heavy external dependencies |
| `wings-syntax` | lib | Lossless lexing, resilient recursive-descent parsing, AST definitions | Depends only on `wings-source` |
| `wings-sema` | lib | Symbol resolution, type inference & checking, actor/struct/message validation | Independent of IR, Cranelift, and LSP wire types |
| `wings-lsp` | bin + lib | `stdio` JSON-RPC language server (diagnostics, hover, goto-def, completion) | **Must never depend on** `wings-ir`, `wings-codegen`, or `wings-vm` |
| `wings-abi` | lib | Managed C ABI layouts (`ScalarContext`, `EffectRequest`, `ShadowFrame`), status codes, type schemas | Pure contract definitions; no compiler or runtime logic |
| `wings-ir` | lib | Validated managed IR, frame/step accounting, backward CFG live-reference root analysis | Depends only on `wings-abi` |
| `wings-artifact` | lib | Bounded serialization & validation for `manifest.json` and multi-actor `bundle.json` | Depends only on `wings-abi` and `serde` |
| `wings-codegen` | lib | IR-to-Cranelift machine code emission, superblock step coalescing, safepoints, native linking | Isolates heavy Cranelift dependencies |
| `wings-compile` | bin + lib | End-to-end compilation driver, HIR lowering (`lower`), `check` / `build` / `build-app` CLI | Compiler toolchain entry point |
| `wings-vm` | bin + lib | Native loader, coroutine execution, precise GC, M:N scheduler, 3-level ledger, `run` / `serve` CLI | **Must never depend on** frontend, IR, `wings-codegen`, or `wings-compile` (including dev-deps) |

CI enforces workspace dependency isolation automatically so `wings-lsp` stays lightweight and `wings-vm` never pulls in compiler crates or stack unwinding across the C ABI (`corosensei` explicitly disables `unwind` and `asm-unwind`).

## Compilation Pipeline, Managed C ABI & Precise GC

1. **Why Not a Rust `#[actor]` Proc-Macro**: A Rust proc-macro cannot inspect or bound arbitrary loops, native stack growth, or hidden interior pointers inside opaque third-party Rust functions. A dedicated frontend lets the compiler place deterministic checks at every function entry, loop backedge, and allocation site.
2. **Managed C ABI (`wings-abi`)**:
   - Generated native handlers receive a fixed-layout `*mut ScalarContext` containing atomic cancellation flags, remaining total steps, remaining time-slice steps, native stack limit pointer, active `ShadowFrame` list head, slow-path callbacks, heap callbacks, and async effect hooks.
   - Arithmetic overflow, division by zero, out-of-bounds slicing, step/stack exhaustion, and cancellation propagate strictly via status codes—**panicking or unwinding across the C ABI is strictly forbidden**.
3. **Precise CFG Root Liveness & Chunked GC**:
   - `wings-ir` constructs a control-flow graph (CFG) isomorphic to machine-code emission and runs backward liveness analysis to compute exact live reference slots at every safepoint and call site, storing them in an explicit `ShadowFrame` chain without conservative stack/register scanning.
   - `wings-vm` runs per-process non-moving mark-sweep GC starting from committed `state`, the `ShadowFrame` chain, entry arguments, and candidate reply roots, polling step budgets and yield flags every 16 visited nodes/roots while pooling backing byte buffers.
4. **Artifact Verification & Loader Boundaries**:
   - Single-actor builds emit `manifest.json` plus a native shared library (`program.dylib` / `program.so`); multi-actor bundles emit `bundle.json` with per-actor subdirectories.
   - Before `dlopen`, `wings-vm` enforces regular-file manifests (≤ 64 KiB), pre-parses ELF64 / Mach-O64 load segments to admit page-aligned image spans (≤ 64 MiB), and verifies that dynamic libraries link only against base OS system libraries (`libSystem` on macOS, `libc` on Linux).

## Platform Scope

First-class support targets **macOS AArch64 (Apple Silicon)** and **Linux x86-64 (GNU)**. Platform-specific primitives (`kqueue` vs. `epoll` + `eventfd` wakeups, `pthread` stack bounds inspection, Mach vs. `/proc` memory accounting, and Mach-O / ELF preflight checks) are encapsulated inside `wings-vm`, keeping upper-layer IR and language semantics platform-neutral.
