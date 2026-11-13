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

## 固定监督索引与空槽链：ADR-0064

针对上一段 Linux 失败，在 `28db2c7` 上按 [ADR-0064](adr/0064-indexed-supervisor-slots.md) 消除监督创建寻找空槽及 current_pid/stop 的线性搜索，保留原始稳定身份、全部准入/初始化顺序和终态契约；poll 仍为固定容量扫描。没有改初始化栈、状态大小、测试规模或时间限制。

Linux 同配置 QEMU TCG x86-64 重新构建全部 release 工具/示例及 28 个 harness，共 465 项通过（workspace 460、三个桥接示例合计 5）；完整受限 release AOT 首次通过，包括此前失败的 10000×256 B、另外两组大容量 Bytes、全部旧 AOT/迁移/暂停/GC/监督回归。单独的 v2 Wings/Python 套件也在原 CPU 5 秒/墙钟 10 秒/actor 总 deadline 3 秒通过。前段超时是改造前结果，现已解决，不删除该失败记录。

本机 macOS check/clippy/fmt/MSRV 通过，debug/release workspace 各 460 项、三个示例各合计 5 项通过。新增回归覆盖八轮满表/任意顺序停止与复用、全部旧身份/外来 supervisor 拒绝、失败创建不占槽和重启失败后继续创建；保留稳定 child 重启与强度、并发退出不丢失及活栈停止测试。完整 debug/release AOT 首次通过；两种构建显式 v2 三端对照均通过 18/17/49 项，v1 均通过 45/14/28 项。

依赖隔离与禁 unwind feature、文档链接/围栏和 diff whitespace 检查通过。隔离 Linux 未安装 OTP，也未运行原生 debug/clippy/MSRV/doc-tests，不是最新原生 CI 或 Linux BEAM 证据。实例测试后已停止。没有吞吐、尾延迟或整机内存结果。

## 双发送者 FIFO 与合法交错：ADR-0065

基于 `7b9a035`，新增 [ADR-0065](adr/0065-two-sender-counter-fifo.md) 的独立 v3 Counter 正确性协议，未更改 v1/v2、ABI v14 或语言类型大小写。两个真实宿主线程/BEAM 进程直接发送、各 16 条窗口，完整 Bytes 保存各发送者到每目标已提交序号；独立前缀模型检验合法交错，不强求两端回复值逐条相等。只接受所有交错均安全的加法集合，并发失败/溢出仍未由 v3 覆盖。

本机环境沿本页前段；workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过。debug/release workspace 各 460 项、四个桥接示例各合计 6 项通过。完整 debug/release AOT 首次通过；两种构建显式真实 OTP 29/ERTS 17.1.1 JIT 三端均通过 21 组业务、8 组伪造历史拒绝、17 个桥接与 47 个 JSON 拒绝，另有真实 AOT 跳号/起始序号错误和成功后重复序号失败检查。两种构建 v2 三端均继续通过 18/17/49，v1 均通过 45/14/28。

业务涵盖三种完整 state、单目标两流各 128 请求、100 目标、空/非均衡流、跨 16 条窗口、负 delta 与 i64 两端安全集合。模型另接受两种不同合法交错，拒绝非法回复、交换序号、错误终值、重复键式结果及非整数结果；不是仅用交换律核对总和。Wings 最后检查实际长度/计数器/ID/两流已提交序号及逻辑归零，不把固定前缀称为完整 padding 导出。

Linux 同配置隔离仿真重新交叉构建全部 release 工具/示例，29 个 harness 共 466 项通过（workspace 460、四个桥接合计 6）；完整受限 release AOT 首次通过，包含新双发送者、旧三种大容量 Bytes、全部迁移/活栈/GC/监督回归。Linux 未安装 OTP，未执行原生 debug/clippy/MSRV/doc-tests，本结果不替代最新原生 CI 或 Linux BEAM 对照；测试结束已停止实例。

所有业务仍为 CPU 5 秒/墙钟 10 秒/禁 core、actor 阶段总 3 秒，完整 AOT CPU 20 秒/墙钟 30 秒不变。依赖隔离/禁 unwind feature、Markdown 相对链接/围栏和 diff whitespace 检查通过。此处无开环、吞吐、尾延迟或整机内存结果。

## mailbox 100/256 槽准入：ADR-0066

基于 `ba11647`，按 [ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 将 Scheduler mailbox 参数上限从 64 扩至 256，默认/原显式容量及 ScalarWorker 不变。原 hot-queue 要求 100 槽不改；只补准入前置，没有 BEAM 等价有限准入层或开环性能结果。

workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 462 项、四个桥接合计各 6 项通过。新增 64/100/256 满队列、0/257/极大配置拒绝、process/tenant/VM 额度失败回滚、额外 send/request 不变账目、全局 256 结果槽、健康 actor 推进、满队列停止、已消费终态不能再取及最终归零。内部直接验证 100/256 环形 FIFO 回环与 starting 保留容量/队首恢复。真实 `scheduler_overload` 用完整 4 KiB Bytes、强制检查点 Hold 和 100 排队请求验证拒绝/健康 actor/取消清理；所有 AOT 仍受原 CPU 20 秒/墙钟 30 秒/禁 core。

本机完整 debug/release AOT 首次通过；两种构建 v3 显式三端均通过 21/8/17/47，v2 均通过 18/17/49，v1 均通过 45/14/28。Linux 同配置隔离仿真 29 个 release harness 共 468 项通过；首次完整 AOT 在旧 v2 大容量 Bytes 用例触发原 3 秒阶段 deadline，保留原代码/规模/限制第二次完整通过，包含新 100 槽 Bytes 满队列和旧全部回归。没有将超时当预期业务失败，也不以新增满队列专项通过代替完整套件；原生门禁及该大容量路径的耗时余量仍待验证。

依赖隔离、禁 unwind feature、Markdown 链接/围栏及 diff whitespace 通过。Linux TCG 未安装 OTP、未执行原生 debug/clippy/MSRV/doc-tests，测试后已停止实例；功能结果仍不替代最新原生双平台、真实 Linux BEAM 或吞吐/整机内存门禁。

## BEAM 发送前有限准入：ADR-0067

基于 `223db2d`，新增 [ADR-0067](adr/0067-beam-bounded-admission.md) 的固定 Pid/容量/atomics gate，发送前 CAS 占槽、绑定 actor 开始归还、终态关闭；没有代理、无界等待者或自动重试。v4 用真实 Wings/BEAM 两个完整 4 KiB actor 核对 1/100/256 排满、下一条拒绝、健康单向消息提交 +7、满队列停止、已接受请求失败和旧入口拒绝。Wings 另核对结果一次消费与最终逻辑归零；BEAM 核对 DOWN/旧 binary/队列计数，不冒充 VM 资源账本。

本机 check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 462、四个桥接各合计 6 项通过。两种构建完整受限 AOT 首次通过，显式 OTP 29/ERTS 17.1.1 JIT v4 三端均通过全部容量、7 个非法容量拒绝及 gate 自测；gate 自测覆盖归还/FIFO/复用、非法/外来 started、两真实发送进程竞争不超额、关闭后迟到与竞争不重开。两种构建 v1/v2/v3 显式三端均继续通过，分别为 45/14/28、18/17/49、21/8/17/47。

Linux 同配置隔离仿真重新交叉构建全部 release 工具/示例、29 个 harness 共 468 项通过，完整受限 release AOT 首次通过，包含 v4 Wings/模型与旧全部回归。首次向客体传包遭 SSH 握手 reset；确认实例 Running 后重试同一实例传输成功，未重复启动 VM/业务。Linux 无 OTP、未执行原生 debug/clippy/MSRV/doc-tests，仍不是原生最新 CI 或 Linux BEAM 证据；测试后已停止实例。

业务保持 CPU 5 秒/墙钟 10 秒/禁 core、actor 阶段总 3 秒，AOT 原 CPU 20 秒/墙钟 30 秒不变；依赖隔离、禁 unwind feature、Markdown 链接/围栏与 diff whitespace 检查通过。无开环、计划时刻延迟、吞吐或整机内存结果；gate 的未来集成成本必须计入 BEAM 测量。

## 短时计划开环：ADR-0068

基于 `24f337d`，新增 [ADR-0068](adr/0068-bounded-open-loop-counter.md) 的 v5 `smoke`：同一单调 t0 的计划时刻、四组完整 Bytes/binary 代表负载、前 100 活动 actor、一个真实直接发送者及 BEAM CAS gate。每条到达均保留计划/实际发送/观察时刻、拒绝或外部 100ms 超时、迟到实际值；最终收齐接受请求并核对每目标 FIFO/提交/停止。最多 256 未收到真实结果的 pending，超时不释放、拒绝不重试，最多 10000 原始样本。无 ABI 或语法改动，待审语法草案不纳入提交。

本机 check、all-targets clippy `-D warnings`、fmt 和 Rust 1.85.0 all-targets check 通过；debug/release workspace 各 462、五个示例桥接/生成器单测各合计 9 项，合计各 471 项通过。新 Rust 单测核对严格参数边界、精确计划/热点连续轮转及 deadline 边界；独立 Python 模型覆盖超时 pending 保留、拒绝不执行、精确 100ms/迟到分类及 19 个伪造历史拒绝。两种构建最终完整受限 AOT 均通过，v5 Wings/BEAM/Python 显式三端各通过四组固定速率 100ms、两组高压到达和 10 个配置拒绝；v1/v2/v3/v4 两种构建三端亦全部通过，保留原 45/14/28、18/17/49、21/8/17/47 及全部 gate 场景。

Linux 沿原 2 vCPU/4 GiB/QEMU TCG 隔离环境重新交叉构建 release 工具/示例，30 个 harness 共 471 项通过。第一次完整 AOT 在新 dense-64k 的 10ms×1000000/s 用例没有在原阶段内收齐；原实现另一次完整参数专项仍失败，增加诊断后确认内部实际结果票据为 `TimedOut`，发生在业务段约 2.88 秒，接近初始化后剩余阶段边界。没有放宽 CPU 5 秒/墙钟 10 秒/总阶段 3 秒，没有删去 10000 到达或缩小状态/actor。

检查发现宿主每次到达最多全扫 256 个带锁结果票据；改为固定游标轮转、每轮最多探测 32 个，BEAM 同样每轮最多收集 32 条。修改只限制驱动每轮工作，不改 runtime 执行或减少原始样本，不承诺两端相同采样成本。最终原参数 Linux v5 专项及重新执行的完整 release AOT 均通过；本机亦重新执行完整 debug/release 回归与三端对照。保留原失败日志 `/tmp/wings-open-loop-linux-aot.log`、`/tmp/wings-open-loop-linux-probe.log` 和诊断日志，最终为 `/tmp/wings-open-loop-linux-aot-final.log`；不能把失败尝试改称成功。实例测试后已停止；Linux 未安装 OTP、未跑原生 debug/clippy/MSRV/doc-tests，不是最新原生双平台门禁。

本机 v5 原始 JSON 保留于 `target/open-loop-smoke-debug` 和 `target/open-loop-smoke-release`，包含所有拒绝和迟到，不据短时比例推断持续吞吐或领先。所有正确性/AOT 原资源限制不变；依赖隔离、禁 unwind feature、84 份 Markdown 路径/围栏、Python 语法和 diff whitespace 检查通过。没有长时预热/采样/重复、饱和扫描、分位达标或整机内存结果，MVP 仍未完成。

## 未完成门禁

- 最新源码的原生 Linux 构建/clippy/MSRV/doc-tests 和原生双平台 CI，不能用 QEMU TCG 或 macOS 交叉编译替代。
- 并发失败、长时开环、热点/公平性、过载/超时竞争及端到端诊断。双发送者安全加法各自 FIFO、有限准入/关闭、短时计划开环分别由 ADR-0065/0067/0068 补充，仍缺完整性能验收。
- 相同业务/同平台同机器的 BEAM 吞吐与整机内存双目标，包括预热 30 秒、采样 60 秒、5 次重复和尾延迟/拒绝要求。
- 更完整的业务值与资源账本闭环；当前初始 slot 分布及返回后固定编号迁移不是自适应 work stealing，语言 capability 查找与最多 256 pending 维护仍为有界扫描。

M0/MVP 状态不变；不推送、发布或降低验收门槛。
