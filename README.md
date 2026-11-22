# Wings VM

面向高并发有状态应用的微进程虚拟机。

Wings 用 Rust 实现运行时，通过 Cranelift 将 `wings-lang` 业务代码 AOT 编译为受管原生机器码，目标是**在相同业务负载下，比 BEAM 更快、更省内存**，同时保留消息隔离、抢占调度、资源配额与故障恢复能力。

- **性能目标**：在相同 CPU 配额和尾延迟要求下，取得更高的持续吞吐。
- **内存目标**：在相同业务状态、邮箱深度和活跃比例下，降低整机内存占用，提高单机进程密度。
- **首要平台**：**macOS AArch64（Apple Silicon）** 与 **Linux x86-64**。

> [!IMPORTANT]
> **早期开发声明（`0.0.x`）**：项目处于早期快速迭代阶段，**当前不考虑任何向后兼容**。语言语法、受管 ABI、编译产物清单（`manifest.json` / `bundle.json`）、CLI 选项与 Rust 内部/公开接口均可能直接发生破坏性演进，升级版本后请直接重新编译源码。

## 当前能力概览

- **语言与前端**：支持 `actor` 与顶层/局部不可变 `struct`、多 `state` 绑定、`int` / `bool` / `unit` / `[int]` / `bytes` / `str`、类型化 `pid<Actor>` 与可空 `pid?<Actor>`、分支与 `while` 循环、受管栈帧内的有界函数递归（含互递归），以及配套 `wings-lsp` 语言服务器。
- **Actor 交互与容错**：支持 `spawn(Actor)`、单向消息（`!` / `.tell`）、带超时回调的异步请求（`.ask(..., timeout: ..., then: ...)`）、延时自定时消息（`send_after`）以及进程监控通知（`monitor`）。
- **编译与运行时**：支持精确 CFG GC 根计算、进程内分块非移动 mark-sweep GC、O(1) `slot + generation` 能力寻址、可复用协程栈池、无锁快速邮箱、多 worker 抢占式 `Scheduler` 与 `VM -> Tenant -> Process` 三级资源准入。
- **应用打包与网络接入**：支持 `wings-compile build-app` 打包多 Actor 应用，并通过 `wings-vm serve` 提供 `stdio`、`--tcp`（JSON Lines）与 `--http`（JSON-RPC + `/healthz`）服务入口。
- **当前未闭环项**：跨文件模块、语言级声明式监督树尚未实现；动态装载器/系统分配器内部开销尚未完全纳入三级账本；同负载 BEAM 的 hot-queue 服务 p99 与 cgroup CPU 硬配额双门禁仍在推进中（详见[评测与验收](docs/evaluation.md)与[风险与里程碑](docs/roadmap.md)）。

## 快速开始

环境要求：Rust `1.85+`，以及系统 C 链接器（`cc`，可通过环境变量 `CC` 指定）。

### 1. 检查与单 Actor 运行

```sh
# 静态语法与类型检查
cargo run -p wings-compile -- check examples/counter.wings

# 编译单 Actor AOT 产物（输出目录须不存在）
cargo run -p wings-compile -- build examples/counter.wings -o target/counter-aot

# 通过 JSON Lines 执行消息序列（--trusted 仅用于运行可信本地构建产物）
cargo run -p wings-vm -- run --trusted target/counter-aot < examples/counter.messages.jsonl
```

### 2. 多 Actor 应用打包与服务化启动

```sh
# 编译多 Actor 应用包（含监督重启与定时心跳示例）
cargo run -p wings-compile -- build-app examples/session-service.wings -o target/session-bundle

# 启动多 worker HTTP 服务（支持 POST /<handler> 与 GET /healthz）
cargo run -p wings-vm -- serve --trusted target/session-bundle --workers 2 --http 127.0.0.1:8080

# 或启动 TCP JSON Lines 服务
cargo run -p wings-vm -- serve --trusted target/session-bundle --workers 2 --tcp 127.0.0.1:9000
```

### 3. 开发与验证命令

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# 构建二进制与验证示例并执行完整端到端 AOT 回归（需 Python 3）
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_scalar_aot.py
```

## 文档导航

| 文档 | 说明 |
|---|---|
| [产品定位](docs/product.md) | 性能与内存目标、适用场景、与 BEAM / Tokio / Wasmtime 的取舍对照 |
| [架构设计](docs/architecture.md) | 10 个 Workspace Crate 边界、Cranelift AOT 管线、受管 C ABI 与精确 GC |
| [语言设计](docs/language.md) | `wings-lang` 语法、类型系统、异步消息、定时器与监控语义 |
| [运行时](docs/runtime.md) | 微进程表示、栈池、无锁邮箱、M:N 调度器、三级资源准入与网络服务入口 |
| [评测与验收](docs/evaluation.md) | BEAM 对照负载、当前性能/内存实测状态与优化实验纪律 |
| [风险与里程碑](docs/roadmap.md) | 早期演进策略、已知风险、M0–M7 阶段划分与正式 MVP 门禁清单 |
| [包发布](docs/publishing.md) | `Apache-2.0` 许可、crates.io 拓扑发布顺序与版本记录 |

参与代码或文档贡献前请阅读 [协作约定](AGENTS.md)。
