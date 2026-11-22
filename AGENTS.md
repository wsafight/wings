# Wings VM Collaboration Guidelines / 协作约定

This document applies to the entire repository. / 本文件适用于整个仓库。

---

## English Guidelines

### Early-Stage Policy (No Backward Compatibility)

- Wings is in its **`0.0.x` early evolution stage with zero backward-compatibility guarantees**.
- Language syntax, keywords, built-in functions, IR instructions, the managed C ABI, artifact manifests (`manifest.json` / `bundle.json`), CLI flags, and public Rust APIs may be directly refactored or replaced whenever a cleaner design emerges.
- **No legacy compatibility baggage**: Remove obsolete syntax forms, enum variants, manifest fields, and transitional shims outright rather than maintaining dual-track compatibility layers.

### Current Capabilities & Remaining Boundaries

- **Implemented & Verified**:
  - `wings-lang` frontend and single-file `wings-lsp` (`actor`, `struct`, multi-`state`, scalars/`[int]`/`bytes`/`str`, nullable `pid?<Actor>`, branching, loops, bounded direct/mutual recursion within managed stack frames).
  - Cranelift AOT backend (precise CFG GC root liveness, superblock/call-site safepoints, single-actor `build`, and multi-actor `build-app` bundles).
  - `wings-vm` runtime: O(1) `slot + generation` capability addressing, reusable coroutine stack pool, lock-free fast mailboxes, multi-worker preemptive `Scheduler` (per-worker wake bitmasks, sticky affinity, and interleaved direct fast claims), `send_after` timers, `monitor` lifecycle notifications with bounded supervision restarts, three-level resource admission (`VM -> Tenant -> Process`), global Rust heap accounting (`WingsAllocator`) + OS footprint ledger (`native_memory_ledger`), and `wings-vm serve` (`stdio` / `--tcp` / `--http` JSON-RPC).
  - Dual-platform paired BEAM & CPU-quota benchmark suite (`.github/workflows/benchmark.yml`): passes `hot-queue`, `sparse-256` (10,000 actors), `mixed-4k`, and `fairness` on both Linux x86-64 under `cgroup v2` hard CPU quotas (`20000 / 10000`) and macOS AArch64 (`macos-15`).
- **Remaining Boundaries** (state honestly in all public docs):
  - **Hot-Queue Microsecond Gap & Dedicated Bare-Metal Soaks**: Wings passes the paired `hot-queue` throughput, memory, and sub-`100 µs` service p99 gate on Linux `cgroup v2` (~`16 µs`), though raw single-actor contended service p99 remains ~3x higher than BEAM (~`5 µs`); CI runs bounded 3–5s windows on shared cloud VMs while 30s/60s 5-pair scripts remain for dedicated bare-metal runs.
  - **Dynamic Loader Metadata**: Rust global heap allocations/fragmentation (`WingsAllocator`) and Mach-O/ELF image spans are tracked explicitly, while internal OS dynamic loader (`dyld` / `ld.so`) structures outside the Rust allocator are observed at the whole-process RSS/PSS/footprint level rather than per-tenant sub-accounts.
  - **Post-MVP Language Features**: Multi-file module imports and declarative supervision trees remain scheduled for upcoming milestones.

### Core Architecture & Implementation Rules

- **Workspace Dependency Isolation**: Strictly preserve the acyclic crate graph (`docs/architecture.md`). `wings-lsp` must never depend on IR, Cranelift, or VM; `wings-vm` (including dev-dependencies) must never depend on frontend, sema, IR, Cranelift, or `wings-compile`.
- **Execution & State Isolation**: All business values are immutable and actors share no mutable business memory. `send` / `ask` / `spawn` / `send_after` / `monitor` side effects take effect at the call site and are not rolled back on handler failure; state updates and auto-replies commit only upon normal return.
- **Preemption & Safety Boundary**: Preemption relies on compiler-inserted safepoints and atomic flag polling; long runtime operations (GC, large `bytes`/`str` operations) must check budget and cancellation in bounded chunks. Rust panics/unwinds must never cross the managed C ABI (`corosensei` disables `unwind`).
- **Precise GC & Three-Level Admission**: Per-process precise non-moving mark-sweep GC tracks live roots via an explicit shadow stack. Growable allocations reserve `Process -> Tenant -> VM` quotas upfront, roll back atomically on failure, and refund completely upon termination.

### Documentation & Verification Workflow

- **Bilingual Sync**: Keep root `README.md` + `docs/*.md` (English default), `README.zh-CN.md` + `docs/zh/*.md` (Simplified Chinese), and the `site/` website synchronized. Keep prose concrete, restrained, and free of marketing hype.
- **Minimal File Set**: Maintain only `README.md`, `README.zh-CN.md`, `AGENTS.md`, and the 7 core topic docs (`product.md`, `architecture.md`, `language.md`, `runtime.md`, `evaluation.md`, `roadmap.md`, `publishing.md`) in `docs/` and `docs/zh/`. Do not create ad-hoc verification logs, draft proposals, or fragmented ADR files.
- **Daily Checks**: Run `python3 scripts/verify_incremental.py` for working-tree changes, adding `--base main` for committed branch changes. Inspect with `--plan`; `--all` expands incremental scope only. Daily PR/`main` CI uses `incremental`; do not run full release CI or paired benchmarks for every edit/commit. Failed/interrupted checks must never become cached success.
- **Pre-Release Checks** (manual `.github/workflows/release.yml`: dual native, MSRV `1.85.0`, and site jobs; branch protection must separately require `incremental`):
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo +1.85.0 check --workspace --all-targets --locked`
  - `cargo test --workspace --tests --locked && cargo test --workspace --doc --locked`
  - Run `python3 scripts/verify_scalar_aot.py` before release; daily ABI/codegen/scheduler/CLI changes use the scheduler's bounded AOT smoke. Build `site/` for documentation/website changes through the incremental scheduler.

---

## 中文约定

### 早期阶段原则（无需向后兼容）

- 项目当前处于 **`0.0.x` 早期演进阶段，不承诺任何向后兼容**。
- 语言语法、关键字、内置函数、IR 指令、受管 C ABI、产物清单（`manifest.json` / `bundle.json`）、CLI 参数与 Rust 库公开接口均可直接按更优方案重构或替换。
- **禁止保留历史兼容包袱**：演进设计时直接删除旧写法、旧枚举变体、旧字段或过渡适配层，不维护双轨兼容路径，优先保证代码精简、边界清晰与实现健壮。

### 当前进度与保留边界

- **已跑通并纳入自动化门禁**：
  - `wings-lang` 前端与单文件 LSP（`actor`、`struct`、多 `state`、标量/`[int]`/`bytes`/`str`、可空 `pid?<Actor>`、分支、循环、受管栈帧内的有界直接/互递归）。
  - Cranelift AOT 编译（精确 CFG GC 根、超块与调用点检查、单 Actor 产物 `build` 与多 Actor 应用包 `build-app`）。
  - `wings-vm` 运行时：O(1) `slot + generation` 能力寻址、可复用协程栈池、无锁快速邮箱、多 worker 抢占式 `Scheduler`（按 worker 定向唤醒位图、亲和窃取路由、短消息快路径与挂起任务 1:1 切片交错）、`send_after` 定时器、`monitor` 生命周期通知与有界监督重启、三级资源账户（`VM -> Tenant -> Process`）、全局 Rust 堆分配与碎片统计（`WingsAllocator`）及 OS 物理内存对账（`native_memory_ledger`），以及 `wings-vm serve`（`stdio` / `--tcp` / `--http` JSON-RPC）。
  - 双平台配对基准与硬配额门禁（`.github/workflows/benchmark.yml`）：Linux x86-64（`cgroup v2` 硬 2-vCPU 配额 `20000 / 10000`）与 macOS AArch64（`macos-15`）均通过 `hot-queue`、`sparse-256`（10,000 Actor）、`mixed-4k` 与 `fairness` 四组 BEAM 对照门禁。
- **保留边界说明**（对外说明时须如实保留边界）：
  - **Hot-queue 微秒级绝对延迟与裸金属长稳**：Linux `cgroup v2` 下 `hot-queue` 已通过吞吐、内存与 `< 100 µs` 服务 p99 门禁（约 `16 µs`），但单收件人极限争用下的微秒级服务 p99 绝对值仍高于 BEAM（约 `5 µs`）；CI 采用有界 3–5s 窗口，独占物理机长稳验收使用默认 30s/60s 五对脚本。
  - **系统动态装载器元数据**：Rust 全局堆分配/碎片（`WingsAllocator`）与模块映像跨度已显式追踪，Rust 分配器之外的系统动态装载器（`dyld`/`ld.so`）内部元数据按整进程 RSS/PSS/footprint 口径对账，未做租户级拆分。
  - **后续语言特性**：跨文件模块导入与语言级声明式监督树位于后续里程碑。

### 核心架构与实现约束

- **Workspace 依赖隔离**：严格维持无环依赖（见 `docs/architecture.md`）。`wings-lsp` 不得依赖 IR、Cranelift 或 VM；`wings-vm`（含测试依赖）不得依赖前端、语义分析、IR、Cranelift 或 `wings-compile`。
- **执行与状态隔离**：业务值不可变，微进程之间不共享可变业务数据。`send` / `ask` / `spawn` / `send_after` / `monitor` 副作用在调用点生效，handler 失败不回滚已发出副作用；状态更新与自动回复仅在正常返回后提交。
- **抢占与安全边界**：抢占依赖编译器强制 safepoint 与原子标志轮询，runtime 长操作（如 GC、大字节/字符串处理）必须分块检查预算与取消。严禁 Rust panic/unwind 跨越受管 C ABI（`corosensei` 禁用 unwind 特性）。
- **精确 GC 与三级准入**：进程内精确、非移动 mark-sweep GC，活跃引用通过显式 shadow stack 维护。可增长资源在分配前经过 `Process -> Tenant -> VM` 三级准入，失败原子回滚，退出完整退账。
- **代码组织**：保持单文件职责聚焦，避免巨型源文件堆积；跨模块共享校验与错误类型（如 `VmError`），减少字符串拼接与重复样板代码。

### 写作、验证与发布流程

- 日常仅运行 `python3 scripts/verify_incremental.py`，包含分支已提交改动时加 `--base main`，`--plan` 查看计划；`--all` 也只是全部增量检查。PR/`main` CI 使用 `incremental`；完整双平台、MSRV、AOT 检查仅发布前手动触发 `release.yml`，BEAM 基准另行手动运行。分支保护需单独改为要求 `incremental`；失败或中断不得缓存为成功。
- 仓库文档与 `site/` 官网保持中英双语同步：根目录默认 `README.md` 与 `docs/*.md` 为英文，`README.zh-CN.md` 与 `docs/zh/*.md` 为对应简体中文；表达具体、克制，不写空洞宣传语。
- 保持文档精简：只维护根目录 `README.md`、`README.zh-CN.md`、`AGENTS.md` 与 `docs/`（含 `docs/zh/`）下的 7 篇核心主题文档（`product.md`、`architecture.md`、`language.md`、`runtime.md`、`evaluation.md`、`roadmap.md`、`publishing.md`）。不为每次日常提交新建独立的验证日志、提案草稿或碎片化 ADR 文件；设计变更直接同步更新中英双语主题文档。
- 崩溃、非法访存与分配失败注入只能在带 CPU/墙钟/core 限制的隔离测试子进程内执行，不在宿主机直接制造无界资源耗尽。
- 包发布遵循 `Apache-2.0` 许可证与 `docs/publishing.md` 依赖顺序，仅在维护者明确要求时手动执行。
