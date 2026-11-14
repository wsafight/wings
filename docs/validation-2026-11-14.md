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

## 独立长时采样：ADR-0069

基于 `245e7fe`，按 [ADR-0069](adr/0069-streamed-open-loop-measurement.md) 新增独立 `--measure`，固定连续 30 秒预热/60 秒采样、五次配对重复，可选最多八点翻倍；原 v1–v5 的 CPU 5 秒/墙钟 10 秒/总阶段 3 秒及 AOT CPU 20 秒/墙钟 30 秒不变。最多 256 实际 pending、完整 state 和原路由/mailbox 保留，外部超时不取消提交。每个到达/回复写固定 40 B 事件、64 KiB 缓冲，独占新建文件；离线以固定 radix 桶精确选择分位，不将全量延迟留在内存。冻结执行文件、Erlang/gate、artifact 与源码，写入及监测成本不扣除。

### 本机长时集成结果

环境仍为 macOS 26.6.2 AArch64、Rust 1.99.1、OTP 29/ERTS 17.1.1 JIT。只执行 hot-queue 固定 10000 request/s：100 actor/全部活动、4 KiB state、mailbox 100、90% 到 actor 1。两端每次 900000 到达，其中采样 600000；五对共 9000000 到达、18000000 事件。全部收齐，无拒绝/超时，候选尾延迟通过。Wings 五次 p99 为 44000..88083 ns、p999 为 118125..1417833 ns；BEAM p99 为 6916..14709 ns、p999 为 71166..267833 ns。这里只证明该固定速率的长时集成，不是最大持续吞吐，不能据此声称 Wings 更快。

观测的进程树加驱动 RSS 峰值：Wings 44285952..67993600 B，BEAM 91996160..105627648 B，均含共享页重复统计和驱动；采样每秒一次，不是 RSS 连续峰值、macOS physical footprint 或完整整机内存门禁。实验部分时段与编译/正确性/QEMU 工作重叠，没有 CPU 硬配额、电源/热状态/QoS/核分配和完整平台内存证据。没有扫描饱和点，也没有其余三组长测或 fairness，故不报告性能/内存领先或 MVP 完成。

原始文件在 `target/open-loop-long-hot-v6-frozen`，精简结果、每次原始文件 SHA-256 和关键冻结输入哈希见 [JSON 证据](open-loop-hot-macos-2026-11-14.json)。十次原始事件均用最终离线 verifier 重新独立核对，全部与首次 summary 相同；冻结输入 SHA-256 全部一致。初次未冻结探索在开发时主动终止，记录为 `invalid_experiment` 并保留 `target/open-loop-long-hot-v6`，不计入五对。正式探测的 Python 驱动在其启动源码版本内运行，后续最终改动只加严离线事件时钟、失败清理、限制器与回归；不把两版本实验拼成同一个排名。

### 回归与失败记录

最终本机 workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 462，五个示例各合计 12 项，总计各 474 项。新增长测固定时段/合法倍频、40 B 编码、已有文件不可覆盖单测。Python 模型核对 90000 到达的相位/精确 deadline/迟到/拒绝、256 个超时 pending 不提前复用、三轮 radix 与排序 oracle 一致、跨 16/32 位与小样本不报 p999、伪造/截断/未知事件、启动失败/非零退出/不完整控制事件，以及更严继承限制不被提高。两种构建 v1–v5 真实三端全部通过，长测非法参数及已有文件拒绝在 Wings/BEAM 均通过；原 gate 测试不变。

完整 AOT 曾在新 Python 失败路径自测触发 `preexec_fn` 错误：限制器试图将父 AOT CPU 硬上限 20 提高到长测 240。改成对 soft/hard 与目标上限取最小值，并用更严 CPU/文件限制的独立子进程核对。没有改变父限制；最终重新执行 debug/release 完整 AOT 均通过，日志为 `/tmp/wings-long-last-aot-debug.log` 和 `/tmp/wings-long-last-aot-release.log`。此前失败日志保留，不能算首次通过。

Linux 同原 2 vCPU/4 GiB QEMU TCG 环境交叉构建 release 工具/示例。长测重叠期间完整 AOT 多次在旧 v2 大容量 Counter 触发原 3 秒 deadline/内部超时；一次原参数完整重跑通过，后续又复现，因此不以偶然通过删除失败记录，也不缩小 actor/state/到达规模。停止长测及本机回归后重新启动隔离客体、重传同一完整 package 和最终 Python/新示例 harness，30 个 harness 共 474 项和完整 release AOT 通过；最终日志 `/tmp/wings-long-linux-isolated-tests.log`、`/tmp/wings-long-linux-isolated-aot.log`，重叠失败保留于 `/tmp/wings-long-linux-{aot,final-aot,last-aot}.log`。这与竞争/仿真耗时有关但未独立证明根因，旧路径 3 秒余量仍是风险，不放宽限制。Linux 无 OTP/长测、无原生 debug/clippy/MSRV/doc-tests，测试后停止；仍非原生双平台证据。

依赖隔离、禁 unwind feature、85 份 Markdown 路径/围栏、Python 语法、JSON 证据与 diff whitespace 检查通过。未推送或发布，语法草案仍待审，完整 MVP 门槛不变。

## CPU 与突发心跳短时业务：ADR-0070

基于 `aed92ec`，按 [ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 增加 101 个真实 256 B actor、mailbox 1、同 worker 百万次 checked 加法与其余 100 actor 每 10ms 同刻突发心跳。Wings/BEAM 均直接发送且保留 255 实际冷 pending 加一个热请求；外部 100ms deadline 不释放旧 pending，不撤回已接受的状态提交。六层有界非递归函数沿用受管帧、根与强制检查点，不改语言、ABI v14、生产调度器或语法草案。

环境同上：macOS 26.6.2 AArch64、Rust 1.99.1，真实对照为 OTP 29/ERTS 17.1.1 JIT。workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 462 项、六个示例合计各 13 项，共各 475 项通过。新增严格时段/精确突发计划单测；两种构建完整受限 AOT 通过，日志 `/tmp/wings-fairness-aot-debug.log` 和 `/tmp/wings-fairness-aot-release.log`。没有提高原 CPU 5 秒/墙钟 10 秒/禁 core、总阶段 3 秒或完整 AOT CPU 20 秒/墙钟 30 秒限制。

两种构建显式 v1–v7 三端回归全部通过。v7 的 10/30ms 分别核对 100/300 条冷心跳、至少一个完成的百万次 CPU 任务、每目标 FIFO/值、最终 state 和停止清理；这些运行均收齐心跳，无拒绝或超时。独立模型另覆盖精确 deadline、迟到分类、拒绝不算零延迟、每 actor 最大等待、255 个已超时请求仍占 pending、额外到达拒绝，以及 20 份伪造历史；少于 10000 个样本不报 p999。最终原始输出与逐 actor 摘要在 `target/fairness-smoke-{debug,release}`；这不是 90 秒采样、吞吐或公平性验收。Wings 另以 100 steps 真实失败、10000000 steps 返回百万，确认总预算不可绕过。

Linux 使用同一 2 vCPU/4 GiB QEMU TCG 隔离客体，重新交叉构建 release 工具与示例。31 个 harness 共 475 项及完整受限 release AOT 通过，日志 `/tmp/wings-fairness-linux-tests.log` 和 `/tmp/wings-fairness-linux-aot.log`；没有缩小旧 v2 大容量用例或百万次任务。本次通过不消除上节记录的大容量阶段 deadline 余量风险。客体无 OTP/长测，也未执行原生 debug/clippy/MSRV/doc-tests；测试后停止。仿真仍不等同最新原生双平台。

依赖隔离与禁止协程 unwind feature 检查通过。`observed_while_CPU_pending` 仅表示票据区间重叠，不是实际 CPU 执行或让出时间戳；单 worker/单在线 scheduler 不是 OS 单核配额。长时 fairness、独立让出延迟、CPU 硬配额、平台完整内存及 BEAM 吞吐/内存双目标均未据此通过。未推送或发布，语法提案仍待审。

## 可选原子让出观测：ADR-0071

基于 `5c61b1e`，按 [ADR-0071](adr/0071-observed-atomic-yield.md) 增加 `PreemptionToken::new_observed(account)` 与固定大小副本查询。显式账户先预留 4096 B，再分配 observer/token；首次未消费请求与最近确认让出分开保存。只有私有 checkpoint 消费请求、真实 coroutine 切回宿主后才确认时间，默认 token 不读时钟/不取诊断锁。ABI v14、编译器强制检查点、取消/预算顺序和状态提交契约不变，没有把独立 fixture 的样本冒充 Scheduler timer 指标。

最终本机 check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 472 项、六个示例合计各 13 项，总计各 485 项通过。新增 10 个测试覆盖首次请求合并、真实切栈后确认、步骤/effect/取消/预算不误报、跨线程请求、后继请求不被旧确认清除、poison 恢复、三个资源层级准入失败回滚与租约最终释放。早期测试把“已正常返回后的 invocation”误当作仍持有 observer，失败后改为验证挂起期间持有、返回后释放；没有延长对象生命期来迎合测试。测试开发时的 Rust 借用错误也在编译阶段修正，不计入最终通过。

真实 AOT 的 `observed_yield` 以原百万次 checked 加法负载验证预请求和远端线程请求。预请求不消耗新 steps，原子请求让出后仍算出百万，再发 Pulse 确认 state 为 2；远端若未命中执行窗口则显式标为未消费/null，不编造零延迟。`verify_fairness.py` 在原 CPU 5 秒/墙钟 10 秒/禁 core、总阶段 3 秒限制内执行并核对 JSON。完整 AOT 原 CPU 20 秒/墙钟 30 秒不变；候选 p99<5ms、长时公平性、CPU 硬配额与双平台性能门禁仍未通过。

最终 debug 完整 AOT 及显式 v7 三端通过，日志 `/tmp/wings-yield-final-aot-debug.log`、`/tmp/wings-yield-final-three-debug.log`。release 完整 AOT 一次在旧 TCP 失败场景读首条回复时遇到 `ConnectionResetError`，当时与 QEMU/其他回归重叠；没有独立证明根因，也没有放宽 100ms I/O 或请求边界。初次开发版 release AOT 已通过；最终源码在停止客体、无并行重型测试后完整重跑通过，日志 `/tmp/wings-yield-isolated-aot-release.log`，失败日志 `/tmp/wings-yield-final-aot-release.log` 保留。最终 release v7 三端与独立观测通过，原始输出在 `target/observed-yield-final-{debug,release}`；只作功能证据。

Linux 同配置隔离仿真 release 31 个 harness 共 485 项通过。首次 AOT 传包漏了编译工具的两个 example，在 `frame_limits` 启动前报文件缺失；补传原 cross-build 的两份现成 binary，不改源码/边界，完整 AOT 从头重跑通过。日志 `/tmp/wings-yield-final-linux-tests.log`、`/tmp/wings-yield-final-linux-complete-aot.log`，首次传包失败在 `/tmp/wings-yield-final-linux-aot.log`。客体仍无 OTP/原生 debug/clippy/MSRV/doc-tests，测试后已停止，不替代原生 Linux 门禁。CI 补上 open-loop/fairness 示例单测的 debug/release 入口，但未经推送、未宣称远端 CI 已通过。

停止客体后最终 debug/release v1–v6 显式 Wings/BEAM/Python 回归全部通过，v6 仅核对模型/collector/配置，不重跑 90 秒实验。依赖隔离、禁 unwind feature、88 份 Markdown（含待审语法草案）的相对路径/代码围栏、Python 语法与 diff whitespace 检查通过。未 stage 语法草案，未推送或发布；M0/MVP 状态不变。

## Scheduler 有界让出样本：ADR-0072

基于 `b550546`，按 [ADR-0072](adr/0072-scheduler-yield-observation.md) 接入每 worker 可选固定 FIFO，默认 `yield_samples=0`；显式容量 1..=65536，在分配前向 tenant/VM 计费，启动失败释放已创建控制对象。真实 coroutine 返回后附完整 Pid/worker/序号，满则丢新样本并计数，不覆盖旧值。token 另保留 request/合并/消费/清除/确认计数，饱和置 overflow；关闭/步骤/effect/取消/预算不会伪造原子样本。

环境同上。最终 check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；debug/release workspace 各 478 项，六个示例合计各 13 项，总计各 491 项。新增 6 项覆盖 FIFO 满/循环复用/缺口/饱和、重复发布去重、token 准入失败回滚、容量 0/1/65536/越界和两个层级的部分启动失败回滚，以及双 worker timer 的完整身份、守恒与停止清理。早期显式 SchedulerConfig 初始化漏新默认字段、旧 observer 清除测试未更新持久计数期望、clippy 的范围索引告警均修正后重跑；未为测试改对象生命期或资源上限。

真实百万次 checked 加法的 v7 增加独立 `--observe`，保留 4096 steps/1ms、101×256 B、mailbox 1、255 冷 pending 加一个热请求、10/30ms 同刻突发，不改变 BEAM/默认运行。最终逐条计时输出及请求/清除/丢样保存在 `target/scheduler-yield-final-{debug,release}`；当前这些短时运行 timer 未触发、样本为零，所有分位为 null，不据此宣称让出延迟或公平性达标。Python 另覆盖有丢样/零样本模型和 19 份伪造历史，不把遗漏首条、假守恒或倒退时钟当完整。

完整 AOT 另接 `scheduler_yield`：原 4 KiB Bytes Hold、双 worker、1ms quantum、无限步骤片、一槽 FIFO，真实 timer 请求/受管检查点切回后才有样本，排满后核对 dropped、Pid/worker、取消与最终归零。这是边界回归，不是 v7 的采样条件。所有业务和 AOT 原限制均不变，没有通过修改公平性工作量/quantum 凑数据；完整长时公平性、开销/丢样、OS CPU 配额与原生双平台门禁继续待验收。

本机最终 debug/release 完整 AOT 均通过，日志 `/tmp/wings-scheduler-yield-final-aot-{debug,release}.log`；两种构建显式 v7 三端和独立观测均通过，日志 `/tmp/wings-scheduler-yield-final-three-{debug,release}.log`。默认 timer 不读诊断时钟或锁，但不可把“默认关闭”解读为本次宿主结构布局零变化；固定元数据仍按实际大小准入。

Linux 同原 2 vCPU/4 GiB QEMU TCG 交叉构建 release，31 个 harness 共 491 项通过。完整 AOT 两次均在旧 v2 多 actor 路径触发原 3 秒 deadline：首次与本机三端回归重叠，停止这些任务后的隔离重跑仍失败；日志 `/tmp/wings-scheduler-yield-linux-aot.log`、`/tmp/wings-scheduler-yield-linux-isolated-aot.log`。不能把本次 Linux 完整 AOT 记为通过，也没有提高限制、缩小业务或删用例。

为区分新改动与既有余量，取上次 `b550546` 验证包的现成 binary，同一客体/同一新编译业务产物、原 CPU 5 秒/墙钟 10 秒/阶段 3 秒比较三组完整大容量输入。10000×256 B 两版均 deadline；1000×4 KiB 旧版 configure 失败、新版通过；100×64 KiB 两版通过。日志 `/tmp/wings-scheduler-yield-linux-comparison.log`；这些是有界诊断，不是性能结果，也未证明仿真之外的根因。大容量启动/配置余量仍须处理，不能用旧版也失败豁免最终完整回归。

同一客体另运行新增真实 AOT `scheduler_yield`、v7 全部 Wings/模型及 `--observe` 和独立观测，均通过，日志 `/tmp/wings-scheduler-yield-linux-targeted.log`。启用观测的 10/30ms 分别保留 13/6 个真实 timer 样本、各 2 个清除、无丢样；它们只是仿真短测。默认 30ms 运行实际有 188 回复、12 超时、100 mailbox_full，全部接受请求仍收齐/state 核对成功，没有删掉失败到达或把其当性能通过。新增专项不替代上面的完整 AOT 失败；客体无 OTP/原生 debug/clippy/MSRV/doc-tests，测试后停止。

本机最终 v1–v6 显式三端 debug/release 全部通过；v6 仍仅模型/collector/配置，不执行 90 秒实验。依赖隔离、禁 unwind feature、89 份 Markdown（含未提交待审草案）相对路径/围栏、Python 语法、fmt/diff whitespace 检查通过。未推送或发布，语法未改；此切片保留 Linux 完整 AOT 未通过状态，MVP 仍未完成。

## 显式复用初始化栈池：ADR-0073

新增宿主 `SchedulerHandle::spawn_with_heap_in_pool` 与 `SchedulerSupervisor::spawn_with_heap_in_pool`，仅在同步 initializer 期间借用同 tenant 计费池，不存入跨线程 Scheduler 或 supervisor；默认 spawn 和自动重启仍走原临时池。v2 大容量 Counter 改用调用者一槽/缓存一槽池，池创建仍算入原 3 秒阶段，保留 10000×256 B、1000×4 KiB、100×64 KiB、100 活动 actor、32 窗口及原 CPU/墙钟限制。未修改 ABI v14 或待审语法。

三项新回归覆盖同池顺序创建独立 state、错误 tenant/未计费/忙池拒绝、process/tenant/VM 额度与 initializer 失败回滚，以及释放显式池后真实失败监督重启不持有池。专项通过；本机 workspace check、全目标 clippy `-D warnings`、fmt、Rust 1.85.0 全目标 check 通过，debug/release 各 481 项 workspace 加 13 项示例测试，共各 494 项通过。日志 `/tmp/wings-init-pool-{check,clippy,fmt,msrv,tests-debug,tests-release,examples-debug,examples-release}.log`。

完整本机 AOT 首轮在原 30 秒 `frame_limits` 上限超时，后两次在原 30 秒 timer 编译上限超时；原记录分别保留于 `/tmp/wings-init-pool-aot-debug-first-timeout.log`、`/tmp/wings-init-pool-aot-debug-second-timeout.log`、`/tmp/wings-init-pool-aot-debug-third-timeout.log`。release 首次在原 5 秒 TCP 服务启动等待失败，日志 `/tmp/wings-init-pool-aot-release-first-tcp-timeout.log`。显式 BEAM v1 首次也触发原 10 秒子进程上限，日志 `/tmp/wings-init-pool-verify_counter_equivalence-debug-first-timeout.log`。不据宿主负载诊断豁免这些失败，不提高时限。

Linux x86-64 release 交叉构建 bins、54 个示例和 31 个测试 harness 成功，传包核对当前改动源码且排除未批准语法草案。此次同配置 2 vCPU/4 GiB QEMU TCG 客体在 Lima 启动 10 分钟内未达到 running；后续 SSH 在 banner/kex 阶段失败，没有执行本轮客体测试或完整 AOT。日志 `/tmp/wings-init-pool-linux-{build,test-build,start,ssh-check,ssh-final}.log`，客体已强制停止并保留 `/tmp/wings-init-pool-linux-stop.log`。交叉构建不替代运行验证，前节 v2 的 Linux 3 秒失败仍未证明修复，也不构成原生 Linux 或性能门禁。

停止客体后，保留原代码/输入/断言和全部上限，macOS debug/release 完整 AOT 隔离重测均通过；两种构建另行 v2 Wings/OTP 29/独立 Python 各通过 18 个完整状态/FIFO/退出业务、17 个 bridge 拒绝和 49 个 JSON 拒绝。日志 `/tmp/wings-init-pool-aot-{debug,release}.log`、`/tmp/wings-init-pool-three-{debug,release}.log`。这些通过不抹去首轮失败，也没有证明 Linux 大容量启动已修复。

本机两种构建另行 v1/v3/v4/v5/v6/v7 显式 Wings/OTP 29/独立模型全部通过；v6 只核对长测协议/collector/配置，不执行 90 秒实验。日志 `/tmp/wings-init-pool-verify_{counter_equivalence,senders_counter,admission_counter,open_loop,measurement,fairness}-{debug,release}.log`。v7 原百万次加法与心跳工作量不变，短测 `--observe` 不据零数据声称让出延迟达标。

待本机回归全部结束后，同配置客体第二次启动成功，核对四份运行时改动源码 SHA-256 与本机一致；31 个 release harness 共 494 项及完整原限 AOT 全部通过，包含原三组大容量 v2，没有删输入、增加时限或重试业务。日志 `/tmp/wings-init-pool-linux-isolated-start.log`、`/tmp/wings-init-pool-linux-tests.log`、`/tmp/wings-init-pool-linux-aot.log`。客体仍无 OTP/原生 debug/clippy/MSRV/doc-tests，不替代原生 Linux 门禁。

另外取历史 `b550546` 验证包的现成 binary，用同客体/同一新编译业务产物/原 CPU 5 秒、墙钟 10 秒和阶段 3 秒比较。首次诊断在旧版 10000×256 B 返回 deadline 后停止，保留 `/tmp/wings-init-pool-linux-comparison-baseline-deadline.log`；后来逐组完整比较两版三组均通过，见 `/tmp/wings-init-pool-linux-comparison.log`。此前诊断脚本误把 JSON 发给二进制 bridge，两个 invalid 只属 harness 失误，不是有效业务或性能证据，见 `/tmp/wings-init-pool-linux-comparison-harness-error.log`。因此本次完整回归通过，但不能证明旧 deadline 的唯一根因或优化收益，更不能宣称 BEAM 领先。测试结束正常停止客体，见 `/tmp/wings-init-pool-linux-isolated-stop.log`。

独立开环/fairness/让出计数模型与 90000 到达长测 collector 模型通过，日志 `/tmp/wings-init-pool-models.log`；这些模型不替代真实三端执行或长时公平性。依赖隔离、禁 unwind feature、90 份 Markdown 路径/围栏、Python 语法和 fmt/diff 检查通过。不推送或发布，MVP 未完成。

## 有界并发请求与监督恢复：ADR-0074

增加 `examples/concurrent-recovery.wings`、`concurrent_recovery` 宿主示例与回复表固定屏障竞争单测。分别一/双 worker、两 actor、完整 4 KiB Bytes、两发送者各八条，核对超时/取消后的 16 条仍占 mailbox/请求额度、17 槽共享结果的健康余量、满队列无净预留、stop/取消只取一个合法终态；旧 Pid 拒绝、队列不重放。Fail 替换候选并 collect 后除零，即使取消宿主结果也须有真实失败退出，快照长度/32 B 前缀保持旧提交零值，一次新 heap/state 重启后强度超限，最终 VM 零。每配置仍共享原 3 秒阶段，完整 AOT 仍 CPU 20 秒/墙钟 30 秒/禁 core，ABI v14/语法/执行路径未改。

首次专项 fixture 在 Hold 活栈期间错误调用 idle Bytes snapshot，按原查询契约失败；测试改为断言该查询拒绝，不放宽运行时边界，日志 `/tmp/wings-recovery-aot-specific-active-snapshot-error.log`。修正后的 fixture 四次专项与增加 collect 后的四次专项均通过，见 `/tmp/wings-recovery-aot-specific.log`、`/tmp/wings-recovery-aot-specific-final.log`。独立回复表单测覆盖零 deadline 与非过期、publish/drop 两种情况各 16 次屏障竞争，检查单一终态、不可重复消费/取消、publisher 清理和下一代槽无旧值。

本机 check、全目标 clippy `-D warnings`、fmt、Rust 1.85.0 全目标 check 通过。macOS debug/release 各 workspace 482 项加示例 13 项共 495 项，两种构建完整原限 AOT 均通过；日志 `/tmp/wings-recovery-{check,clippy,fmt,msrv,tests-debug,tests-release,examples-debug,examples-release,aot-debug,aot-release}.log`。Linux 验证另记。依赖隔离、禁 unwind feature、91 份 Markdown（含未提交待审语法稿）路径/围栏、Python 语法与 diff 检查通过。本片不是 BEAM 对照，不据短时并发回归宣布完整故障、公平性、性能或 MVP 达标。

Linux 同配置隔离仿真交叉构建 55 个示例及 31 个 release harness，传包核对本片四份代码/脚本与当前工作区一致，排除待审语法稿。客体 31 个 harness 共 495 项及完整原限 AOT 均通过，包含新增真实 AOT 恢复和旧 v2 大容量；日志 `/tmp/wings-recovery-linux-{build,test-build,start,tests,aot}.log`。测试结束正常停止，见 `/tmp/wings-recovery-linux-stop.log`。客体无 OTP/原生 debug/clippy/MSRV/doc-tests，仍非原生 Linux、双平台性能或完整故障门禁。

## 分口径平台内存观测：ADR-0075

基于 `22f44b3`，在原 v6 长测逐秒采样点接入冻结的 C11 helper。Linux 原生读取 smaps_rollup 的 RSS/PSS/private/shared/swap，macOS 使用 SDK 的 resident/physical footprint；主机压缩/swap 单列，不归因给 VM。记录每 PID/start_id、前后 census、时间窗及 helper CPU/max RSS/wall；缺失、拓扑或身份变化保留原始错误，完整性为 false、平台总数为 null。原 1 GiB RSS 安全停止保留监测辅助成本，不是 OS 硬配额或真实峰值。ABI v14、业务、容量、路由、deadline、30 秒预热/60 秒采样/五次配对和所有原限不变。

### 有界回归

本机 check、全目标 clippy `-D warnings`、fmt、Rust 1.85.0 全目标 check 通过；debug/release 各 workspace 482 项加示例 13 项共 495 项，完整原限 AOT 两种构建均通过。最终版本再跑完整 AOT 及显式 BEAM 测量配置验证，两种构建均通过；有界模型覆盖非法/重复字段、缺失值、身份与历史容量、拓扑变化、null 与零的区别，真实父/子/孙进程验证原生字段、helper 成本与清理。日志 `/tmp/wings-memory-{check,clippy,fmt,msrv,tests-debug,tests-release,examples-debug,examples-release}.log`、`/tmp/wings-memory-final-{aot-debug,aot-release,measure-debug,measure-release,guards}.log`。首次 C 编译因 POSIX feature macro 隐藏 Darwin SDK 类型失败，补 `_DARWIN_C_SOURCE` 后通过，不手写结构布局。

Linux 沿用 2 vCPU/4 GiB、Ubuntu 24.04 x86-64 QEMU TCG 隔离客体；客体 GCC 原生编译 C helper 并通过真实 Linux PSS/后代/错误验证，未仅以交叉编译充数。Rust 源码未改，复用 `22f44b3` 的 31 个 release harness，共 495 项及接入新采样验证的完整原限 AOT 全部通过，见 `/tmp/wings-memory-linux-tests.log`、`/tmp/wings-memory-linux-aot.log`。首次增量传包发现客体 `/tmp` 随重启清空，恢复原完整包再叠增量；这是 harness 准备失败，不是业务回归或放宽时限。测试后正常停止客体，见 `/tmp/wings-memory-linux-stop.log`；仍无原生 Wings Linux debug/clippy/MSRV/doc-tests 或客体 OTP，不能替代原生双平台门禁。

### 长时集成

正式 `mixed-4k` 在 macOS M3 Pro 上完成原 1000×4 KiB、100 活跃、mailbox 1、256 pending、100ms deadline、1000 到达/秒的五对 30 秒预热/60 秒采样；每端合计 300000 个采样段到达。十轮都正常退出、最终状态和 drain 通过。Wings 合计 194 次超时/89 次拒绝，BEAM 223 次超时/44 次拒绝，两端各仅一轮通过候选尾延迟，不筛选最好的一轮。原始目录 `target/open-loop-memory-mixed-v6`，日志 `/tmp/wings-memory-long-mixed.log`；十份 trace 用当时冻结 verifier 再独立复核，50 份原始文件与所有冻结输入 SHA-256 一致，见 `/tmp/wings-memory-evidence-audit.log` 和 [JSON 摘要](open-loop-memory-mixed-macos-2026-11-14.json)。业务产物复用旧冻结 ABI v14 Counter，不把它写成本片新编译产物。

平台采样 Wings 442/442 完整，BEAM 442/443 完整；BEAM 首轮末尾进程消失保留 errno 3，平台总数为 null，即使 census 拓扑一致也不算完整。包括初始化/预热/采样的各轮 observed footprint peak：Wings 45.03–55.36 MiB、BEAM 68.77–75.27 MiB；RSS 另列，不混加，均包含驱动和捕获的后代，不是真峰值或整机物理页去重。host compressor resident 约 10.38–10.87 GB、swap used 约 2.098 GB 是主机背景值，不是 VM 成本。helper peak 至多约 1.41 MB，十轮 helper user+system CPU 合计约 2.01 秒；单次 probe wall 最大约 30.1ms、census 最大约 1.515 秒，监测偏差未验收，不能据较低 footprint 宣布性能/整机内存双门禁通过。

首次将驱动复制到临时目录遗漏 verifier 依赖，导入阶段即失败，没有启动业务，保留 `/tmp/wings-memory-long-driver-import-error.log`；正式运行只复制驱动/采样模块并显式设置仓库 scripts 的 PYTHONPATH，源文件 hash 冻结，不在运行中改采样代码。长测前已停止客体及构建/测试，但主机其他活动未隔离，仍无 CPU 硬配额、热状态/电源/QoS/核分配记录，不把这十轮当作受控性能验收。

长测后复核 SDK 注释，发现初版 `swapped_logical_bytes` 错把 swapped_count 命名成解压后逻辑字节。最终 helper 改名 `compressor_swapped_page_bytes`，含义为已换出 compressor 页数×页大小，不等于解压后逻辑量或磁盘 swap used；补独立模型拒绝旧字段/区分三种量。已冻结的长测 raw/source/hash 不改，JSON 明确解释历史字段，不能用最终 helper 反写该实验。不将有界 OS 测试或字段存在写成完整内存验收。

改名后的本机 check/clippy/fmt/MSRV 与完整原限 debug/release AOT 全部通过，见 `/tmp/wings-memory-post-{check,clippy,fmt,msrv,aot-debug,aot-release}.log`；专项及显式 BEAM 测量配置两种构建通过，见 `/tmp/wings-memory-semantic-{specific,measure-debug,measure-release}.log`。最终五份脚本增量传入新启动客体，GCC 原生 helper 和完整原限 release AOT 再次通过，正常停止客体，见 `/tmp/wings-memory-final-linux-{start,aot,stop}.log`。92 份 Markdown 路径/围栏、Python AST、依赖隔离、禁 unwind 与 diff 空白检查通过；语法草案仅审阅，仍不在本片提交。

## 计费 Bytes 消息：ADR-0076

基于 `3f5fce8`，按 [ADR-0076](adr/0076-accounted-byte-messages.md) 接通不可变 Bytes 的语言自发送、目标发送和异步请求参数。沿用原语法和大小写，待审语法草案不参与实现或提交。源码 ABI 升至 v15，EffectRequest 为 72 B；每条消息的 Bytes 合计至多 65536 B，三个引用 mask 与 schema 精确匹配。回复与宿主原始整数入口仍拒绝引用句柄。

发送方先向 process→tenant→VM 预留独立 staging 容量，再 fallible 分配；持目标唯一发布权时转移票据并准入 envelope。接收方导入自己的精确堆，导入期间两份字节均计费；每至多 256 B 经过原预算、取消和让出检查。混合参数逐个进入入口根，后续导入 GC 保留前参。控制字段增加后，单 worker 与 Scheduler 的保守 invocation 预留统一为 4352 B，完整计入控制类型估算，资源上限不变。启动准备失败保留原队列载荷，完成准备后才转移到活栈。

本机环境同上。workspace debug/release 各 490 项、六个示例各 13 项，总计各 503 项通过；新增 8 项覆盖规范分段、mask/边界、三级准入、跨账户净转移及失败回滚、源副本独立、混合导入 GC、队满/停止清理和启动失败后重试。`byte_messages` 真实 AOT 包含空/1 B/64 KiB、自发送/子进程/请求、混合记录/列表/多个 Bytes、共享源的独立副本、65537/65538 B 拒绝、目标堆耗尽、源失败后已接受发送保留及单/双 worker；128 个有界取消位置实际命中 source staging 和 target 双份导入计费，退出账目归零。最后强化故障分类，分别核对 DivisionByZero、SendRejected、HeapExhausted 及失败前已提交状态；原限 debug/release 专项通过，见 `/tmp/wings-byte-messages-fault-{debug,release}.log`。

本机 check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过；原限 debug/release 完整 AOT 与显式 v1–v7 Wings/BEAM/Python 全部通过，日志 `/tmp/wings-byte-messages-aot-final-{debug,release}.log`、`/tmp/wings-byte-messages-verify_*-{debug,release}.log`。最后的故障断言与失败上下文补充后，再次 check/clippy/fmt/MSRV 及两种构建的完整原限 AOT 全部通过，见 `/tmp/wings-byte-messages-close-{check,clippy,fmt,msrv,aot-debug,aot-release}.log`。依赖隔离、禁 unwind feature、Python AST、93 份 Markdown 相对路径/围栏和 diff 空白检查通过。这些仍是功能/配置验证，不是性能验收。

Linux 沿用 2 vCPU/4 GiB、Ubuntu 24.04 x86-64 QEMU TCG 隔离客体，交叉构建全部工具/示例和 81 个 release harness，共 503 项通过，见 `/tmp/wings-byte-messages-linux-tests.log`。首次完整 AOT 在既有 v5 开环业务报 `open-loop stage deadline`；当时与宿主短时构建重叠，原报错没有场景上下文，不能断言具体负载或唯一原因。保留失败日志 `/tmp/wings-byte-messages-linux-aot.log`。客体重启后先在无并行重型工作时逐项输出调用场景，六组原规模/原限及十项配置拒绝均通过，见 `/tmp/wings-byte-messages-linux-open-loop-diagnose.log`，没有重现首次超时。为后续故障增加 verifier 的 runtime/负载/时长/到达率上下文，未修改业务、池、队列或时限，未将一次成功归因为未实施的优化。

更新最后的故障断言 fixture 后，核对 296 份源码及新 fixture binary SHA-256，在原 CPU 20 秒/墙钟 30 秒/禁 core 的 AOT 子进程及业务 CPU 5 秒/墙钟 10 秒/阶段 3 秒内，从头完整 release AOT 通过，见 `/tmp/wings-byte-messages-linux-final-{hashes,aot}.log`；测试后正常停止客体，见 `/tmp/wings-byte-messages-linux-final-stop.log`。客体仍无 OTP、原生 debug/clippy/MSRV/doc-tests，不替代原生 Linux 或双平台门禁。现有 Bytes 消息切片也不代表字符串、通用记录、引用回复、完整账本或 MVP 已完成。

## 连续 CPU 与突发心跳长测：ADR-0077

基于 `ab520c0`，按 [ADR-0077](adr/0077-streamed-fairness-measurement.md) 增加独立 v8。未修改 runtime、ABI v15、语言或待审语法；原 v7 的 10/30ms、百万次 checked 加法、101×256 B、mailbox 1、255+1 实际 pending 和全部短测/AOT 时限保持。长测连续预热 30s/采样 60s，共 900000 次计划心跳；最后 burst 在 89.99 秒，CPU 仍续发至 90 秒窗口结束，再收齐结果、核对全部状态并退出。`measurement_elapsed_ns` 独立证明完整窗口，没有将循环短测或 89.99 秒提前退出写成 90 秒。

运行端固定 64 KiB buffer 写 40 B 事件，原始接受/拒绝/实际完成与热任务逐条保留；独立重放检查完整计划、每目标 FIFO/值、实际 pending、CPU 完整返回和最终提交/清理。全局及每 actor 分列延迟、超时、拒绝和最大等待，单 actor 采样段 6000 条到达，p999 为 null。新增有界模型覆盖 20 份伪造历史、缺失/截断、255 在途超时保留、CPU 提前结束、trace 独占创建、严格 CLI 及复用采样 collector；模型不冒充长测。

早期模型用普通 `invoke` 检查已存在 trace，因其主动拒绝 stderr 而先报 harness 错误；改用已有有界命令路径断言真实 exit 3 和原文件不变，没有放宽被测行为。审计 token 的既有 `later_request_survives_confirmation_and_poisoned_lock_recovers` 回归时，发现短/长 verifier 错将两次请求到确认的区间重叠判为非法；修正为请求时间各自单调、确认时间各自单调、每对请求不晚于确认，保留逆序/越界拒绝，并新增合法重叠模型。没有改变 token、请求/消费/确认计数或丢样契约。

### 本机与隔离功能验证

macOS 环境同上，debug/release workspace/all-targets 各 503 项通过；check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 通过。原限完整 debug AOT 通过，初次 release 在既有 TCP overflow 场景读首条回复时遇到 `ConnectionResetError`，保留 `/tmp/wings-fairness-long-aot-release.log`。为该错误补服务端退出/完整报告诊断，未更改 100ms I/O、请求或业务边界；无并行重型任务后完整两种构建均通过，见 `/tmp/wings-fairness-long-{window,final}-aot-{debug,release}.log`。首次原因没有独立证实，不归因于未实施的优化，也不据成功重跑抹掉失败。显式 v7、v8 有界模型与真实 BEAM 两种构建通过；v6 collector 显式三端回归通过。

Linux 仍为 Ubuntu 24.04 x86-64、2 vCPU/4 GiB、QEMU TCG 隔离客体。交叉构建并冻结 299 份源码、81 个 release harness 和 59 份工具/示例，SHA-256 一致；503 项及包含新模型的完整原限 AOT 通过，见 `/tmp/wings-fairness-long-linux-{tests,aot}.log`。随后正常停止客体再做本机长测，见 `/tmp/wings-fairness-long-linux-stop.log`；客体没有 OTP 或原生 Rust debug/clippy/MSRV/doc-tests，不替代原生 Linux 门禁。重叠验证及结果比例修正后，重新核对 300 份源码/证据和 59 份 binary hash，再完整原限 AOT 通过并停止客体，见 `/tmp/wings-fairness-long-linux-final-{hashes,aot,stop}.log`；Rust binary 与原 503 项测试时一致，没有用旧 binary 代替修改后的 Rust。

### 默认模式五对 30s/60s

在 macOS M3 Pro、OTP 29/ERTS 17.1.1 JIT、release 下交替顺序完成五对；当时已停止客体及构建/测试任务，但主机未隔离、没有 CPU 硬配额，电源/热状态/QoS/核分配仍缺失。原始目录 `target/fairness-long-v8`，日志 `/tmp/wings-fairness-long-paired.log`，数据及全部原始文件 SHA-256 见 [JSON 摘要](fairness-long-macos-2026-11-14.json)。两端各 3000000 次采样到达；十轮全部收齐真实结果、最终状态与资源清理通过。

Wings 共 2999709 次按时回复、291 次 mailbox 满、无超时或 pending_full，尾延迟候选 4/5；BEAM 共 3000000 次按时回复、无拒绝/超时，候选 5/5。Wings 第三轮的 291 次拒绝不重试、不删样；拒绝发生时生成迟到范围 0.03175–5.619208ms，尚不能独立确定原因。每 actor 的结果仍在原始 JSON，拒绝组及最大等待在摘要保留，没有用全局 p99 较低掩盖拒绝。热任务采样窗内真实完整百万次加法的完成率：Wings 266.07–277.25 次/秒，BEAM 86.07–88.12 次/秒；它是本次负载和未受控环境下的观测，不是最大持续吞吐或双平台结论。

所有平台内存样本读取完整；各轮 driver 加捕获后代的 observed physical footprint peak：Wings 19661736–57705528 B、BEAM 72041912–95258208 B。RSS、footprint 和主机 compressor/swap 分列，不能混加。驱动在后续轮次还可能含离线重放的 allocator 保留；未独立证明增长归因或观察器偏差，不将范围写成纯 VM 内存，更不能以该结果宣布整机内存门禁通过。

### 独立让出诊断与证据复核

默认五对均关闭观测。另冻结一份带重叠验证修正的源码/同一 runner 与产物，单独运行 Wings `--observe` 的 30s/60s；它不属于五对默认性能样本，也不宣称五次观测重复。实际 requests 3、consumed/confirmed/published/taken 各 2、cleared 1、coalesced/dropped/retained 各 0，均守恒；两条样本都在预热，采样窗口无数据，p50/p99/p999 为 null。原 4096 steps/1ms quantum 不调整来凑样本；FIFO 完整不等于让出延迟门禁通过。见 `/tmp/wings-fairness-long-observed.log` 和原始 `observed-wings.*`。

十份默认 trace 用当时冻结 verifier、单份诊断用独立冻结的修正 verifier 全部重新重放，两个完整输入 hash 集及 58 份原始文件 hash 核对一致，见 `/tmp/wings-fairness-long-evidence-audit.log`。原冻结文件和旧结果不回写；重叠修正不影响关闭观测的默认数据。MVP、公平性、CPU 硬配额、过载分组、原生双平台及同负载吞吐/整机内存双门禁均未据此通过。

最终源码本机 debug/release all-targets 各 503 项及完整原限 AOT 再通过，见 `/tmp/wings-fairness-long-close-{tests,aot}-{debug,release}.log`。当前 verifier 再重放一份默认 Wings、一份默认 BEAM 和独立观测，核对原结果及补充拒绝/超时比例、按时回复率；58 个原始文件与证据 JSON hash 一致，见 `/tmp/wings-fairness-long-current-replay.log`。check/clippy/fmt/MSRV、依赖隔离、禁止 unwind feature、Python AST、94 份 Markdown 路径/围栏和 diff 空白检查通过。语法评审草案继续未采纳、不 stage；未推送或发布。

## 请求发布端生命周期：ADR-0078

基于 `3ab76f3`，按 [ADR-0078](adr/0078-request-publisher-lifetimes.md) 修正内部请求提前回收。此前 ScalarWorker 宿主取走超时/取消结果、语言完成事件入队或发起方退出，会释放仍排队/执行请求的槽和 requests 票据；代次虽避免串回包，仍可反复超时绕过请求容量。新规则只在结果端与消息发布端都结束后回收。事件可先投递、目标副作用继续，ABI v15、现行类型拼写、容量及原时限不变。

### 复现与修正

新增单槽测试先在旧实现运行，`consumed_timeout_retains_request_capacity_until_target_message_finishes` 实际失败：取走零超时结果后 requests 为 0，预期 1；记录在 `/tmp/wings-request-lifetimes-before.log`。修复后目标仍排队/挂起时保留原账户票据，反复请求拒绝且队列/账目零变化；正常完成/stop 后才释放并推进代次。结果只消费一次，旧 ID 的再次消费/取消拒绝，不等待目标完成才报告超时。

- 单 worker 保留独立结果端/发布端标志；取消只选择终态，不能结束发布端。三级 requests 单额度、固定单槽分别验证背压；超时/取消、正常完成/stop 两种清理及最终归零均覆盖。
- 语言事件仍使用原预留直接入 mailbox；路由移出后不保留 HandlerId/事件票据，请求票据等到目标消息结束。发起方退出清理事件，目标继续排队，旧进程槽复用不接旧事件；自请求清理一次结束两端。
- Scheduler 固定表核对事件队满/starting 保留、事件先投递但 publisher 未结束、终态不覆盖、已入队能力取消拒绝及代次耗尽。另一组实际一/双 worker 测试用目标进程字节额度暂时阻止 start：超时或取消事件已投递，目标仍排队，请求表继续拒绝；发起方退出后仍保留票据，目标恢复/stop 后才归零。每阶段仍限 3 秒，不在 handler 内增加阻塞屏障。
- 真实 AOT `scalar_requests` 检查排队及活栈超时/取消后的背压、旧 ID 拒绝、迟到消息继续提交 state 与完整回收。`request_events` 新增四组事件已入队后的发起方/目标 stop 或正常推进，核对剩余请求/消息额度及最终状态。现有多 worker、并发恢复、消息复制、GC、监督及 Drop 回归继续执行。

### macOS AArch64

Rust 1.99.1；workspace check、all-target clippy `-D warnings`、fmt、Rust 1.85.0 all-target check、依赖隔离与禁止 unwind feature 全部通过。debug/release all-targets 各 81 个 harness、508 项测试通过；两种构建的 doc-tests 和完整受限 AOT 均通过。新增 5 项测试，未放宽已有断言或额度；原复用测试改为目标消息结束后才准入下一请求。日志见 `/tmp/wings-request-lifetimes-final-{clippy,msrv,fmt,tests-debug}.log`、`/tmp/wings-request-lifetimes-tests-release.log`、`/tmp/wings-request-lifetimes-doctests-{debug,release}.log` 和 `/tmp/wings-request-lifetimes-aot-{debug,release}.log`。

debug/release 各显式执行真实 Wings/BEAM/Python v1、v2 正确性对照：v1 45 组业务、28 JSON/14 桥接拒绝，v2 18 组业务、49 JSON/17 桥接拒绝全部通过。业务保留 CPU 5 秒/墙钟 10 秒、原 3 秒阶段；只验证业务与清理，没有重跑 30s/60s 五对测量，不声称吞吐或内存改进。见 `/tmp/wings-request-lifetimes-beam-v{1,2}-{debug,release}.log`。

### Linux x86-64 隔离仿真

沿用 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载；macOS 交叉构建 release，客体 GCC 13.3 链接并执行真实 AOT `.so`。冻结包 `/tmp/wings-request-lifetimes-linux.tar.gz` 含 301 份源码、81 个 harness 和 59 个工具/示例，不含待审语法稿；客体执行前以及完整 AOT 后复核全部 441 项 hash。81 个独立测试 OS 子进程共 508 项全部通过，完整受限 release AOT 通过；测试/AOT 均保持 CPU 20 秒/墙钟 30 秒/禁 core，业务阶段与原配额不变。日志见 `/tmp/wings-request-lifetimes-linux-{tests,aot,hashes}.log`，本节为随后补写的验证记录。

交叉 linker 保留 `ignoring deprecated linker optimization setting '1'` 工具链警告，没有为通过测试改变 linker 参数。没有原生 Linux debug/clippy/MSRV/doc-tests 或真实 Linux BEAM 数据；TCG 功能回归不替代原生双平台验收。验证后停止客体，见 `/tmp/wings-request-lifetimes-linux-stop.log`。

语法评审稿继续未采纳、未 stage；未推送或发布。完整业务值、全部资源账本、公平性、CPU 硬配额和同负载 BEAM 吞吐/整机内存门禁仍待完成，MVP 状态不变。

## VM 根与准入门闩启动计费：ADR-0079

基于 `59b82e2`，按 [ADR-0079](adr/0079-accounted-vm-bootstrap.md) 补根账户与共享准入门闩的逻辑存储。`vm_accounted` 在两次 Arc 分配前检查根/门闩及四个强弱引用计数字段的成本，used 从启动成本开始；不创建指向根自身的 Reservation。计费根下旧 `tenant`/`process` 入口也强制先预留子节点元数据。旧 `vm` 工具树仍保留零基线；没有声称所有入口或 allocator/动态装载器内部成本已闭环。

### 功能与边界

- 新增 4 项根账户测试：零/不足/精确字节上限、其他维度为零可启动、精确成本/clone 不重复计费、旧子路径不能绕过父/祖先元数据额度、非法层级零变化、满 VM 净转移与跨根拒绝。弱引用检查跨线程最后票据释放后根/门闩都析构，没有自引用环；原并发节点竞争另加计费根模式，8 线程只准入可容纳的 4 节点，结束回到启动基线。
- `vm_bootstrap` 已接完整 AOT：完整计费根下模块/worker 满额度拒绝零变化、标量活栈超时后继续/stop、Bytes 正常状态与失败旧提交快照、一/双 worker 关闭后未消费结果保留原账户及最终启动基线。原 3 秒阶段与 AOT CPU 20 秒/墙钟 30 秒/禁 core 保留。
- CLI 堆模式和 TCP 服务改用计费根；TCP 所有既有 normal/错误/断开/超时/失败场景均校验正的 `bootstrap_bytes` 与相等的 `remaining_bytes`，`resources_released` 表示派生业务预留已归还。本机另跑一帧 `+7`，实际报告 `bootstrap_bytes=168`、`remaining_bytes=168`、committed=7、published/replies 各 1、其他失败计数 0；记录在 `/tmp/wings-bootstrap-tcp-receipt.log`。该值是本机 Rust 类型的逻辑成本，不是 RSS、allocator 开销或跨平台常量。

### macOS AArch64

Rust 1.99.1；workspace check、all-target clippy `-D warnings`、fmt、Rust 1.85.0 all-target check、依赖隔离/禁止 unwind feature、Python AST、95 份 Markdown 路径与代码围栏检查通过。debug/release all-targets 各 82 个 harness、512 项测试通过，两种构建的 doc-tests 通过；最终精确根成本断言的 release 资源测试另行通过。日志见 `/tmp/wings-bootstrap-final-{check,clippy,msrv,fmt,tests-debug,resources-release}.log`、`/tmp/wings-bootstrap-tests-release.log`、`/tmp/wings-bootstrap-doctests-{debug,release}.log` 和 `/tmp/wings-bootstrap-static.log`。

release 完整受限 AOT 通过，见 `/tmp/wings-bootstrap-aot-release.log`。首次 debug 完整 AOT 中新启动计费回归通过，随后既有 Counter v1 正确性子进程触及原墙钟 10 秒上限；失败日志 `/tmp/wings-bootstrap-aot-debug.log` 完整保留。当时还有 release 与 Linux TCG 验证并行，但未证明超时的唯一原因，不归因于 CPU 竞争，也不改变业务或时限。

其他验证结束、停止 Linux 客体后，debug 完整 AOT 保持原参数首次复跑通过，见 `/tmp/wings-bootstrap-final-aot-debug.log`；没有扩大 timeout 或重写失败日志。两种构建都验证原 TCP 全场景的启动基线字段；新 `vm_bootstrap` 回归同时在本机 debug/release 和 Linux release 执行。

debug/release 各显式执行真实 Wings/BEAM/Python v1、v2 正确性对照：v1 45 组业务、28 JSON/14 桥接拒绝，v2 18 组业务、49 JSON/17 桥接拒绝全部通过。保留业务 CPU 5 秒/墙钟 10 秒与原 3 秒阶段，未重跑长时五对或据启动账目宣称性能/内存提升。见 `/tmp/wings-bootstrap-beam-v{1,2}-{debug,release}.log`。

### Linux x86-64 隔离仿真

沿用 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载；macOS 交叉构建 release，客体 GCC 13.3 链接/执行真实 AOT `.so`。冻结包 `/tmp/wings-bootstrap-linux.tar.gz` 含 303 份源码、82 个 harness、60 个工具/示例，不含待审语法稿。执行前及完整 AOT 后全部 445 项 hash 一致；82 个测试子进程共 512 项通过，完整受限 release AOT 通过，包含新增启动计费/CLI/TCP 回归。CPU 20 秒/墙钟 30 秒/禁 core、Counter 业务 CPU 5 秒/墙钟 10 秒与原 3 秒阶段未变。见 `/tmp/wings-bootstrap-linux-{validation,hashes}.log`；本节为测试后补写的验证记录。

交叉构建保留既有 `ignoring deprecated linker optimization setting '1'` 工具链警告；未更改 linker 参数来消除日志。验证结束已停止客体，见 `/tmp/wings-bootstrap-linux-stop.log`。没有新增原生 Linux CI、debug/clippy/MSRV/doc-tests 或 Linux BEAM 数据；TCG 功能回归仍不能替代原生双平台或性能/内存验收。

没有改语言类型拼写或 ABI v15，没有重新发布包或回写冻结的长测结果。完整业务值、其余资源入口、公平性、CPU 硬配额和同负载 BEAM 吞吐/整机内存门禁仍未完成，MVP 状态不变。

## 初始化与执行核心统一控制准入：ADR-0080

基于 `4165829`，按 [ADR-0080](adr/0080-central-execution-admission.md) 将原 4352 字节控制预留移入 initializer 和 Invocation。直接同步/受控/owned task 与 worker 使用相同准入，单/多 worker 删除外层重复票据。显式标量初始化保留池，后续默认调用继续使用该池。内部默认 token 延迟到控制准入和借栈成功后创建；宿主传入的 token 仍由宿主拥有。没有改变 ABI v15、语言拼写、控制上界或业务预算。

### 缺陷复现与边界回归

新增直接执行回归先在旧实现实际失败：process 活栈只计 278528 B，预期栈加控制为 282880 B，差额恰为 4352 B。失败保留在 `/tmp/wings-execution-admission-before.log`；该数字是本机逻辑预留，不是 RSS。

新增 4 项执行边界测试，覆盖三级各仅剩 4351 B 时初始化/同步/task 拒绝、栈与账户零变化、默认调用保留显式池、ready/挂起/Drop/完成清理，以及可信 Rust ABI fixture 在初始化/同步入口内部观察真实控制票据。该 fixture 不冒充生成代码证据。三级拒绝与池满时延迟工厂调用次数为零，成功后恰一次。`accounted_stacks` 另用真实编译产物核对三级拒绝、默认 task、活栈与控制守恒及最后回到计费根基线。

两次完整 debug AOT 首先暴露 `scalar_worker`、随后 `worker_timer` 旧 fixture 只保留 actor 加栈、却期待初始化成功的假设。原失败日志 `/tmp/wings-execution-admission-aot-debug.log` 和 `/tmp/wings-execution-admission-aot-debug-rerun.log` 保留。两处及原单测均先保留旧小额度并验证初始化拒绝/零变化，再构造含原控制容量的精确初始化正边界；入队后仍因消息占用拒绝 handler，队首、旧状态、请求未完成和 timer 失败清理继续验证。满 VM 栈净转移用减少填充票据的方式留出控制容量，并新增完全满额拒绝断言，没有提高父级上限、栈池容量、mailbox/request 容量或超时。

### macOS AArch64

Rust 1.99.1；workspace check、all-target clippy `-D warnings`、fmt 和 Rust 1.85.0 all-target check 通过。最终 debug/release all-targets 各 82 个 harness、516 项测试通过，两种构建 doc-tests 通过。日志为 `/tmp/wings-execution-admission-close-{check,clippy,msrv,fmt,tests-debug}.log`、`/tmp/wings-execution-admission-tests-release.log` 和 `/tmp/wings-execution-admission-doctests-{debug,release}.log`。

最终 debug/release 完整受限 AOT 均通过，包含新直接计费回归、原单/多 worker、消息/请求/GC/监督、CLI 堆/TCP 全场景。内部默认 token 的延迟创建修正后重新执行完整两种构建；最终日志为 `/tmp/wings-execution-admission-controls-aot-debug.log` 和 `/tmp/wings-execution-admission-aot-release.log`。每个 AOT 子进程 CPU 20 秒/墙钟 30 秒/禁 core，业务 CPU 5 秒/墙钟 10 秒、原 3 秒阶段均不变。

debug/release 各显式执行真实 Wings/BEAM/Python v1、v2 正确性回归：v1 的 45 组业务、28 JSON/14 桥接拒绝和 v2 的 18 组业务、49 JSON/17 桥接拒绝全部通过，见 `/tmp/wings-execution-admission-beam-v{1,2}-{debug,release}.log`。没有重跑五对长测或据逻辑控制账目推断性能/内存改进。

依赖隔离、禁止协程 unwind feature、Python AST、97 份 Markdown 的 943 条相对文件路径和代码围栏、diff 空白检查通过，见 `/tmp/wings-execution-admission-static-final.log` 与 `/tmp/wings-execution-admission-docs-final.log`。语法评审稿仍未采纳、未 stage。

### Linux x86-64 隔离仿真

沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 客体，macOS 交叉构建 release；客体 GCC 13.3 链接/执行真实 AOT。冻结包 `/tmp/wings-execution-admission-linux.tar.gz` 含 304 份源码、82 个 harness 和 60 个工具/示例，排除待审语法稿。验证前核对并复用编译工具的绝对路径别名；执行前及完整 AOT 后全部 446 项 hash 一致，全部实现源码与宿主最终工作区一致，验证记录在测试后补写。82 个有独立资源限制的测试子进程共 516 项通过，完整原限 release AOT 通过，见 `/tmp/wings-execution-admission-linux-tests-verified-aliases.log`、`/tmp/wings-execution-admission-linux-aot.log`、`/tmp/wings-execution-admission-linux-hashes.log` 和 `/tmp/wings-execution-admission-linux-host-sources.log`。

保留既有交叉 linker 的 `ignoring deprecated linker optimization setting '1'` 警告，没有更改工具链参数来消除日志；客体测试后停止。没有新增原生 Linux CI、debug/clippy/MSRV/doc-tests 或 Linux BEAM 数据，TCG 功能验证不替代原生双平台或性能/内存门禁。旧未计费工具入口、CLI 适配层、完整业务值/账本、公平性及 BEAM 双门禁仍需完成，MVP 状态不变。

## CLI 默认标量路径计费：ADR-0081

基于 `e43aced`，按 [ADR-0081](adr/0081-accounted-default-cli.md) 将 `wings-vm run --trusted` 默认模式接入与堆模式相同的计费根、子账户、模块加载及单槽零缓存栈池。默认标量由未计费改为原堆模式的 16 MiB 三级逻辑字节上限，堆模式的额度不变；CLI 帮助同步说明两种模式。JSON 协议、ABI v15、语言拼写、总步数与检查点不变。

新增完整 AOT 内的加载前拒绝回归：用 16 MiB 稀疏伪库和坏清单复现旧默认路径先报 `expected ident`，见 `/tmp/wings-cli-accounting-before.log`；修复后默认/堆模式都先报 `resource_exhausted at Vm` 且 stdout 为空，见 `/tmp/wings-cli-accounting-focused-debug.log`。原库大小上限仍为 64 MiB，原模块保守预算为文件页数加 2 MiB，因超过 CLI 原 16 MiB 额度而拒绝，没有加载伪原生代码或在宿主制造内存耗尽。

macOS AArch64、Rust 1.99.1 下，workspace check、all-target clippy `-D warnings`、fmt、Rust 1.85.0 all-target check 通过。debug/release all-targets 各 82 个 harness、516 项测试通过，两种构建 doc-tests 通过，日志为 `/tmp/wings-cli-accounting-{check,clippy,fmt,msrv,tests-debug,tests-release,doctests-debug,doctests-release}.log`。

完整 debug AOT 通过，见 `/tmp/wings-cli-accounting-aot-debug.log`。首次 release AOT 在既有 TCP invalid 场景失败：预期 invalid=3，实际 invalid=2、io_errors=1，正常业务 committed=4，资源已回到 168 B 根基线；报告保留在 `/tmp/wings-cli-accounting-aot-release.log`。独立 TCP 复核随后在 disconnected 场景收到 reset，服务正常退出但 published=0、max_socket_buffer=0、io_errors=1，见 `/tmp/wings-cli-accounting-tcp-release.log`。此次没有修改 TCP 实现、缓冲上限、I/O deadline、业务预算或测试期望。

受限独立 socket 探针使用相同 16384 B 设置与 65536 B 上限，计划至多 128 连接，在第 97 次连接读取到 SO_RCVBUF=277696 B 后失败；探针子进程 CPU 5 秒/墙钟 10 秒/禁 core，见 `/tmp/wings-cli-accounting-socket-probe.log`。它证明本机存在设置后读取超额的情况，尚不能确定上述两次 TCP 失败都由该条件触发；TCP 稳定性与资源边界仍是未解决项，不能以复跑成功消除。

保持原参数首次完整 release 复跑通过，见 `/tmp/wings-cli-accounting-aot-release-rerun.log`，前述失败原样保留。debug/release 各显式 Wings/BEAM/Python v1、v2 正确性回归通过：v1 45 组业务、28 JSON/14 桥接拒绝，v2 18 组业务、49 JSON/17 桥接拒绝。日志为 `/tmp/wings-cli-accounting-beam-v{1,2}-{debug,release}.log`。没有重跑五对长测或宣称吞吐、尾延迟、整机内存改进。

Linux 仍是 Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 功能验证。冻结包 `/tmp/wings-cli-accounting-linux.tar.gz` 含 305 份源码、82 个 harness 和 60 个工具/示例，排除语法评审稿；执行前与完整 AOT 后全部 447 项 SHA-256 一致，实现源码与宿主最终工作区一致，验证记录后补。516 项 release 测试与完整受限 AOT 通过，包含新 CLI 默认/堆模式加载准入、真实正常调用及错误/清理场景。见 `/tmp/wings-cli-accounting-linux-{tests,aot,hashes,host-sources}.log`。原 AOT CPU 20 秒/墙钟 30 秒/禁 core、业务 CPU 5 秒/墙钟 10 秒和 3 秒阶段不变，保留旧 linker 警告；客体验证后停止。该结果没有新增原生 Linux CI、debug/clippy/MSRV/doc-tests 或真实 Linux BEAM 证据。

依赖隔离、禁 unwind feature、Python AST、98 份 Markdown 的 955 条相对路径/围栏及 diff 空白检查通过，见 `/tmp/wings-cli-accounting-static.log` 与 `/tmp/wings-cli-accounting-docs-final.log`。本次只补 CLI 核心计费路径，JSON/标准 I/O 与宿主控制对象的完整计费、独立工具入口、业务值、公平性和原生双平台 BEAM 双门禁仍未完成；macOS TCP 异常独立保留，MVP 状态不变。

## TCP 运行期间的缓冲限制：ADR-0082

基于 `207ba04`，按 [ADR-0082](adr/0082-bounded-tcp-segments.md) 修正 TCP 示例仅在设置时读取缓冲、随后仍可能增长的假设。macOS 已接受连接先将实际 MSS 限至当前正值与 1024 的较小值，再重设原 16 KiB 收发缓冲；每次 read/write 返回后核对原 64 KiB 上限。新增固定的 socket_setup_errors、socket_buffer_rejections 与 macOS 实际 max_tcp_segment；其他平台的 MSS 字段为 null。原全部资源预留、连接/帧数、50 ms 请求/等待、100 ms 绝对读写、2 秒 accept、业务预算和 ABI v15 不变。

max_tcp_segment 是配置确认时跨连接的最大值，max_socket_buffer 是配置及各次 I/O 后的最大容量读数；采样不覆盖所有内核瞬间，也不测量物理分配峰值。

### 缺陷复现与探针

本机为 macOS 26.6.2、Darwin 25.6.0 / `xnu-12377.161.14~5`、Apple Silicon。新增真实 socket 回归先在原实现失败：完成 32 B 帧读取后 SO_RCVBUF 已为 277696 B，超过 65536 B；失败保留在 `/tmp/wings-tcp-segments-before.log`，不是静态推断。

独立探针的四个配置组各 128 连接、每连接 16 帧，子进程保持 CPU 5 秒/墙钟 10 秒/禁 core。默认和仅限制 listener 的两组实际连接 MSS=16332，运行期间各 128 个连接全部超额，峰值 277696 B；接受后限制 MSS 的两组均为 1024、峰值 32768 B、无超额，见 `/tmp/wings-tcp-mss-probe.log`。配置前曾读取到 326640 B，不能将配置后的检查写成握手或 OS 物理内存硬上限。

公开 XNU 提交 `f6217f891ac0bb64f3d375211650a4c1ff8ca1ea` 中固定缓冲仍可按 MSS 增长的路径用于解释机制，原文件与该提交 hash 核对一致；该公开提交来自 2025-10-16，不是本机内核构建源码。原 ADR-0081 两次 TCP 失败没有细分字段，仍不能追溯其唯一原因；失败日志不改写。

### 本机功能验证

新增 2 项单测：128×16 帧真实双向传输期间独立读取缓冲核对上限与平台 MSS；配置后人为设置小规模超额，检查拒绝前保存真实峰值与计数。原单测同时保留 listener/接受连接配置与部分帧不刷新 100 ms 截止时间，4 项通过。debug/release 各三轮完整 TCP 专项通过，包含新增原最大 8×64 帧、512 次实际请求/回复、最终 committed=512 及所有旧错误场景；新增设置错误和缓冲拒绝均为零。见 `/tmp/wings-tcp-segments-unit-debug.log` 与 `/tmp/wings-tcp-segments-focused-{debug,release}.log`。这是有限功能验证，不是调度延迟或吞吐测量。

workspace check、all-target clippy `-D warnings`、fmt、Rust 1.85.0 all-target check 通过；debug/release all-targets 各 82 个 harness、518 项测试通过，两种构建 doc-tests 通过。日志为 `/tmp/wings-tcp-segments-{check,clippy,fmt,msrv,tests-debug,tests-release,doctests-debug,doctests-release}.log`。

debug/release 完整原限 AOT 各首次通过，包含新最大连接/帧数与旧 TCP 全场景、CLI 默认/堆模式、执行/GC/消息/请求/监督及配置前加载拒绝。每个 AOT 子进程仍为 CPU 20 秒/墙钟 30 秒/禁 core，业务保留 CPU 5 秒/墙钟 10 秒及原 3 秒阶段，见 `/tmp/wings-tcp-segments-aot-{debug,release}.log`。两种构建另显式执行真实 Wings/BEAM/Python v1、v2：45/18 组业务以及原 JSON/桥接拒绝全部通过，见 `/tmp/wings-tcp-segments-beam-v{1,2}-{debug,release}.log`。没有重跑五对长测、改变原冻结结果或宣称性能/内存门禁通过。

### Linux x86-64 隔离仿真

沿用 Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 客体；macOS 交叉构建 release，客体 GCC 13.3 链接并执行真实 AOT `.so`。冻结包 `/tmp/wings-tcp-segments-final-linux.tar.gz` 含 306 份源码、82 个 harness 和 60 个工具/示例，排除语法评审稿；执行前与完整 AOT 后全部 448 项 source/harness/binary hash 一致，且冻结实现源码与宿主最终工作区一致。82 个独立资源限制测试子进程共 518 项通过，完整受限 release AOT 通过，包含新增 TCP 最大 8×64 业务与运行期间缓冲报告，见 `/tmp/wings-tcp-segments-final-linux-{tests,aot,hashes,host-sources}.log`。验证后停止客体；保留 linker 警告，不代表原生 Linux CI、Linux BEAM 或原生平台性能/内存证据。

## 计费模块禁止未计费执行降级：ADR-0083

基于 `389491e`，按 [ADR-0083](adr/0083-accounted-module-execution.md) 修正 `validate_pool` 在池没有账户时跳过全部校验的问题。计费模块默认 `instantiate()` 在创建 TLS 工具池前拒绝；显式初始化、受控调用、owned task 与镜像接回都要求本 VM 的 process 计费池。未计费模块的独立工具入口保留原边界，不修改 ABI v15、资源上限、总预算或时限。

可信 Rust ABI fixture 的新增回归先在旧实现实际失败：`module.instantiate().is_err()` 未成立，原路径已经执行 initializer，见 `/tmp/wings-module-binding-before.log`。修复后检查未计费/未绑定池的拒绝不进入业务、栈与账目零变化，owned task 不变 active，镜像拒绝后仍可成功接回且只能消费一次；正常 Get 返回后入口总调用数恰为 initializer 加 handler 两次。该 fixture 不冒充生成代码证据。

`module_accounting` 真实 AOT 回归新增计费根下相同入口与镜像拒绝，在 process/tenant/VM 各自满额时检查不能切换工具池绕过；每级释放填充后仍读出旧状态 7，最后成功更新为 9，清理回到根基线。原租约共享/循环装载/跨 VM/暂停取消用例改用显式池，活动调用核对真实新增 4352 B 控制，释放后回到池/模块基线，而非继续将仅模块字节作为完整执行成本。

首次完整 debug AOT 暴露新检查顺序遮住原跨 VM 诊断：`detached_process` 的 foreign tenant 池应返回 `different VM`，却先报 process 绑定错误。失败保留于 `/tmp/wings-module-binding-aot-debug.log`；核心调整为先执行已有 VM 身份校验，再检查 process 绑定，没有放宽原测试期望、消耗镜像或修改额度/时限。

最终 macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check 与 fmt 通过；debug/release 各 82 个 harness、519 项测试及 doc-tests 通过。两种构建的完整受限 AOT，以及显式 Wings/BEAM/Python v1 的 45 组业务、28 项 JSON/14 项桥接拒绝和 v2 的 18 组业务、17 项桥接/49 项 JSON 拒绝全部通过。日志为 `/tmp/wings-module-binding-final-{check,clippy,msrv,fmt,tests-debug,doctests-debug,build-debug,aot-debug}.log`、`/tmp/wings-module-binding-{tests,doctests,build,aot}-release.log` 与 `/tmp/wings-module-binding-beam-v{1,2}-{debug,release}.log`。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体；release 冻结包 `/tmp/wings-module-binding-linux.tar.gz` 含 307 份源码、82 个 harness 与 60 个工具/示例，排除语法评审稿。各测试子进程保留 CPU 20s/墙钟 30s/core 0：519 项测试及完整受限 AOT 通过；执行后全部 449 项 source/harness/binary hash 一致，冻结源码也与宿主实现一致，随后仅补本验证记录。见 `/tmp/wings-module-binding-linux-{tests,aot,hashes,host-sources}.log`。验证后停止客体，保留交叉 linker 警告；这些是功能证据，不替代原生 Linux CI、双平台 BEAM 吞吐/整机内存门禁或完整账本。

## 计费根的模块租约准入：ADR-0084

基于 `eceb441`，按 [ADR-0084](adr/0084-accounted-root-module-admission.md) 收敛 `load_trusted` 工具模块与 `vm_accounted` 的反向组合。新增可信 Rust ABI fixture 先在旧实现实际失败：`module.instantiate_in_pool(&bound).is_err()` 未成立，见 `/tmp/wings-root-module-before.log`。该 fixture 只证明核心入口行为，不冒充生成代码证据。

核心从账户推导根已有的启动计费标志，在原 VM 身份检查后拒绝没有模块租约的代码；不新增账户或预留。回归检查初始化、带堆初始化、受控调用、owned task 和镜像拒绝不进入业务、不改账目和池、不消费镜像，随后工具路径仍可执行。真实 AOT `vm_bootstrap` 增加相同标量入口、Bytes 初始化与 worker/Scheduler 的模块拒绝，原单槽容量仍可接入合法计费模块；单/双 worker 的标量与 Bytes 结果租约最终回到根启动基线。保持原资源、CPU/墙钟和阶段截止时间。

首次完整 debug AOT 在 `accounted_stacks` 发现旧 ADR-0080 直接调用 fixture 仍把工具模块放进计费根，见 `/tmp/wings-root-module-aot-debug.log`。改为每个根加载计费模块后，第二次在填充 process/tenant 额度时先遇到 VM 拒绝：真实模块占用原 4 MiB 根的余量，不再能分别填满下级，见 `/tmp/wings-root-module-final-aot-debug.log`。保留原 4 MiB/3 MiB/1 MiB 和单槽的工具模块拒绝/账目零变化检查；将完整三级控制专项移入已有 32 MiB 的 `vm_bootstrap` 回归，保留 tenant 3 MiB、process 1 MiB、单槽和控制 4352 B，并加强为必须报告目标级别的资源错误。正路径使用真实计费模块、按实际 used 填充，最后同时释放模块再核对启动基线；其余独立旧 `vm` 工具树用例保持不变，不扩大任何既有工具入口的配置或时限。

最终 macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 与 doc-tests 通过；debug/release 各 82 个 harness、520 项测试通过。两种构建的完整受限 AOT，以及显式 Wings/BEAM/Python v1 的 45 组业务、28 项 JSON/14 项桥接拒绝和 v2 的 18 组业务、17 项桥接/49 项 JSON 拒绝全部通过。新增 `accounted_stacks` 与 `vm_bootstrap` 输出见 `/tmp/wings-root-module-focused-aot.log`；完整日志为 `/tmp/wings-root-module-verified-{check,clippy,msrv,fmt,tests-debug,doctests-debug,build-debug,aot-debug}.log`、`/tmp/wings-root-module-{tests,doctests,build,aot}-release.log` 与 `/tmp/wings-root-module-beam-v{1,2}-{debug,release}.log`。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体；release 冻结包 `/tmp/wings-root-module-linux.tar.gz` 含 308 份源码、82 个 harness 与 60 个工具/示例，排除语法评审稿。各测试子进程保留 CPU 20s/墙钟 30s/core 0：520 项测试及完整受限 AOT 通过；执行后全部 450 项 source/harness/binary hash 一致，冻结源码也与宿主实现一致，随后停止客体。见 `/tmp/wings-root-module-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-root-module-host-sources.log`。这些是功能证据，不替代原生 Linux CI、双平台 BEAM 吞吐/整机内存门禁或完整账本。

## 模块路径解析前的控制准入：ADR-0085

基于 `2a5fc9d`，按 [ADR-0085](adr/0085-module-path-admission.md) 修正计费模块在预留前解析路径的顺序。新增单测先在旧实现实际失败：零控制余量时先返回路径错误，而非 `resource_exhausted at Vm account`，见 `/tmp/wings-module-path-before.log`。修复后在路径解析前持有原 2 MiB 控制票据，库元数据验证后再追加原页预算；成功总额仍为库页加 2 MiB。

单测分别覆盖旧零启动根和计费根的零/少一字节/精确控制余量、路径错误与重复失败回滚；跨线程末持有者 fixture 将原 1024 B 分为两份 512 B，保持原总额并验证两部分同时受最后代码租约约束。真实 AOT `module_accounting` 增加同样的早拒绝和精确控制但库页不足的回滚检查，保留原完整模块精确/少一字节额度、32 次循环加载、坏产物、跨 VM 与暂停取消清理。

最终 macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、521 项测试及完整受限 AOT 通过。显式 Wings/BEAM/Python v1 的 45 组业务、28 项 JSON/14 项桥接拒绝和 v2 的 18 组业务、17 项桥接/49 项 JSON 拒绝也在 debug/release 全部通过。见 `/tmp/wings-module-path-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 与 `/tmp/wings-module-path-beam-v{1,2}-{debug,release}.log`；依赖/unwind、102 份 Markdown 的 1000 条相对路径和围栏、Python AST 与 diff 检查通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-module-path-linux.tar.gz` 含 309 份源码、82 个 harness 和 60 个工具/示例，排除语法评审稿；原 CPU 20s/墙钟 30s/core 0 的独立测试子进程共 521 项通过，完整受限 AOT 通过。执行后全部 451 项 source/harness/binary hash 一致，冻结源码与宿主实现一致，随后停止客体；见 `/tmp/wings-module-path-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-module-path-host-sources.log`。交叉 linker 警告保留；不将这些功能证据当作原生 Linux CI、完整账本或 BEAM 吞吐/整机内存验收。

## CLI 请求缓冲与有界解析：ADR-0086

基于 `2814571`，按 [ADR-0086](adr/0086-accounted-cli-json.md) 将 CLI 的复用行缓冲、JSON 转义暂存和固定控制先预留到 process/tenant/VM 的原 16 MiB 树中。真实旧 CLI 接受重复 message/args，各输出 `{"reply":7}`；新增拒绝断言实际失败，见 `/tmp/wings-cli-input-before.log`。原 Value 折叠字段后已执行当前 handler；新 visitor 明确拒绝重复字段，不把这项行为变化写成原路径已有保证。

复用 workspace 已锁定 serde，不增加第三方包或版本；取消通用 Value/Map，固定 256 B 消息名、64 个标量槽及回复结构。原 64 KiB 行限制、参数数量、业务步数、ABI v15 与类型拼写不变。准入先于取消标志、行分配和 initializer；264 KiB 是含转义 Vec 增长暂存的保守逻辑容量，不是标准 I/O 全局存储或 OS 内存硬限制。回归覆盖三级零/少一字节/精确边界、账目零变化、缓冲复用、最大行/末行/CRLF、转义/Unicode、数值范围、重复/缺失/未知字段、嵌套/字符串/65 参数和读写失败。完整 AOT 增加默认/堆两模式拒绝当前非法请求、保留此前合法输出，以及真实 64 参数 handler。

首次完整本机 debug/release 的 529 项测试与完整 AOT、BEAM v1/v2 正确性通过后，审计发现 serde 默认 Unexpected::Str 错误格式化会展开输入：60,003 B 的 DEL 字符 JSON 字符串产生约 360 KiB stderr，超过暂存余量，见 `/tmp/wings-cli-input-error-size-before.log`。初次 Linux 冻结包仅完成交叉构建与客体启动，未执行功能验收；停止客体后为顶层/args 错误字符串增加固定诊断，单测与真实 AOT 加入两项长字符串错误大小检查。后续最终冻结及全部回归以修正版本为准，保留最初失败证据。

最终 macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、529 项测试及完整受限 AOT 通过。显式 Wings/BEAM/Python v1 的 45 组业务、28 项 JSON/14 项桥接拒绝和 v2 的 18 组业务、17 项桥接/49 项 JSON 拒绝在 debug/release 全部通过，见 `/tmp/wings-cli-input-final-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 与 `/tmp/wings-cli-input-final-beam-v{1,2}-{debug,release}.log`。长 DEL 字符串最终分别为 60,003/60,028 B 输入、56 B stderr，见 `/tmp/wings-cli-input-focused-aot-bounded-errors.log`；8 项适配器单测通过，读取/输出错误仍保留票据至适配器销毁。依赖隔离、协程禁 unwind、Python AST、103 份 Markdown 的 1010 条相对路径/围栏和 diff 检查通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。修正版本冻结包 `/tmp/wings-cli-input-final-linux.tar.gz` 含 311 份源码、82 个 harness 和 60 个工具/示例，排除语法评审稿，显式包含新增 CLI 源文件。原 CPU 20s/墙钟 30s/core 0 的独立测试子进程共 529 项通过，完整受限 release AOT 通过；执行后全部 453 项 source/harness/binary hash 一致，冻结源码与宿主实现一致。日志为 `/tmp/wings-cli-input-final-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-cli-input-final-host-sources.log`。客体已停止；保留交叉 linker 警告，不把功能验证视为原生 Linux CI、完整资源账本或 BEAM 吞吐/整机内存验收。

## BEAM 驱动计费根与清理回执：ADR-0087

基于 `457f72c`，按 [ADR-0087](adr/0087-accounted-beam-workload-roots.md) 收敛六个对照驱动的零启动工具根。旧真实 v1 报 `resources_released: true` 但没有 bootstrap/remaining 回执，新增断言实际失败，见 `/tmp/wings-beam-root-before.log`。修复后各驱动使用 `vm_accounted`，保留原限额与业务；最后释放所有派生持有者再比较完整 Resources 的启动基线，报告实际正启动字节与相等余量。

独立回执检查器覆盖旧历史重放、缺失/半份回执、bool/float/负数/超 u64 与清理不等。当前 v1–v7 正确性入口要求回执；v6/v8 新采集显式传入预期运行时，回执或 runtime 错误在 trace verifier 前使实验无效并保存原始输出。离线重放允许旧冻结记录没有两字段，已有 trace/hash/结果不回写。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、529 项测试和完整受限 AOT 通过。七个显式 BEAM 入口在两种构建全部通过：v1 的 45 组业务、v2 的 18 组业务、v3 的 21 组 FIFO/交错业务、v4 的 1/100/256 队列、v5 的六个短时开环计划、v7 的两个突发心跳计划及 v8 有界采集器模型；这不包含新的长时性能测量。独立回执模型拒绝 37 份伪造记录；采集器另拒绝五种错误回执/runtime，并保存为 `invalid_experiment`，不进入 trace verifier。日志为 `/tmp/wings-beam-root-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log`、`/tmp/wings-beam-root-verify_*-{debug,release}.log` 和 `/tmp/wings-beam-root-{receipts,measurement-model,fairness-model}.log`。依赖隔离、协程禁 unwind、Python AST、104 份 Markdown 的 1020 条相对路径/围栏和 diff 检查通过。

首次完整 debug AOT 在 TCP 示例等待监听输出的原五秒边界超时，保留 `/tmp/wings-beam-root-aot-debug-initial.log`；原参数的独立 TCP 回归及随后完整 debug/release AOT 通过，见 `/tmp/wings-beam-root-tcp-focused.log` 与最终 AOT 日志。没有修改 TCP 实现、容量或任何 CPU/墙钟/阶段时限；首次失败不计为通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-beam-root-linux.tar.gz` 含 313 份源码、82 个 harness 和 60 个工具/示例，显式包含新回执检查器与 ADR，排除语法评审稿。原 CPU 20s/墙钟 30s/core 0 的独立测试子进程共 529 项通过，完整受限 release AOT 通过；执行后全部 455 项 source/harness/binary hash 一致，313 份冻结源码与宿主工作区一致，随后仅补本验证记录。日志为 `/tmp/wings-beam-root-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-beam-root-host-sources.log`。客体已停止，交叉 linker 警告保留；该结果不替代原生 Linux CI、完整宿主资源计费或同负载 BEAM 吞吐/整机内存门禁。

## CLI 业务标准 I/O 的独立预留与清理：ADR-0088

基于 `148e1c1`，按 [ADR-0088](adr/0088-owned-cli-stdio.md) 收敛业务 stdin/stdout 的生命周期。旧标准库最小探针观察到锁释放后仍保留 9344 B allocator 请求，预期释放断言在 CPU 20s/墙钟 30s/core 0 的独立 OS 子进程失败；见 `/tmp/wings-cli-stdio-before.log`。它只验证当前 macOS 标准库全局锁的行为，不等于完整 CLI 或 RSS 数值，不硬编码其他平台成本。

新业务 I/O 在复制描述符与 initializer 前预留 12 KiB，包含 8 KiB 输入缓冲、固定 128 B 输出缓冲及控制余量；保留原 264 KiB 请求适配器、16 MiB 三级限额和每回复 flush。副本设置 close-on-exec，退出时先销毁缓冲/副本再归还预留；Drop 不重试未 flush 内容。启动参数、帮助/stderr、完整宿主账本及双门禁仍未完成。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 与 debug/release doc-tests 通过；两种构建各 82 个 harness、535 项测试及完整受限 AOT 通过。六项 I/O 单测覆盖固定控制上界、三级零/少一字节/精确准入且拒绝前不打开文件、两阶段打开失败回滚、close-on-exec/原描述符保留、flush 失败期间持有预留与退出丢弃尾部。真实默认/堆 CLI 均在输入 EOF 前及时回复，断管和目录输入读失败正常退出；回复观测使用原五秒阶段边界，按字节读取有上限，不因缺少换行而无界等待。显式 Wings/BEAM/Python v1 的 45 组业务与 v2 的 18 组业务及原 JSON/桥接拒绝在两种构建全部通过，无新性能结论。见 `/tmp/wings-cli-stdio-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 与 `/tmp/wings-cli-stdio-verify_{counter_equivalence,multi_counter}-{debug,release}.log`。依赖隔离、协程禁 unwind、Python AST、105 份 Markdown 的 1029 条相对路径/围栏及 diff 检查通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-cli-stdio-linux.tar.gz` 含 315 份源码、82 个 harness 和 60 个工具/示例，显式包含新 I/O 模块与 ADR，排除语法评审稿。原 CPU 20s/墙钟 30s/core 0 的独立子进程共 535 项测试通过，完整受限 AOT 通过，包括默认/堆 CLI 的交互回复、断管和目录输入错误。执行后全部 457 项 source/harness/binary hash 一致，315 份冻结源码与宿主工作区一致，随后仅补本验证记录。见 `/tmp/wings-cli-stdio-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-cli-stdio-host-sources.log`。客体已停止，交叉 linker 警告保留；不将功能验证替代原生 Linux CI、完整资源账本或同负载 BEAM 吞吐/整机内存门禁。

## CLI 启动参数的有界读取与准入：ADR-0089

基于 `a4f941b`，按 [ADR-0089](adr/0089-bounded-cli-startup-arguments.md) 收敛自有启动参数副本。旧真实 CLI 对 `/tmp/wings-artifact-\xff` 参数在 `std::env::args` 的 String 转换中 panic，退出 101，见 `/tmp/wings-cli-argv-before.log`；探针在 CPU 20s/墙钟 30s/core 0 子进程运行。macOS 文件系统不允许创建该非法字节别名，最初别名探针在启动 CLI 前失败，不计为运行证据；后改为直接传原始参数记录 panic，Linux 再验证真实非 UTF-8 别名。

新启动路径保留 Rust main/运行时，先建立计费根/子账户并预留 20 KiB，再借用原生 argv；仅复制通过校验的目录字节，最多七个参数、含 NUL 总计 16 KiB，不解析 argv[0]。参数数量/字节超限或非法选项正常退出 2，非 UTF-8 路径交文件系统处理；原步数、堆容量、三级 16 MiB、ABI 和业务时限不变。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、541 项测试与完整受限 AOT 通过。六项启动单测覆盖三级拒绝前不读参数、固定控制上界、精确预留/释放、正常/重复选项和数值边界、数量/指针/含 NUL 字节边界、非 UTF-8 目录副本及来源失败回滚。真实 CLI 覆盖 help、八参数先拒绝、默认/堆两模式精确 16 KiB 与多一字节、非 UTF-8 选项/数值正常退出 2、非 UTF-8 路径正常文件系统错误而非 panic；原 JSON/I/O 和业务回归继续通过。显式 Wings/BEAM/Python v1 的 45 组业务、v2 的 18 组业务及原 JSON/桥接拒绝在两种构建全部通过，未新增长时性能结果。日志为 `/tmp/wings-cli-argv-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 与 `/tmp/wings-cli-argv-verify_{counter_equivalence,multi_counter}-{debug,release}.log`。依赖隔离、协程禁 unwind、Python AST、106 份 Markdown 的 1038 条相对路径/围栏及 diff 检查通过。

独立分配探针复用本切片源文件与同一资源实现，在 CPU 20s/墙钟 30s/core 0 的子进程测量 Startup 创建至销毁：8 B 和精确上限下 16,369 B 的目录分别仅请求 8/16,369 B，期间保留相同字节，销毁后 allocator 请求和 process 账目都回到基线，预留均为 20,480 B；见 `/tmp/wings-cli-argv-allocation-probe.log`。该结果支持当前路径没有复制整份 argv，不等于 Rust 启动、OS 已有 argv/环境、RSS 或完整 CLI 账本已闭环。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-cli-argv-linux.tar.gz` 含 317 份源码、82 个 harness 和 60 个工具/示例，显式包含新启动参数模块与 ADR，排除语法评审稿。原 CPU 20s/墙钟 30s/core 0 的独立子进程共 541 项测试通过，完整受限 AOT 通过。glibc init-array 的参数捕获经真实 CLI 执行验证；非 UTF-8 目录别名在默认/堆模式均返回真实 AOT 回复 7，参数超限、非 UTF-8 选项、JSON/I/O 与全部业务回归继续通过。执行后全部 459 项 source/harness/binary hash 一致，317 份冻结源码与宿主工作区一致，随后仅补本验证记录。日志为 `/tmp/wings-cli-argv-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-cli-argv-host-sources.log`。客体已停止，交叉 linker 警告保留；不把功能验证当作原生 Linux CI、完整资源账本或同负载 BEAM 吞吐/整机内存证据。

## CLI 帮助与诊断的计费输出：ADR-0090

基于 `c988af1`，按 [ADR-0090](adr/0090-bounded-cli-reporting.md) 收敛控制输出。旧真实 CLI 帮助 stdout 断管时由 println panic、退出 101；64 KiB 内的 60,002 B 非法清单字符串产生 360,083 B stderr，见 `/tmp/wings-cli-report-before.log`。两项探针均在 CPU 20s/墙钟 30s/core 0 子进程运行，非法库为空文件且错误发生在清单解析、没有装载执行。

新控制输出在原 VM 16 MiB 限额内预留 8 KiB，包含固定 4 KiB 诊断缓冲和独立 stderr 描述符；帮助按需复制 stdout，直接写静态文本。正常错误前缀和 2/1 退出码保留，断管不 panic；诊断最多 4096 B，按 UTF-8 边界截断并标注。此处仅约束自有输出缓冲和对外字节，来源错误 String 的构造/生命周期仍须收敛；根/诊断初始化失败只走固定静态应急文本，不用工具根回退。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、546 项测试与完整受限 AOT 通过。五项控制输出单测覆盖固定上界、VM 零/少一字节/精确准入且拒绝前不打开描述符、错误层级/打开失败回滚、UTF-8/边界截断及缓冲复用、原描述符所有者保留/副本清理和写失败不 panic。真实帮助 stdout 断管退出 1，参数/运行错误在 stderr 断管下保留 2/1，帮助两端同时断管也正常失败；默认/堆两模式的非法产物诊断不超过 4096 B 且带截断标记，原参数/JSON/业务 I/O 继续通过。显式 Wings/BEAM/Python v1 的 45 组业务、v2 的 18 组业务及原 JSON/桥接拒绝在两种构建全部通过，未新增长时性能结果。日志为 `/tmp/wings-cli-report-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 与 `/tmp/wings-cli-report-verify_{counter_equivalence,multi_counter}-{debug,release}.log`。依赖隔离、协程禁 unwind、Python AST、107 份 Markdown 的 1049 条相对路径/围栏及 diff 检查通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-cli-report-linux.tar.gz` 含 319 份源码、82 个 harness 和 60 个工具/示例，显式包含新控制输出模块与 ADR，排除语法评审稿。原 CPU 20s/墙钟 30s/core 0 的独立子进程共 546 项测试通过，完整受限 AOT 通过，包括帮助/诊断断管、默认/堆两模式非法产物的 4096 B 边界与原参数/JSON/I/O/业务回归。执行后全部 461 项 source/harness/binary hash 一致，319 份冻结源码与宿主工作区一致，随后仅补本验证记录。日志为 `/tmp/wings-cli-report-linux-{tests,aot,hashes,stop}.log` 与 `/tmp/wings-cli-report-host-sources.log`。客体已停止，交叉 linker 警告保留；不替代来源错误分配、完整宿主账本、原生 Linux CI 或同负载 BEAM 吞吐/整机内存验收。

## 产物清单字符串的来源错误边界：ADR-0091

基于 `783b71b`，按 [ADR-0091](adr/0091-manifest-string-preflight.md) 收敛清单错误构造。独立全局 allocator 探针在 CPU 20s/墙钟 30s/core 0 子进程调用真实计费模块加载：60,002 B 非法 JSON 产生 360,072 B 错误文本，返回后持有 720,098 B，模块预留已归还；错误销毁后存储归零。累计分配请求为 2,391,068 B，不作为同时峰值或 RSS，见 `/tmp/wings-artifact-error-before.log`。空库未执行，错误发生在清单阶段。

文件读取在原 64 KiB 和 2 MiB 模块控制预留内增加无对象树的字符串预检，所有 JSON 键/字符串须符合既有至多 256 UTF-8 字节、无控制字符边界；保留原 serde 类型解码和语义检查。修复后同一探针文本 61 B、返回后存储 76 B、累计请求 131,874 B，销毁后存储与账目回到基线，见 `/tmp/wings-artifact-error-after.log`；返回短错误仍属于调用方控制路径，不把该探针写成所有来源错误计费完成。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 与 debug/release doc-tests 通过；两种构建各 82 个 harness、550 项测试及完整受限 AOT 通过。产物套件共 27 项，新增四项覆盖顶层/字段/类型/handler/参数/未知键的长字符串、控制/转义/UTF-8 边界和短非 ASCII 错误，原对象/序列/单元枚举表示及缺省 reply 保持，语法/递归/尾随拒绝不变。真实默认/堆 CLI 的该清单错误不超过 256 B，不含末端截断标记；输出截断仍由固定缓冲单测独立验证，断管和原 JSON/I/O/业务继续通过。显式 Wings/BEAM/Python v1 的 45 组业务、v2 的 18 组业务与原 JSON/桥接拒绝在两种构建全部通过，未新增长时性能结果。日志为 `/tmp/wings-artifact-error-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release,aot-debug,aot-release}.log` 和 `/tmp/wings-artifact-error-verify_{counter_equivalence,multi_counter}-{debug,release}.log`。依赖隔离、协程禁 unwind、Python AST、108 份 Markdown 的 1058 条相对路径/围栏和 diff 检查通过。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。release 冻结包 `/tmp/wings-artifact-error-linux.tar.gz` 含 321 份源码、82 个 harness 和 60 个工具/示例，显式包含预检模块与 ADR，排除语法评审稿。首次测试在既有 scheduler 诊断回归的两次账目读取间遇到后台消息清理：bytes 从 5,435,856 降至 5,434,488、messages 从 1 降至 0，固定快照相等断言失败，见 `/tmp/wings-artifact-error-linux-tests-initial.log`；原参数的定向回归与全部 550 项测试随后通过。首次完整 AOT 又在既有 sparse-256 开环三秒阶段截止失败，见 `/tmp/wings-artifact-error-linux-aot-initial.log`；原参数独立开环和完整 AOT 随后通过，见 `/tmp/wings-artifact-error-linux-{scheduler-focused,open-loop-focused,tests,aot}.log`。首次失败均保留，不计为通过；本片没有修改调度实现/断言、容量或 CPU/墙钟/阶段时限，不能从重跑通过推出时序问题已解决。

最终各 harness 保留 CPU 20s/墙钟 30s/core 0，完整受限 AOT 包含新清单来源拒绝与原 JSON/I/O/业务；执行后全部 463 项 source/harness/binary hash 一致，321 份冻结源码与宿主工作区一致，随后仅补本验证记录。见 `/tmp/wings-artifact-error-linux-{hashes,stop}.log` 与 `/tmp/wings-artifact-error-host-sources.log`。客体已停止，交叉 linker 警告保留；不将 TCG 功能回归替代原生 Linux CI、完整资源账本、时序稳定性或同负载 BEAM 吞吐/整机内存门禁。

## 调度退出诊断测试的清理边界

基于 `86c5a58`，修正上节首次 Linux 测试暴露的错误断言。既有 [ADR-0055](adr/0055-multi-worker-exit-diagnostics.md) 明确诊断在 actor/mailbox 清理前发布；worker 在 registry 解锁后销毁消息和退役 actor。旧测试读取 last_exit 前后比较全局 vm.used，可能恰好跨过该清理，先前实际观察到一条消息和 1368 B 预留归还。该现象不证明诊断查询持有资源，也不能靠重跑通过证明已修复。

保留运行中的故障分类、owner/旧 Pid、旧提交状态、预算失败、成功/stop 不覆盖诊断及其他 actor 前进的全部断言；只把“重复读取不改变账目”的检查移至 scheduler 销毁并 join worker 后。此时明确验证 process/message/request 为零，handle 仍持有固定控制存储，两次查询后完整 Resources 与基线相等，最后释放 handle 全部归零。运行时实现、发布顺序、阶段/CPU/墙钟时限、容量及原 MVP 门槛不变，本次不改 ADR 契约。

macOS AArch64 的 workspace check、all-target clippy `-D warnings`、Rust 1.85.0 all-target check、fmt 和 debug/release doc-tests 通过；两种构建各 82 个 harness、550 项测试通过，目标测试分别额外执行 32 次独立子进程回归。每个重复子进程保留 CPU 20s/墙钟 30s/core 0，记录为 `/tmp/wings-scheduler-diagnostic-repeat-{debug,release}.log`；其余检查见 `/tmp/wings-scheduler-diagnostic-{check,clippy,msrv,fmt,tests-debug,tests-release,doctests-debug,doctests-release}.log`。依赖隔离、协程禁 unwind、Python AST、108 份 Markdown 的 1059 条相对路径/围栏和 diff 检查通过。本片只改测试和评测说明，未重新执行 AOT 或 BEAM 业务，不沿用历史运行结果冒充新测量，也不据此声称三秒开环超时已经解决。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。冻结包 `/tmp/wings-scheduler-diagnostic-linux.tar.gz` 含 321 份源码、82 个 harness 与 60 个工具/示例，排除语法评审稿；原独立子进程限额下 550 项 release 测试通过，目标测试另外 32 次独立重复通过，与本机合计 96 次。测试后全部 463 项 source/harness/binary hash 一致，321 份冻结源码与宿主工作区一致，随后仅补本验证记录；日志为 `/tmp/wings-scheduler-diagnostic-linux-{tests,repeat,hashes,stop}.log` 与 `/tmp/wings-scheduler-diagnostic-host-sources.log`。客体已停止；没有重跑 AOT/BEAM，不将 TCG 功能或固定重复次数当作原生 Linux CI、完整时序稳定性或性能/内存双门禁证据。

## 开环初始化栈池复用：ADR-0073

基于 `b1f1243`，调查前一片保留的 sparse-256 三秒阶段超时。v5/v6 仍逐 actor 使用默认 `spawn_with_heap`，每次构造和销毁一槽初始化池；这与已按 [ADR-0073](adr/0073-reusable-initializer-stack-pool.md) 接入复用的 v2 不同。交付版本在原 3/120 秒阶段开始后创建同 tenant 的一槽、最多缓存一槽计费池，每次仍绑定目标 process，全部创建后立即销毁池，再配置全部 actor 和开始原计划时钟。原输入、actor/状态/mailbox、256 pending、业务步数、额度及 CPU/墙钟限制不变。阶段耗尽的错误标明执行环节，配置失败保留实际终态；ABI v15 和待审语法不变。

独立临时探针仅插入阶段记录，并先比较每次独立池和同一池复用；探针中的复用池保留到退出，交付版本缩短至批量创建结束。Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 客体中，固定 sparse-256、100ms、1000/s、总阶段三秒，子进程 CPU 五秒/墙钟十秒/core 0，交替次序各执行八次。旧版五次失败、复用版四次失败；失败均在启动/配置，保留全部记录 `/tmp/wings-open-loop-phase-linux.log`，不能将后续成功回写为全通过。成功尝试中，旧版创建阶段 1.559–1.668 秒、清理完成累计 2.723–2.813 秒；复用版分别为 0.448–0.462 秒和 1.543–1.637 秒。这些数值只是该仿真环境的阶段定位，未控制为原生性能实验，不证明冷启动稳定性、持续吞吐或整机内存领先。探针、构建和执行脚本保留于 `/tmp/wings-open-loop-phase-{probe,run}.py` 及对应生成源码。

本机首次完整 release AOT 在既有 TCP 普通业务场景的五秒启动等待处失败，见 `/tmp/wings-open-loop-init-aot-release.log`；本片未改 TCP 实现、场景或时限，原参数独立 TCP 套件随后通过，见 `/tmp/wings-open-loop-init-tcp-release-focused.log`。该失败没有保留子进程的启动位置，不能由重跑通过或系统负载观察认定根因已解决。

macOS AArch64 的 debug/release 各 82 个 harness、550 项测试及两种构建的 doc-tests 通过，每个测试进程仍为 CPU 20 秒/墙钟 30 秒/core 0。最终源码的 workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离、禁 unwind、Python 语法与 108 份 Markdown/1061 个相对链接及围栏检查通过。完整 AOT 的 debug 与后续 release 均通过，日志分别为 `/tmp/wings-open-loop-init-aot-debug.log`、`/tmp/wings-open-loop-init-aot-release-final.log`；两种构建显式 OTP 29 的 v5 Wings/BEAM/Python 均通过六组原始到达及十个配置拒绝，日志为 `/tmp/wings-open-loop-init-beam-{debug,release}.log`，原始输出在 `target/open-loop-init-{debug,release}`。本片未重跑 v6/v8 的 90 秒长测，不回写历史性能记录。

Linux 冻结包 `/tmp/wings-open-loop-init-linux.tar.gz` 含 321 份源码、82 个 harness 与 60 个工具/示例，排除语法评审稿。在上述隔离仿真配置中，550 项 release 测试和完整受限 AOT 通过；交付的未插桩开环驱动另以原 sparse-256、100ms、1000/s、三秒总阶段执行八次独立子进程，全部完成实际回复、最终状态和资源回执核对，CPU 五秒/墙钟十秒/core 0 未变。测试后全部 463 项源码/执行文件 hash 一致，宿主与冻结源码仅本节后补验证记录不同；日志为 `/tmp/wings-open-loop-init-linux-{tests,aot,repeat,hashes,stop}.log` 和 `/tmp/wings-open-loop-init-host-sources.log`。客体已确认 Stopped。有限重复通过不消除前述启动早期失败，也不替代最新源码的原生 Linux CI、完整时序稳定性或 BEAM 吞吐/整机内存双门禁，MVP 未完成。

## 双发送者的并发失败前缀：ADR-0092

基于 `5b42034`，新增 [ADR-0092](adr/0092-concurrent-failure-counter.md) 的独立 v9：两个真实宿主线程/BEAM 进程各有 16 窗口，使用三种完整 Bytes/binary 状态和逐目标序号，显式 fail 在候选构造后失败。保留每条输入的 reply/failed/closed 终态、每目标旧提交值/两 sender 提交前缀及健康 sibling；Wings 使用每 child 的监督终态而非可覆盖最近退出缓存，BEAM 使用容量 32 CAS gate 和独立 monitors，失败关闭、不重启或重发。原 3 秒阶段、业务 CPU 5 秒/墙钟 10 秒/core 0、AOT 20/30 秒、ABI v15 和类型拼写不变；待审语法稿不纳入提交。

独立模型检查实际回复存在合法 FIFO 成功前缀、fail 不成功、前缀边界有已接受的注入、关闭后不得再次接受、每条输入恰一结果、最终 counter/序号准确。枚举两条小流的交错及 fail 位置，96 个历史通过、24 个伪造历史拒绝；27 组真实工作负载覆盖空流、单/双 sender 失败、竞争失败、独立目标、跨窗口关闭、100 目标和 i64 两端；18 个非法桥接和 51 个非法 JSON 拒绝。真实 CLI 另验证每 sender 的跳号/零序号报 overflow，合法序号注入报 division-by-zero，候选失败不输出成功值。此处仍未覆盖 timeout/cancel、恢复或长时并发故障，不替代完整性能门禁。

macOS AArch64 的 debug/release 各 83 个 harness、551 项测试、doc-tests 及完整受限 AOT 通过；新示例单测及 Wings/模型入口已进入既有双平台 CI，但本次未运行远端 CI。workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离、禁 unwind、Python 语法和 109 份 Markdown/1072 个相对链接及围栏检查通过。两种构建显式 OTP 29 的 v9 三端均通过全部 27/96/24/18/51 场景，见 `/tmp/wings-concurrent-failure-{tests,aot}-debug.log`、对应 release 日志和 `/tmp/wings-concurrent-failure-beam-{debug-final,release}.log`；本片未重跑 v1–v8 的显式 BEAM 或 v6/v8 长测，不沿用它们为本片的新性能证据。

另选 64 KiB 状态下的双 sender 竞争失败和三目标跨窗口失败两组，每组八次，保留原 CPU/墙钟/三秒阶段；debug/release 的 Wings 和 BEAM 各 16 次独立子进程全部通过。64 份原始输入/输出保留于 `target/concurrent-failure-repeat-{debug,release}`，重复脚本在 `/tmp/wings-concurrent-failure-repeat.py`。实际提交前缀存在差异，两种构建中 Wings 分别出现五/八组、BEAM 各两组，均由同一模型逐份重放核对；次数与交错差异只证明这些有限执行，不能作为完整并发状态空间或长时稳定性证明。

Linux x86-64 沿用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。冻结包 `/tmp/wings-concurrent-failure-linux.tar.gz` 包含 326 份源码、83 个 harness 和 61 个工具/示例，排除语法评审稿；551 项 release 测试、完整受限 AOT（含 v9 Wings/模型全部场景）及上述两组各八次独立执行通过。新一轮未安装或执行 BEAM。全部 470 项 source/harness/binary hash 在结束后一致；326 份冻结源码与宿主仅后补本验证记录不同。16 份原始结果已复制至 `target/concurrent-failure-repeat-linux` 并在宿主独立重放通过。日志为 `/tmp/wings-concurrent-failure-linux-{tests,aot,repeat,hashes,stop}.log` 和 `/tmp/wings-concurrent-failure-host-sources-final.log`，客体已确认 Stopped。TCG 不算最新原生 Linux CI，也不作为吞吐/整机内存、长时故障或完整 MVP 证据。

## 等待前重查就绪与非零期限：ADR-0093

基于 `8803a83`，检查非零请求期限路径时发现 Scheduler 空认领与停车的两次加锁之间存在就绪发布窗口。先等价提取原 `!progress` 决定，再以同一 registry 锁内的空认领、入队/通知、停车决定复现：已有本 worker 队头仍选择等待，确定性回归实际失败，见 `/tmp/wings-scheduler-park-before.log`。旧路径至多增加原一毫秒等待，不描述成无限死锁，也不据此认定此前所有偶发超时的根因。

按 [ADR-0093](adr/0093-scheduler-ready-before-park.md)，在实际等待持有的锁内重查本 worker 队头；空认领后已有就绪工作则直接进入下一轮，其他 worker 的队列不触发本 worker 自旋。已认领但 attach/start 准入失败仍保留原一毫秒退避；活栈 owner、每轮认领/时间片、固定迁移、quota、quantum、请求 deadline、强制检查、ABI v15 和语法均不变。回归另核对单次认领、空队列/外来队列、失败退避及最终清理。

回复表补实际正一毫秒 deadline 的 wait/发布/失败与取消竞争，以已知单调截止时间和操作前后边界判定合法终态，不强求某个线程获胜。迟到发布不得覆盖超时、先选定的回复不被后续取消/过期改写，每个结果只消费一次。晚发布分组在超时已取走后暂留 publisher，核对原单槽/请求额度仍满、再准入拒绝且账目不变，释放后新代次恢复。可信 Rust fixture 不冒充生成代码 safepoint，也不把有限竞争覆盖写成长时稳定性或性能验收。

定向重复脚本首次在 release 的 Cargo JSON 顺序中误选同名 CLI harness，实际运行零个目标测试，计数断言立即拒绝，见 `/tmp/wings-scheduler-park-repeat-release-initial.log`；不计为通过。脚本修正为宿主明确匹配 lib target，并在宿主/客体执行前列出和核对两个非零期限测试，要求唯一匹配且每次实际通过数为一/二，保留原独立子进程限制。

macOS AArch64 的 debug/release 各 83 个 harness、554 项测试、doc-tests 及完整受限 AOT 通过；workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离、禁 unwind、Python 语法与 Markdown 链接/围栏检查通过。两种构建的三项新回归分别做 32 次独立子进程重复（16 次停车决定、16 次同时执行两个期限测试），每进程 CPU 20 秒/墙钟 30 秒/core 0，均实际执行目标数量并通过。真实 AOT 包含原生成代码/GC/失败和监督全部场景；显式 OTP 29 的 v5 六组计划到达/十个配置拒绝和 v9 全部 27/96/24/18/51 场景在两种构建中通过，原始 v5 输出在 `target/scheduler-park-open-loop-{debug,release}`。日志为 `/tmp/wings-scheduler-park-{tests,repeat,aot,beam}-debug.log`、对应 release 和 `/tmp/wings-scheduler-park-v9-beam-{debug,release}.log`；汇总检查为 `/tmp/wings-scheduler-park-evidence.log`。本片未重跑 v6/v8 长测，不把正确性通过写成 p99、吞吐或整机内存改进。

Linux x86-64 使用 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体。冻结包 `/tmp/wings-scheduler-park-linux.tar.gz` 包含 327 份源码、83 个 harness 与 61 个工具/示例，排除语法评审稿；554 项 release 测试、完整受限 AOT 和同参数的 32 次独立定向进程通过。与 macOS 两种构建合计 96 次定向进程，含 48 次停车决定回归及 96 次正期限测试执行；有限次数不作为完整并发状态空间证明。结束后全部 471 项 source/harness/binary hash 一致，宿主源码仅后补本验证记录不同，日志为 `/tmp/wings-scheduler-park-linux-{tests,aot,repeat,hashes,stop}.log` 与 `/tmp/wings-scheduler-park-host-sources-final.log`。客体已确认 Stopped；Linux 此轮无 BEAM，不把 TCG 或交叉构建计为最新原生 Linux CI、同负载性能/内存或完整 MVP 门禁。最终静态检查覆盖 110 份 Markdown、1082 个相对链接及围栏。

## 监督终态的字段释放顺序回归（2026-11-14）

本轮未发现新的运行时缺陷，补充 [ADR-0056](adr/0056-multi-worker-supervision.md) 已有清理交接契约的确定性回归，不改变执行、请求、监督、资源额度、ABI v15 或待审语法。可信 Rust fixture 在同一 registry 临界区为已配置计费堆的 actor 排入一条拥有 4096 字节独立载荷的消息和一个标量请求，再正常摘除 actor；不声称这等同于生成代码的实际失败、stop 或关闭路径覆盖。

回归组合覆盖一/二 worker，Stopped/Closed/Failed 三种终态元数据，以及调用方继续持有结果、已消费取消、已消费超时、直接丢弃四种生命周期，共 24 组。设置终态后持有终态票据锁，在另一线程销毁已摘除 actor，使最后的发布动作暂时无法完成；在原三秒阶段上限内核对进程、计费堆、字节载荷和 mailbox 消息额度已经释放。尚未消费的调用方可以合法保留一个请求额度，此时结果已经为 Failed 且只可消费一次，消费后旧 process 自身账目归零；释放票据锁后发布端才能完成，poll 只取一次对应终态且不重启。最终在所有句柄释放后核对 VM 账目归零，不把 last_exit 当作清理屏障。

另做一次未提交的字段顺序扰动：临时将 Actor 的终态发布端移到业务字段之前，同一受限回归在三秒上限内以 `termination blocked actor cleanup` 失败，记录在 `/tmp/wings-termination-order-mutation.log`；随后完整恢复生产字段顺序。该失败是验证测试敏感性的人工扰动，不是现有实现缺陷，也没有保留生产代码变更。恢复后的定向测试通过，日志为 `/tmp/wings-termination-order-focused.log`。

macOS AArch64 的 debug/release 各 83 个 harness、555 项测试及 doc-tests 通过；新回归在两种构建下各做 16 次独立子进程重复，每次实际执行唯一指定测试和全部 24 组组合，保留每进程 CPU 20 秒/墙钟 30 秒/core 0。workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离、禁 unwind、Python AST 和 Markdown 检查通过。日志前缀为 `/tmp/wings-termination-order-`，测试/重复分别为 `tests-{debug,release}.log` 和 `repeat-{debug,release}.log`，各检查单独记录。此处只新增 `cfg(test)` 验证和测试 fixture 可见性，不重跑生产 AOT/BEAM 负载，也不把终态元数据 fixture 当作新的真实执行或性能证据。

Linux x86-64 在原 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体中通过 83 个 release harness、555 项测试，以及同限制的 16 次独立定向进程。与 macOS 两种构建合计 48 次定向进程、1152 组清理组合执行；有限重复不代表完整并发状态空间证明。冻结包 `/tmp/wings-termination-order-linux.tar.gz` 保留 327 份源码、83 个 harness、61 个工具/示例，排除语法提案；结束后 471 项 source/harness/binary hash 一致，客体已确认 Stopped。日志为 `/tmp/wings-termination-order-linux-{tests,repeat,hashes,stop}.log`，状态回执为 `/tmp/wings-termination-order-linux-status.json`；宿主源核对为 `/tmp/wings-termination-order-host-sources.log`，冻结后仅追加本验证记录。静态检查覆盖 110 份 Markdown、1083 个相对链接和围栏。此轮仍没有最新原生 Linux CI、完整业务值/账本或同负载 BEAM 吞吐及整机内存门禁证据，M0/MVP 状态不变。

## 宿主类型化复制消息（2026-11-14）

按 [ADR-0094](adr/0094-typed-host-message-values.md)，ScalarWorker、ScalarSupervisor 和 SchedulerHandle 增加 `send_values`/`request_values`，以 MessageValue 传入 Int/Bool/Unit、RecordInt 的内容、IntList/Bytes 借用切片。完整检查类型及原 64 参数/64 整数/65536 字节合计边界后，复制到目标账户计费的独占载荷；Scheduler 在 registry 锁外复制，发布前重查完整 Pid/generation，准备期间旧目标退出/slot 复用/关闭都不能把副本送给新 actor。已有根、受管导入、请求终态和原资源上限保持，原始整数/JSON/IngressSender 仍不能提供引用或段编码，所有回复保持标量，语法评审稿不在本片采纳。

六项新单测覆盖精确 schema、全参数预检、零/最大/越界、最高 mask、混合规范分段、源修改独立性、三级准入失败、复制成功后 envelope 准入失败、队满/结果表满的无净预留、已消费取消仍保留目标发布票据、stale/复用/关闭与准备后队列变满的确定顺序交错，以及最终清理。新真实 AOT `examples/host-values.wings`/`host_values` 接入完整验证入口，以混合参数覆盖空值、满 64 KiB 字节、64 节点列表、GC、三类宿主入口、一/二 Scheduler worker、Bool/Unit 标量回复、超时/取消不撤回、目标导入失败与根启动基线清理。每阶段仍三秒，每独立测试/编译/AOT 进程仍 CPU 20 秒/墙钟 30 秒/core 0；不把功能验证写成性能结果。

保留两项测试配置纠正。首次三级准入 fixture 把三层字节上限设成相同值，未给上层计费元数据留空间，填充票据提前被 VM 拒绝；日志 `/tmp/wings-host-values-focused-4.log`，修正为逐层收窄的 8/4/1 MiB，使被测分配分别在指定层拒绝，并完整通过。首次新 AOT 将含 64 节点列表的正常 actor 配成八个堆槽，在 `65536/0/64` 输入上按预期资源规则返回 HeapExhausted；诊断确认 allocated=capacity=8，旧 state 未提交，日志 `/tmp/wings-host-values-aot-focused-debug.log` 与 `/tmp/wings-host-values-aot-initial-heap-diagnostic.log`。正常最大参数场景实际同时存活 64 列表节点、一个记录和两个字节对象，fixture 改为 68 槽；独立二槽目标仍须 Failed 且旧状态为零。没有修改 runtime 堆上限、三级资源总额、步数或 CPU/墙钟门槛，最初失败不删除、不计通过。

macOS AArch64 debug/release 各 84 个 harness、561 项测试、doc-tests 及完整受限 AOT 通过；debug 全部 harness 之后追加的队满交错断言又完整执行最新 245 项库测试，其他源码未变。两种构建各 16 次独立定向进程、48 次新测试执行通过，脚本先验证目标测试存在且每次实际执行数为四/二，防止零测试误报。workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离、禁 unwind、Python AST 和 Markdown 检查通过。日志前缀为 `/tmp/wings-host-values-`：`tests-{debug,release}.log`、`lib-debug-final.log`、`repeat-{debug,release}.log`、`aot-{debug,release}.log` 和各检查日志。本片没有重新采集 BEAM 长测，不将旧冻结测量写成最新原生双平台或双性能门禁结果。

Linux x86-64 在原 Ubuntu 24.04、2 vCPU/4 GiB、无宿主挂载的 QEMU TCG 隔离客体中通过 84 个 release harness、561 项测试、完整受限 AOT 和 16 次独立定向进程。三种构建合计 48 次定向进程、144 次新单测执行，不能据有限重复宣称完整并发状态空间覆盖。冻结包 `/tmp/wings-host-values-linux.tar.gz` 包含 331 份源码、84 个 harness、62 个工具/示例，排除语法评审稿；结束后 477 项 source/harness/binary hash 一致，客体已确认 Stopped。日志为 `/tmp/wings-host-values-linux-{tests,repeat,aot,hashes,stop}.log`，状态回执 `/tmp/wings-host-values-linux-status.json`；宿主核对为 `/tmp/wings-host-values-host-sources.log`，冻结后仅追加本验证记录。静态检查覆盖 111 份 Markdown、1102 个相对链接及围栏。TCG 只提供功能证据，最新原生 Linux CI、完整业务值/账本、长时故障与公平性和同平台同负载 BEAM 吞吐/整机内存双门禁仍未完成。

## 未完成门禁

- 最新源码的原生 Linux 构建/clippy/MSRV/doc-tests 和原生双平台 CI，不能用 QEMU TCG 或 macOS 交叉编译替代。
- 长时并发故障、完整长时开环、热点/公平性、过载/超时竞争及端到端诊断。双发送者安全加法各自 FIFO、有限准入/关闭、短时计划开环与 CPU/突发心跳业务分别由 ADR-0065/0067/0068/0070 补充，ADR-0092 增加并发失败前缀，仍缺完整性能验收。
- 相同业务/同平台同机器的 BEAM 吞吐与整机内存双目标。hot-queue 与 mixed-4k 各五对 30s/60s 只补长时集成；mixed-4k 两端均有拒绝/超时，仍缺 CPU 硬配额、完整平台内存、其余负载、饱和扫描和原生双平台。
- 更完整的业务值与资源账本闭环；当前初始 slot 分布及返回后固定编号迁移不是自适应 work stealing，语言 capability 查找与最多 256 pending 维护仍为有界扫描。
- TCP 运行期间缓冲边界已接入有限示例，但握手期内核容量、完整账本与原生平台硬配额仍不在该回归覆盖内。

M0/MVP 状态不变；不推送、发布或降低验收门槛。
