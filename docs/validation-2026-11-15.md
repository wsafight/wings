# 2026-11-15 验证记录

[文档目录](../docs.md) · [评测与验收](evaluation.md) · [前次记录](validation-2026-11-14.md)

## 计费的有界 IntList 回复：ADR-0097

基于 `2d59f98`，实现 [ADR-0097](adr/0097-accounted-int-list-replies.md)。保持既有类型拼写、ABI v15、受管取消/预算、原请求唯一终态及副作用调用点生效；新增状态 ReplyRejected，不改变 context/output/effect 布局。语法提案仍未采纳、未纳入本切片提交，Bytes 回复、完整业务值与 MVP 门禁未完成。

生成入口返回后仍在原协程内为回复与候选引用状态建立出口根，逐节点复制最多 64 个整数，检查末尾空节点。目标预留并拥有独立结果，成功完成全部验证/复制才提交候选；超过上限、无效引用、取消、总预算和存储拒绝均不提交。结果不保留 actor、模块、业务堆或栈；Clone 共享已计费存储，最后拥有者释放预留。JSON 输出为 `{"reply":{"int_list":[...]}}`，输入仍不接受引用、段编码或系统 RequestId。

### macOS AArch64

环境：macOS 26.6.2（25G83）、Apple Silicon AArch64、Rust 1.99.1；MSRV 为 Rust 1.85.0。以下是本机功能证据，不是 BEAM 吞吐、尾延迟或整机内存结论。

- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、`cargo +1.85.0 check --workspace --all-targets --locked` 全部通过。
- debug/release 各构建全部测试后，在独立 OS 子进程内运行 **87 个 harness、580 项测试**，全部通过；两种构建的 workspace doc-tests 通过。
- 两种构建的完整 `scripts/verify_scalar_aot.py` 全部通过，包含新列表驱动、CLI 内容与边界、以及原有标量/记录/字节、调度/监督/请求、GC、容量压力与拒绝回归。
- 每种构建另行执行 40 个隔离重复进程：4 组 runtime 回归各 8 次，共 88 次测试执行；真实列表 AOT 驱动 8 次，共 576 组语言场景，另含每轮宿主/导入检查，全部通过。运行前用 `--list` 核对各组恰有 2/2/5/2 项，避免空匹配通过。
- 依赖隔离、协程禁止 unwind feature、Markdown 相对链接/围栏、Python AST 和 diff whitespace 检查通过。

新运行时测试验证结果对象 0/1/64 元素、65 元素拒绝、进程/租户/VM 三级预留失败、源内容独立、Clone/末拥有者 Drop、账户句柄释放后继续持有结果；出口 0/1/16/64 次恢复后的取消、64 节点加空尾的精确 65 步预算、丢弃暂停调用的正常取消与清理。执行开始后占满三级最小剩余容量，验证结果分配失败保留旧状态。worker 与一/二 worker Scheduler 均检查队满保留完成路由、释放容量后规范列表信封、发起方退出后释放路由和保留结果票据。前端/共享 LSP、IR、Manifest 与两种 runtime 的五种回复交叉矩阵独立检查精确类型和无效结果。

[`examples/list-replies.wings`](../examples/list-replies.wings) 与 `list_replies` 驱动覆盖直接同步、ScalarTask、worker、监督、固定 ingress 和一/二 worker Scheduler。语言场景为长度 0/1/64 × 成功/零超时/取消/目标失败 × 自己/子进程 × worker/Scheduler1/Scheduler2，共 72 组；检查内容顺序、i64 两端、候选与旧列表同时保活、完成入口收集、非成功默认空列表、超时/取消不撤回目标执行、目标与宿主组件退出后保留结果、导入失败及最终根基线。取消保留原允许竞争，不强制跨 worker 的线程胜出顺序。

构建和完整 AOT 命令：

```sh
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo build --workspace --locked --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

独立 harness 运行器读取成功完成构建的 JSON 清单，逐个启动 executable、检查退出码并累计测试数。每个独立 harness 与 AOT 子进程保持 **CPU 20 秒、墙钟 30 秒、core 0**；业务阶段仍是 **3 秒**。没有删除压力用例、缩短业务状态、提高容量或放宽时限。上一切片的 dev 后端优化配置保持不变，debug assertions/overflow checks 仍启用。

### Linux x86-64 隔离仿真

环境沿前次记录：无宿主挂载的 QEMU TCG、Ubuntu 24.04 x86-64、2 vCPU/4 GiB；macOS 交叉构建 release 工具/示例/测试，客体 GCC 链接真实 Linux AOT `.so`。冻结包为 **344 个源码文件、87 个 harness、65 个工具/示例二进制**，共 496 项哈希；语法提案不在包内。冻结源码与宿主工作区逐一比对 344 项；本验证记录与后补目录/证据链接在冻结之后写入，不属于该冻结包。

客体独立运行的 **87 个 harness、580 项测试通过**，完整受限 release AOT 全部通过。相同的 40 个隔离重复进程通过，含 88 次 runtime 测试执行与 576 组真实语言场景及宿主/导入检查；测试前逐组核对非空精确测试数。测试后 496 项源码/测试/工具哈希再次匹配，未放宽原 CPU/墙钟、业务阶段和压力负载限制。测试后已停止隔离实例，并核对 `wings-x86` 为 `Stopped`。

该环境只提供 Linux 内核/ABI 的隔离仿真功能证据，不代替最新源码原生 Linux CI、debug/clippy/MSRV/doc-tests、真实 Linux BEAM 对照或性能验收。

### 本次失败与证据边界

开发过程修正了未限定状态常量、旧 IntList 拒绝断言、缺少新结果变体的穷尽匹配，以及单元素循环的 Clippy 失败。新增 fixture 曾误用启动 API、错误要求取消丢弃后保留业务堆，以及未按三级最小剩余额度设置拒绝边界；均修正后重跑。一次构建失败后的旧 shell 流程仍读取部分 JSON 并输出 85 harness/319 tests，该结果无效，未计入验收。最终运行器顺序检查构建成功，debug/release 均以实际 87/580 完整结果为准；这些 fixture/断言问题不记作 runtime 缺陷或通过证据。

Bytes 回复、字符串/通用记录、完整启动/装载资源账本、长时公平/过载/故障与恢复诊断、原生双平台 CI，以及各平台同机同负载 BEAM 持续吞吐和整机内存双门禁仍未完成，MVP 状态不变。
