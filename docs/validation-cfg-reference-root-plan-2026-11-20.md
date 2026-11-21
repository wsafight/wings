# CFG 引用根计划验证（2026-11-20）

按 [ADR-0149](adr/0149-cfg-reference-root-plan.md) 替换 `wings-ir` 内部根集合计算，基于已推送的 `b0a616c`。本机 macOS 26.6.2 AArch64、Rust 1.99.1。IR 指令、codegen、ABI v22、shadow frame、检查点、预算和资源上限不变。

## 定向回归

- `cargo test --locked -p wings-ir` 通过 **49 项**。新增连续引用合流测试逐点检查：then/else 值只出现在各自合流边，第一个结果作为第二个合流的 phi 输入，合流后两个引用在 `Collect` 前同时保活，未选中的分支临时值不占根槽。空分支测试直接覆盖 `If -> Else -> EndIf`，在 If 前按两条路径保活不同输入、在 then 的 Else 点不保活 false 输入、在 merge 只发布合流结果。引用 state 零次循环测试覆盖 header/While、EndLoop 回边和 after-loop；另一个测试确认相同引用同时作为 state/reply 只占一个最终根槽。
- 既有嵌套合流断言由保守根集合改为路径精确结果：`at[9]=[1,8]`、`at[10]=[1,10]`、容量 2。首次执行新算法时旧断言按原预期失败，确认差异是删除未选中路径值，不是修改执行语义；没有用重跑掩盖。
- `cargo test --locked -p wings-compile` 通过 **47 项**，保留 `last_uses` 兼容诊断断言。`cargo check --workspace --locked` 和 `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。

## 真实 AOT 强制 GC

`examples/record-loops.wings` 新增三个 handler，复用 `verify_persistent_roots` 的独立 ctypes 宿主。该宿主在每次 heap 与 slow-path 回调前遍历真实 shadow frame 并强制收集，因此后续读取若漏根会实际失败，而非只比较静态计划：

- `Sequential` 连续两次引用合流，覆盖 Bool 四种组合及两个结果在收集后的同时使用；预期回复依次 110、130、121、131。
- `Backedges` 在嵌套循环回边后读取内层、外层和循环前保存的引用，覆盖外层 0/1/5 次进入及内层回边；预期 state/回复分别为 0/20、2/22、6/26。
- `ZeroExit` 不进入循环，在 after-loop 收集后读取入口候选和既有 state，预期 state=6、回复=36。
- `EmptyBranches` 两臂都直接选择已有引用，没有臂内 heap 操作；两条路径均在合流后强制收集，分别得到 state/reply 40/40 与 40/40。

Debug 与 Release 分别由当前源码重建 `wings-compile`，生成独立产物并执行 `--check-record-loop-roots`，均通过。日志/产物在 `target/root-plan-validation-20261120/`；该定向结果不替代完整 AOT。

## 完整验证与边界

- Debug 全目标有界测试通过 **108 个 harness、728 项**，报告为 `target/root-plan-validation-20261120/workspace-debug/report.json`；运行器自测 8 项和 AOT 运行器自测 14 项通过。每个 harness 保持 CPU 20s、墙钟 30s、core 0。
- Release 全目标有界测试同样通过 **108 个 harness、728 项**，报告为 `target/root-plan-validation-20261120/workspace-release/report.json`；两种构建 doctest 均通过。
- `cargo fmt --all -- --check`、`git diff --check` 通过；224 份 Markdown 的相对链接与代码围栏检查通过。两份全目标报告 SHA-256 分别为 `96355473f6e903ca4034180a6a4704033cfb25842ca3ed785156e4d6436e7298` 和 `9c6ca11ab1f309ad3b5dbabb73a732c09c6986e1ab79aa7c34a7d63f24b01e25`。
- Rust 1.85.0 的 workspace all-targets check 与 Linux x86-64 的 `wings-vm` all-targets 交叉检查通过；交叉检查不算原生 Linux 执行。
- Debug 与 Release 原限完整 AOT 均一次通过，日志分别为 `target/root-plan-validation-20261120/full-aot-{debug,release}.log`，两者 SHA-256 均为 `00bb2a652119e1e7902d15da09e5cad0604ce7d8fc6afa6a759db8ff12a6b5b5`。本轮未复现历史启动超时，不构成其根因修复。
- ADR-0148 的既有 push CI `44721198246` 已全部通过：原生 macOS AArch64、Linux x86-64 及 MSRV 三job成功。该结果只关闭前一切片的 CI 查询，不覆盖本片源码。
- 本片提交为 `2705753e6f44e8d566ab6eeaaa42bcab9b6e8d56`。两次 `git push` 客户端均报告接收超时，但远端 tracking ref 随后已指向该提交；保留客户端失败，不写成两次命令成功。push CI [`44727811707`](https://github.com/wsafight/wings/actions/runs/44727811707) 与 PR CI [`44727818547`](https://github.com/wsafight/wings/actions/runs/44727818547) 均已成功。
- 本片不开放递归、前向调用或动态派发，不完成动态调用根链。历史启动超时、完整资源账本、公平/过载/故障长测、最新原生双平台及 BEAM/MVP 门禁保持开放。
