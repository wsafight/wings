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

## 未完成门禁

- 最新源码的原生 Linux 构建/clippy/MSRV/doc-tests 和原生双平台 CI，不能用 QEMU TCG 或 macOS 交叉编译替代。
- 并发失败、完整长时开环、热点/公平性、过载/超时竞争及端到端诊断。双发送者安全加法各自 FIFO、有限准入/关闭、短时计划开环与 CPU/突发心跳业务分别由 ADR-0065/0067/0068/0070 补充，仍缺完整性能验收。
- 相同业务/同平台同机器的 BEAM 吞吐与整机内存双目标。hot-queue 五对 30s/60s 只补长时集成；仍缺 CPU 硬配额、完整平台内存、其余负载、饱和扫描和原生双平台。
- 更完整的业务值与资源账本闭环；当前初始 slot 分布及返回后固定编号迁移不是自适应 work stealing，语言 capability 查找与最多 256 pending 维护仍为有界扫描。

M0/MVP 状态不变；不推送、发布或降低验收门槛。
