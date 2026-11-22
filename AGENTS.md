# Wings VM Collaboration Guidelines / 协作约定

This document applies to the entire repository. / 本文件适用于整个仓库。

---

## English Guidelines

### Early-Stage Policy (No Backward Compatibility)

- Wings is in its **`0.0.x` early evolution stage with zero backward-compatibility guarantees**.
- Language syntax, keywords, built-in functions, IR instructions, the managed C ABI, artifact manifests (`manifest.json` / `bundle.json`), CLI flags, and public Rust APIs may be directly refactored or replaced whenever a cleaner design emerges.
- **No legacy compatibility baggage**: Remove obsolete syntax forms, enum variants, manifest fields, and transitional shims outright rather than maintaining dual-track compatibility layers.

### Current Capabilities vs. Open MVP Gates

- **Implemented & Verified**:
  - `wings-lang` frontend and single-file `wings-lsp` (`actor`, `struct`, multi-`state`, scalars/`[int]`/`bytes`/`str`, nullable `pid?<Actor>`, branching, loops, bounded direct/mutual recursion within managed stack frames).
  - Cranelift AOT backend (precise CFG GC root liveness, superblock/call-site safepoints, single-actor `build`, and multi-actor `build-app` bundles).
  - `wings-vm` runtime: O(1) `slot + generation` capability addressing, reusable coroutine stack pool, lock-free fast mailboxes, multi-worker preemptive `Scheduler`, `send_after` timers, `monitor` lifecycle notifications with bounded supervision restarts, three-level resource admission (`VM -> Tenant -> Process`), and `wings-vm serve` (`stdio` / `--tcp` / `--http` JSON-RPC).
- **Open Formal MVP Gates** (state boundaries honestly in all public docs):
  - **Paired BEAM Dual Gate**: All five fairness paired runs pass, but hot-queue open-loop service p99 remains higher than BEAM, and simultaneous sustained-throughput + whole-machine-memory sign-off under Linux cgroup v2 hard CPU quotas is still pending.
  - **Complete Native Resource Ledger**: Heap, stack pool, mailboxes, request tables, and module image spans are admitted across three tiers, while `dlopen` loader metadata and global allocator fragmentation are not yet fully closed.
  - **Dual-Platform Paired Benchmarks**: Both macOS AArch64 and Linux x86-64 must pass full CI and same-host BEAM comparisons on current `main`.

### Core Architecture & Implementation Rules

- **Workspace Dependency Isolation**: Strictly preserve the acyclic crate graph (`docs/architecture.md`). `wings-lsp` must never depend on IR, Cranelift, or VM; `wings-vm` (including dev-dependencies) must never depend on frontend, sema, IR, Cranelift, or `wings-compile`.
- **Execution & State Isolation**: All business values are immutable and actors share no mutable business memory. `send` / `ask` / `spawn` / `send_after` / `monitor` side effects take effect at the call site and are not rolled back on handler failure; state updates and auto-replies commit only upon normal return.
- **Preemption & Safety Boundary**: Preemption relies on compiler-inserted safepoints and atomic flag polling; long runtime operations (GC, large `bytes`/`str` operations) must check budget and cancellation in bounded chunks. Rust panics/unwinds must never cross the managed C ABI (`corosensei` disables `unwind`).
- **Precise GC & Three-Level Admission**: Per-process precise non-moving mark-sweep GC tracks live roots via an explicit shadow stack. Growable allocations reserve `Process -> Tenant -> VM` quotas upfront, roll back atomically on failure, and refund completely upon termination.

### Documentation & Verification Workflow

- **Bilingual Sync**: Keep root `README.md` + `docs/*.md` (English default), `README.zh-CN.md` + `docs/zh/*.md` (Simplified Chinese), and the `site/` website synchronized. Keep prose concrete, restrained, and free of marketing hype.
- **Minimal File Set**: Maintain only `README.md`, `README.zh-CN.md`, `AGENTS.md`, and the 7 core topic docs (`product.md`, `architecture.md`, `language.md`, `runtime.md`, `evaluation.md`, `roadmap.md`, `publishing.md`) in `docs/` and `docs/zh/`. Do not create ad-hoc verification logs, draft proposals, or fragmented ADR files.
- **Pre-PR Checks** (`main` is protected and requires `native (macos-15, arm64)`, `native (ubuntu-24.04, x86_64)`, `msrv` `1.85.0`, and `site` CI jobs):
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo +1.85.0 check --workspace --all-targets --locked`
  - `cargo test --workspace --tests --locked && cargo test --workspace --doc --locked`
  - Run `python3 scripts/verify_scalar_aot.py` when touching ABI, codegen, scheduler, or CLI protocols, and `npm run build` in `site/` when touching docs or website code.

---

## 中文约定

### 早期阶段原则（无需向后兼容）

- 项目当前处于 **`0.0.x` 早期演进阶段，不承诺任何向后兼容**。
- 语言语法、关键字、内置函数、IR 指令、受管 C ABI、产物清单（`manifest.json` / `bundle.json`）、CLI 参数与 Rust 库公开接口均可直接按更优方案重构或替换。
- **禁止保留历史兼容包袱**：演进设计时直接删除旧写法、旧枚举变体、旧字段或过渡适配层，不维护双轨兼容路径，优先保证代码精简、边界清晰与实现健壮。

### 当前进度与正式门禁差距

- **已跑通能力**：
  - `wings-lang` 前端与单文件 LSP（`actor`、`struct`、多 `state`、标量/`[int]`/`bytes`/`str`、可空 `pid?<Actor>`、分支、循环、受管栈帧内的有界直接/互递归）。
  - Cranelift AOT 编译（精确 CFG GC 根、超块与调用点检查、单 Actor 产物 `build` 与多 Actor 应用包 `build-app`）。
  - `wings-vm` 运行时：O(1) `slot + generation` 能力寻址、可复用协程栈池、无锁快速邮箱、多 worker 抢占式 `Scheduler`、`send_after` 定时器、`monitor` 生命周期通知与有界监督重启、三级资源账户（`VM -> Tenant -> Process`），以及 `wings-vm serve`（`stdio` / `--tcp` / `--http` JSON-RPC）。
- **尚未关闭的正式 MVP 门禁**（对外说明时须如实保留边界）：
  - **同负载 BEAM 双目标门禁**：fairness 五对长测已通过，但 hot-queue 开环服务延迟 p99 仍高于 BEAM，且尚未完成 Linux cgroup v2 硬 CPU 配额下的持续吞吐与整机内存双目标同时验收。
  - **完整原生资源账本**：堆、栈池、邮箱、请求表与模块映像跨度已纳入三级准入，但动态装载器、系统库内部暂存与全局 allocator 开销尚未完全闭环。
  - **双平台原生实测**：macOS AArch64 与 Linux x86-64 均需通过最新源码的完整 CI 与同机 BEAM 对照。

### 核心架构与实现约束

- **Workspace 依赖隔离**：严格维持无环依赖（见 `docs/architecture.md`）。`wings-lsp` 不得依赖 IR、Cranelift 或 VM；`wings-vm`（含测试依赖）不得依赖前端、语义分析、IR、Cranelift 或 `wings-compile`。
- **执行与状态隔离**：业务值不可变，微进程之间不共享可变业务数据。`send` / `ask` / `spawn` / `send_after` / `monitor` 副作用在调用点生效，handler 失败不回滚已发出副作用；状态更新与自动回复仅在正常返回后提交。
- **抢占与安全边界**：抢占依赖编译器强制 safepoint 与原子标志轮询，runtime 长操作（如 GC、大字节/字符串处理）必须分块检查预算与取消。严禁 Rust panic/unwind 跨越受管 C ABI（`corosensei` 禁用 unwind 特性）。
- **精确 GC 与三级准入**：进程内精确、非移动 mark-sweep GC，活跃引用通过显式 shadow stack 维护。可增长资源在分配前经过 `Process -> Tenant -> VM` 三级准入，失败原子回滚，退出完整退账。
- **代码组织**：保持单文件职责聚焦，避免巨型源文件堆积；跨模块共享校验与错误类型（如 `VmError`），减少字符串拼接与重复样板代码。

### 写作、验证与发布流程

- 仓库文档与 `site/` 官网保持中英双语同步：根目录默认 `README.md` 与 `docs/*.md` 为英文，`README.zh-CN.md` 与 `docs/zh/*.md` 为对应简体中文；表达具体、克制，不写空洞宣传语。
- 保持文档精简：只维护根目录 `README.md`、`README.zh-CN.md`、`AGENTS.md` 与 `docs/`（含 `docs/zh/`）下的 7 篇核心主题文档（`product.md`、`architecture.md`、`language.md`、`runtime.md`、`evaluation.md`、`roadmap.md`、`publishing.md`）。不为每次日常提交新建独立的验证日志、提案草稿或碎片化 ADR 文件；设计变更直接同步更新中英双语主题文档。
- 崩溃、非法访存与分配失败注入只能在带 CPU/墙钟/core 限制的隔离测试子进程内执行，不在宿主机直接制造无界资源耗尽。
- 包发布遵循 `Apache-2.0` 许可证与 `docs/publishing.md` 依赖顺序，仅在维护者明确要求时手动执行。
