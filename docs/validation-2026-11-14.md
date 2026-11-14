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

## 未完成门禁

- 最新源码的原生 Linux 构建/clippy/MSRV/doc-tests 和原生双平台 CI，不能用 QEMU TCG 或 macOS 交叉编译替代。
- 并发失败、完整长时开环、热点/公平性、过载/超时竞争及端到端诊断。双发送者安全加法各自 FIFO、有限准入/关闭、短时计划开环与 CPU/突发心跳业务分别由 ADR-0065/0067/0068/0070 补充，仍缺完整性能验收。
- 相同业务/同平台同机器的 BEAM 吞吐与整机内存双目标。hot-queue 与 mixed-4k 各五对 30s/60s 只补长时集成；mixed-4k 两端均有拒绝/超时，仍缺 CPU 硬配额、完整平台内存、其余负载、饱和扫描和原生双平台。
- 更完整的业务值与资源账本闭环；当前初始 slot 分布及返回后固定编号迁移不是自适应 work stealing，语言 capability 查找与最多 256 pending 维护仍为有界扫描。

M0/MVP 状态不变；不推送、发布或降低验收门槛。
