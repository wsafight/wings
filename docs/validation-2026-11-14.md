# 2026-11-14 验证记录

[文档目录](../docs.md) · [评测与验收](evaluation.md) · [前次记录](validation-2026-11-13.md)

## 稀疏就绪队列与大容量 actor：ADR-0062

基于 `3331eeb`，本次只改 Scheduler 的派发队列、空槽取得及 actor 容量，不改变 ABI v14、语言语法、请求表容量、活栈/提交/副作用契约。源码设计见 [ADR-0062](adr/0062-sparse-ready-scheduler.md)。语法评审草案未纳入提交。

### macOS AArch64

环境：macOS 26.6.2、Apple Silicon AArch64、Rust 1.99.1；真实 BEAM 为 OTP 29/ERTS 17.1.1 JIT，同前次记录。以下均为本机功能证据，不是吞吐、尾延迟或整机内存结果。

- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check` 和 Rust 1.85.0 `cargo check --workspace --all-targets --locked` 通过。
- debug/release `cargo test --workspace --locked` 各 458 项通过；两种构建的 TCP/Counter 桥接示例单测各 2 项通过。依赖隔离、禁止协程 unwind feature、文档链接/围栏与 diff whitespace 检查通过。
- 新增纯队列回归覆盖双 worker FIFO、重复发布去重、头/中/尾摘链、退役先移除、代次复用拒绝旧成员、owner/stop 不可排队及返回后固定编号迁移。actor 配置拒绝 16385，请求仍拒绝 257；按需池上限 16384 的构造只计控制、不映射业务栈，公开固定池仍拒绝 2049。
- 保留并通过并发生产者、唯一活栈 owner、暂停/取消、队满、启动拒绝后 FIFO 恢复、超时/迟到结果、语言消息/完成、GC、监督及关闭后请求租约测试。

`cargo build --locked -p wings-compile -p wings-vm --bins --examples` 后执行 `python3 scripts/verify_scalar_aot.py`；release 同命令加 `--release`，运行脚本前设 `WINGS_PROFILE=release`。脚本现在包含 `scheduler_sparse` 的真实 Counter AOT 子进程：1000/10000 actor、2 worker、100 个活跃 actor（各自 slot 间隔为 actor 数除以 100）、mailbox 2、宿主结果 64、每次最多 32 请求。每 actor 三轮各两个连续 Inc，共 600 回复；以不同 delta 检查独立状态、FIFO、五次边界迁移和空 owner。另检查空闲 actor 为零、表满无部分准入、退出后旧 Pid 拒绝、新代初始化为零和 Scheduler Drop 全部归零。此处 mailbox 2 为 FIFO 功能测试，不冒充性能协议 mailbox 1。

每组一次冷启动，VM/tenant 逻辑字节上限各 1 GiB，process 字节上限 4 MiB。debug/release 的以下逻辑预留相同（B），在业务请求前记录；不等于所有 Rust/装载器实际内存，也不是 RSS：

| actor 数 | 活跃 actor 数 | 空表预留 | 创建后预留 | actor 增量 |
|---|---|---|---|---|
| 1000 | 100 | 12820592 | 15652592 | 2832000 |
| 10000 | 100 | 64660592 | 92980592 | 28320000 |

容量控制、本地执行容器仍按最大 actor 数预留，不能假称零成本；消除的是每次派发对所有空闲槽的扫描，不声称创建数量本身代表可扩展性能。每 actor mailbox 2 的控制增量为 2832 B，不能与历史 mailbox 1 的 1672 B 混为同配置优化。该探测使用标量 state，没有完整 256 B/4 KiB/64 KiB Bytes state，不代替 BEAM 代表性业务负载。

完整 debug AOT 首次在既有 TCP normal 场景读取第一条回复时遇到 ConnectionResetError；保留原代码、断言和限制，第二次完整通过。完整 release 首次通过。两种构建另行 `verify_counter_equivalence.py --compare-beam` 均通过真实 Wings/OTP/独立 Python 的 45 业务、28 JSON 拒绝与 14 桥接拒绝；它仍是单 actor 正确性，不是多 actor 性能基线。其他 AOT 子进程保持 CPU 20 秒/墙钟 30 秒/禁 core；Counter 业务保持 CPU 5 秒/墙钟 10 秒、actor 总 deadline 3 秒。稀疏探测每次结果等待最多 3 秒，外层仍受原 AOT 上限约束。

### Linux x86-64

从 `0baffdc`（含 `3b094f8` 的就绪队列与容量改动）重新交叉构建全部 release 工具/示例及测试，27 个独立 harness 共 462 项通过（workspace 458、TCP 2、Counter 桥接 2）；完整受限 release AOT 首次通过，包含真实 Linux `.so` 的 1000/10000 actor 稀疏探测、FIFO/代次/清理、暂停/迁移/监督及固定字节 Counter 模型对照。新增探测仍在原 CPU 20 秒/墙钟 30 秒/禁 core 下执行，没有扩大时间上限。

环境沿用无宿主挂载的 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB；macOS 用 Rust 1.99.1 与 Zig linker 交叉构建，在客体内 GCC 链接 AOT `.so` 后加载执行。Linux 未安装 OTP，未执行原生 debug/clippy/MSRV/doc-tests；本段是 Linux 内核/ABI 的隔离仿真功能证据，不是最新原生 CI、真实 Linux BEAM 对照或性能结果。测试结束停止隔离实例，不保留后台业务进程。

## 多 actor 完整 Bytes Counter：ADR-0063

基于 `3fa1bd8`，接入 [ADR-0063](adr/0063-multi-actor-byte-counter.md) 与 [v2 正确性协议](beam-workloads.md)。不改变 ABI v14、现行类型大小写或已发布包，语法评审草案仍未纳入提交。

### macOS AArch64

环境沿本页前段。workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 全部通过；debug/release workspace 各 459 项、三个示例桥接各合计 5 项通过。完整受限 debug/release AOT 首次通过，包含新多 actor 用例；两种构建各自显式运行真实 OTP 29/ERTS 17.1.1 JIT 对照，v2 均通过 18 组业务、17 个二进制拒绝及 49 个 JSON 拒绝，v1 均继续通过 45/14/28 项。

代表容量为 10000×256 B、1000×4 KiB、100×64 KiB，均保留完整不可变状态与每目标 ID，100 活跃目标、256 请求；另外覆盖最小 actor、空业务、跨窗口、多目标独立失败、checked 两端与溢出优先。Scheduler 新单测检查提交副本、候选失败不提交、旧 Pid/关闭拒绝、非 Bytes/活栈拒绝及最后资源归零；副本在清理后仍可持有，不保留堆/模块/账户租约。Wings 只导出长度/32 B 前缀，不把 `states_verified` 写成全量 padding 导出。依赖隔离、禁 unwind feature、Markdown 相对路径/围栏及 diff whitespace 检查通过。

### Linux x86-64 隔离仿真

同前段环境交叉构建并运行 28 个 release harness，共 464 项通过（workspace 459、三个示例桥接合计 5）。完整 release AOT **未通过**：新 10000×256 B Counter 在 actor 阶段的既有 3 秒 deadline 内未完成；独立有界复现仍失败，其余 17 组通过。未放宽 CPU 5 秒/墙钟 10 秒/actor 总 deadline 3 秒，也不删去大容量用例。后续需消除创建/监督路径的容量扫描及重复初始化开销，再重新执行完整套件；不能把本机通过写成 Linux 通过。

该结果仍是闭环功能验证，没有预热/采样、吞吐、尾延迟或整机内存结果。

## 未完成门禁

- 最新源码的原生 Linux 构建/clippy/MSRV/doc-tests 和原生双平台 CI，不能用 QEMU TCG 或 macOS 交叉编译替代。
- 多 actor 正确性在 Linux 的上述 3 秒失败；两个发送者 FIFO、开环到达、热点/公平性、过载/超时及端到端诊断。mailbox 100 的 hot-queue 协议当前仍超出实现上限 64，不更改协议绕过。
- 相同业务/同平台同机器的 BEAM 吞吐与整机内存双目标，包括预热 30 秒、采样 60 秒、5 次重复和尾延迟/拒绝要求。
- 更完整的业务值与资源账本闭环；当前初始 slot 分布及返回后固定编号迁移不是自适应 work stealing，语言 capability 查找与最多 256 pending 维护仍为有界扫描。

M0/MVP 状态不变；不推送、发布或降低验收门槛。
