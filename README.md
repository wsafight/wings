# Wings VM

**English** | [简体中文](README.zh-CN.md) · [Website & Docs](https://wsafight.github.io/wings/) · [crates.io](https://crates.io/crates/wings-vm)

A managed AOT microprocess virtual machine for high-concurrency stateful applications.

Wings implements its runtime in Rust and compiles `wings-lang` business logic ahead-of-time (AOT) into managed native machine code via Cranelift. Its goal is to **deliver higher sustained throughput and lower memory footprint than BEAM under identical stateful workloads**, while preserving message isolation, preemptive fairness, hierarchical resource quotas, and fault recovery.

- **Throughput Goal**: Achieve higher sustained throughput under identical CPU quotas and tail-latency (p99 / p999) constraints.
- **Memory Goal**: Lower total machine memory footprint and raise single-node process density under identical session state, mailbox depth, and active ratios.
- **Primary Platforms**: **macOS AArch64 (Apple Silicon)** and **Linux x86-64**.

> [!IMPORTANT]
> **Early-Stage Notice (`0.0.x`)**: Wings is under rapid early iteration with **no backward-compatibility guarantees**. Language syntax, the managed C ABI, artifact manifests (`manifest.json` / `bundle.json`), CLI flags, and Rust APIs may change with breaking updates—always recompile `.wings` sources after upgrading.

## Current Capabilities

- **Language & Frontend (`wings-lang`)**: Supports `actor` and top-level/local immutable `struct` types, multiple `state` bindings, `int` / `bool` / `unit` / `[int]` / `bytes` / `str`, typed `pid<Actor>` and nullable `pid?<Actor>`, `if / else` branching, `while` loops, bounded direct and mutual recursion within managed stack frames, and a lightweight `wings-lsp` language server.
- **Actor Messaging & Fault Tolerance**: Supports `spawn(Actor)`, one-way messaging (`!` / `.tell`), non-blocking requests with timeout callbacks (`.ask(..., timeout: ..., then: ...)`), delayed self-timers (`send_after`), and lifecycle exit notifications (`monitor`).
- **Compiler & Runtime**: Precise CFG-based GC root liveness analysis, per-process chunked non-moving mark-sweep GC, O(1) `slot + generation` capability addressing, pooled reusable coroutine stacks, lock-free fast mailboxes, multi-worker preemptive `Scheduler`, three-level (`VM -> Tenant -> Process`) resource admission, and global Rust allocator + OS process memory accounting (`WingsAllocator` / `native_memory_ledger`).
- **Bundling & Network Serving**: Multi-actor bundling via `wings-compile build-app`, and network/IPC serving via `wings-vm serve` over `stdio`, `--tcp` (JSON Lines), and `--http` (JSON-RPC + `GET /healthz`).
- **Paired BEAM Benchmarks & Known Boundaries**: Automated dual-platform paired benchmarks (`.github/workflows/benchmark.yml`) pass `hot-queue`, `sparse-256` (10k actors), `mixed-4k`, and `fairness` against Erlang/OTP on Linux x86-64 (`cgroup v2` hard 2-vCPU quota) and macOS AArch64. Single-actor `hot-queue` Linux service p99 (~`16 µs`) remains ~3× higher than BEAM (~`5 µs`), OS dynamic-loader (`dyld`/`ld.so`) metadata outside the Rust allocator is observed at whole-process RSS/PSS/footprint granularity, and multi-file module imports plus declarative supervision trees remain on the post-MVP roadmap (see [Evaluation & Acceptance](docs/evaluation.md) and [Risks & Roadmap](docs/roadmap.md)).

## Quick Start

Prerequisites: Rust `1.85+` and a system C linker (`cc`, overridable via `CC`). Install the CLI toolchain directly from crates.io:

```sh
cargo install wings-compile wings-vm wings-lsp --version 0.0.3 --locked
```

### 1. Static Check & Single-Actor Execution

```sh
# Static syntax, type, and protocol check
cargo run -p wings-compile -- check examples/counter.wings

# Build a single-actor native AOT artifact (output directory must not exist)
cargo run -p wings-compile -- build examples/counter.wings -o target/counter-aot

# Execute a JSON Lines message stream (--trusted is for trusted local builds only)
cargo run -p wings-vm -- run --trusted target/counter-aot < examples/counter.messages.jsonl
```

### 2. Multi-Actor Application Bundle & Serving

```sh
# Compile a multi-actor bundle (includes supervision restart and heartbeat timers)
cargo run -p wings-compile -- build-app examples/session-service.wings -o target/session-bundle

# Start a multi-worker HTTP server (POST /<handler> and GET /healthz)
cargo run -p wings-vm -- serve --trusted target/session-bundle --workers 2 --http 127.0.0.1:8080

# Or start a TCP JSON Lines server
cargo run -p wings-vm -- serve --trusted target/session-bundle --workers 2 --tcp 127.0.0.1:9000
```

### 3. Development & Verification Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Build binaries & examples and run the full end-to-end AOT verification suite (requires Python 3)
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_scalar_aot.py
```

## Documentation

中文读者请参阅 **[简体中文 README](README.zh-CN.md)** 及 `docs/zh/` 目录下的完整中文文档。

| Document | 中文文档 | Summary |
|---|---|---|
| [Product Positioning](docs/product.md) | [产品定位](docs/zh/product.md) | Performance & memory targets, target workloads, and trade-offs vs. BEAM / Tokio / Wasmtime |
| [Architecture](docs/architecture.md) | [架构设计](docs/zh/architecture.md) | 10-crate workspace isolation, Cranelift AOT pipeline, managed C ABI, and precise GC |
| [Language Design](docs/language.md) | [语言设计](docs/zh/language.md) | `wings-lang` syntax, immutable value types, async messaging, timers, and monitors |
| [Runtime](docs/runtime.md) | [运行时](docs/zh/runtime.md) | Microprocess layout, stack pooling, lock-free mailboxes, M:N preemption, and 3-level quotas |
| [Evaluation & Acceptance](docs/evaluation.md) | [评测与验收](docs/zh/evaluation.md) | Paired BEAM benchmarks, current macOS AArch64 measurements, and optimization discipline |
| [Risks & Roadmap](docs/roadmap.md) | [风险与里程碑](docs/zh/roadmap.md) | `0.0.x` evolution policy, technical slices 1–7, known risks, and M1–M5 milestones |
| [Crate Publishing](docs/publishing.md) | [包发布](docs/zh/publishing.md) | `Apache-2.0` license, topological crates.io release order, and release history |

Before contributing code or documentation, please review [AGENTS.md](AGENTS.md).
