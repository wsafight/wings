# Wings VM

面向高并发有状态应用的微进程虚拟机。

Wings 的目标是**在相同业务负载下，比 BEAM 更快、更省内存**。业务代码通过消息交互，各微进程保存自己的状态，VM 负责调度、内存管理和监督恢复。

- **性能**：在相同 CPU 配额和尾延迟要求下，取得更高的持续吞吐。
- **内存**：在相同状态、消息和活跃比例下，降低整机内存占用，提高单机进程密度。
- **执行模型**：各进程的业务状态相互隔离，保留公平调度、监督恢复和资源检查。

VM 用 Rust 实现，`wings-lang` 业务代码通过 Cranelift AOT 编译执行。首要运行平台是 **macOS AArch64（Apple Silicon）与 Linux x86-64**，两者都要通过完整的 VM 运行测试和性能验收。组件说明见[架构设计](docs/architecture.md)。

## 文档

- [产品定位](docs/product.md)：性能与内存目标、适用场景和采用路径。
- [架构设计](docs/architecture.md)：技术选型、编译后端、受管 ABI、GC 和平台范围。
- [Crate 拆分](docs/crates.md)：workspace 依赖、`wings-vm` 与编译工具的边界，以及 `wings-lsp` 方案。
- [架构决策](docs/adr/README.md)：当前已采纳的设计、替代方案与重审条件。
- [运行时](docs/runtime.md)：微进程、内存、调度、消息、资源额度、监督和热更新。
- [语言设计](docs/language.md)：`wings-lang` 的 handler、状态和异步交互语义。
- [激进优化](docs/optimizations.md)：编译、值布局、分配、消息、调度、GC 与平台专用优化。
- [评测与验收](docs/evaluation.md)：BEAM 对照、负载、指标和故障注入。
- [风险与里程碑](docs/roadmap.md)：已知风险、M0–M7 的范围和验收条件。

完整阅读顺序见[文档目录](docs.md)。参与修改前请阅读[协作约定](AGENTS.md)。

## 当前进度

处于 M0：已有 `process`、具名 `state`、类型化 `on`、状态赋值、局部 `let` 和尾表达式的前端检查及单文件 LSP。Cranelift 标量 AOT 原型可链接、装载可信本机产物并连续调用 Counter；macOS AArch64 与 Linux x86-64 已通过同一套 CI 原生执行验证。原型没有业务堆、调度、抢占或 GC，完整 M0 与性能、内存目标尚未验证。更新日期：2026-11-12。

## 开发

需要 Rust 1.85 或更新版本；构建 AOT 原型还需系统 C 链接工具链（`cc`，也可用 `CC` 指定编译器可执行路径）。检查命令只验证当前语法子集，不生成机器码：

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p wings-compile -- check examples/counter.wings
cargo run -p wings-lsp
```

当前源码可额外构建并运行标量 AOT 示例。输出目录必须不存在；`--trusted` 表示只装载自己构建、可信且与清单匹配的本机原生代码，不是安全沙箱：

```sh
cargo run -p wings-compile -- build examples/counter.wings -o target/counter-aot
cargo run -p wings-vm -- run --trusted target/counter-aot < examples/counter.messages.jsonl
```

可复现的端到端边界验证：`cargo build --locked -p wings-compile -p wings-vm`，再运行 `python3 scripts/verify_scalar_aot.py`（需要 Python 3）。`cargo test` 当前仍没有 Rust 测试用例，行为验证由该独立脚本执行。

此路径只验证直线标量 handler 的原生执行与状态提交，尚无 mailbox、资源账户或监督。产物与驱动协议见[架构原型说明](docs/architecture.md#标量-aot-原型)，不将成功执行示例视为完整 VM 已交付。

LSP 使用 stdio、UTF-16 位置和整份文档同步，支持诊断、局部跳转、悬停与基础补全。目前逐份文档同步分析，尚无跨文件解析、增量失效或运行中取消；双平台 CI 覆盖构建，LSP 协议冒烟仍仅在本机 macOS AArch64 验证。语言支持范围见[语言设计](docs/language.md)。

当前 workspace 版本为 `0.0.2`，采用 [Apache-2.0](LICENSE) 许可证。本次发布范围是全部十个已有实现的包，包括标量 AOT 后端与可信装载原型，不代表完整 VM 已交付。发布顺序、状态和名称检查见[发布说明](docs/publishing.md)。

crates.io 的 `0.0.1` 是原前端版本，仅有 `check` 和 LSP；不能覆盖该版本。`0.0.2` 增加 AOT 构建与可信执行入口，按 PR 合并、CI 验证、手动发布的顺序交付；PR 本身不触发包上传。

`wings` 已由其他项目使用，本项目不发布同名聚合包。`wings-lang` 仅作语言名，不是待发布的 crate；项目名与商标核查仍未完成。名称约定见 [ADR-0008](docs/adr/0008-wings-naming.md)。

## 安装

`0.0.2` 发布完成后，可从 crates.io 安装编译命令、标量执行原型和语言服务器（构建 AOT 还需系统 C 工具链）：

```sh
CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/ cargo install wings-compile wings-vm wings-lsp --version 0.0.2 --locked --registry crates-io
wings-compile check examples/counter.wings
wings-compile build examples/counter.wings -o target/installed-counter-aot
wings-vm run --trusted target/installed-counter-aot < examples/counter.messages.jsonl
wings-lsp --help
```

示例路径相对于本仓库，输出目录必须不存在；检查命令只做静态检查，LSP 通过 stdio 启动，不执行用户程序。`wings-vm` 仅装载可信本机产物，不是安全沙箱。
