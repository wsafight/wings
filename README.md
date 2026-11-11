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

已开始 M0 前端实现：支持 `process`、具名 `state`、类型化 `on`、状态赋值、局部 `let` 和尾表达式回复检查；包含源码位置与 UTF-16 转换、保留原文的 token 流、容错解析和共用前端的 stdio LSP。还没有 AOT 代码生成、产物装载或 VM 执行，性能和内存目标尚未测量。更新日期：2026-11-12。

## 开发

需要 Rust 1.85 或更新版本。检查命令只验证当前语法子集，不生成机器码：

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p wings-compile -- check examples/counter.wings
cargo run -p wings-lsp
```

LSP 使用 stdio、UTF-16 位置和整份文档同步，支持诊断、局部跳转、悬停与基础补全。目前逐份文档同步分析，尚无跨文件解析、增量失效或运行中取消；仅在本机 macOS AArch64 验证，Linux 运行验证尚待完成。语言支持范围见[语言设计](docs/language.md)。

当前 workspace 版本为 `0.0.1`，采用 [Apache-2.0](LICENSE) 许可证。五个前端和 LSP 包已发布到 crates.io，不包含尚未实现的 VM。发布顺序、状态和名称检查见[发布说明](docs/publishing.md)。

`wings` 已由其他项目使用，本项目不发布同名聚合包。`wings-lang` 仅作语言名，不是待发布的 crate；`wings-vm` 和 `wings-lang` 在 2026-11-12 查询时尚未注册，不代表名称已保留。项目名与商标核查仍未完成。名称约定见 [ADR-0008](docs/adr/0008-wings-naming.md)。

## 安装

从 crates.io 安装已发布的检查命令和语言服务器：

```sh
CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/ cargo install wings-compile wings-lsp --version 0.0.1 --locked --registry crates-io
wings-compile check examples/counter.wings
wings-lsp --help
```

示例路径相对于本仓库；检查命令只做静态检查，LSP 通过 stdio 启动，不执行用户程序。
