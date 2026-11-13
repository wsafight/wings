# 2026-11-13 验证记录

此记录对应 `972c229`（含 [ADR-0049](adr/0049-record-message-copies.md)、[ADR-0050](adr/0050-account-node-admission.md)），只描述已执行的功能验证。不改变 [MVP 门槛](roadmap.md#mvp-范围与开发顺序)，没有吞吐、尾延迟或整机内存领先结论。

## macOS AArch64

- 宿主 macOS 26.6.2、AArch64，Rust 1.99.1；MSRV 另用 Rust 1.85.0。
- 通过 workspace check/test、clippy all-targets `-D warnings`、fmt 检查及 Rust 1.85.0 all-targets check。
- 通过 debug/release workspace 测试、全部受限 AOT 回归和 TCP 示例单测。TCP 专项两种构建各重复三次；BEAM oracle v1 的 11 个 checked/FIFO/exit 用例与 23 个非法输入通过，不代表 Wings 与 BEAM 已运行等价性能负载。
- 预编译入口分别以 `WINGS_PREBUILT_EXAMPLES=1`、绝对 `WINGS_TARGET_DIR` 运行 debug/release 全回归，保持每子进程 20 秒 CPU、30 秒墙钟和禁 core dump。

## Linux x86-64：隔离仿真功能验证

QEMU TCG 在 Apple Silicon 上运行独立 Ubuntu 24.04 x86-64 客体，2 个虚拟 CPU、4 GiB RAM、12 GiB 虚拟磁盘，不挂载宿主目录。内核 6.8.0-146-generic，glibc 2.39，GCC 13.3.0，Python 3.12.3。该环境是 Linux 内核/ABI 的功能验证，**不是原生 Linux 性能平台**。

Rust 1.99.1 在 macOS 交叉构建 `x86_64-unknown-linux-gnu`，Zig 0.15.2 作为 linker；Rust 标准库按发行清单 SHA-256 校验。Cranelift 生成的业务产物在客体内编译、由 GCC 链接成 `.so`、由 Linux VM 加载执行，不复用 macOS 动态库。交叉 linker 给 release 产物的兼容性提示为 `ignoring deprecated linker optimization setting '1'`，不视为 Linux 原生 clippy 已通过。

| 验证 | 实际结果 |
|---|---|
| debug 独立测试可执行文件 | 25 个 harness、397 项通过，0 失败 |
| release 独立测试可执行文件 | 25 个 harness、397 项通过，0 失败 |
| TCP 示例 debug 单测 | 2 项通过；与 release 独立测试一起执行时共 26 个 harness、399 项 |
| release 受限 AOT 全回归 | 通过；包括受管帧、RecordInt 复制/入口根、GC、跨线程入口/请求、TCP 与清理 |
| debug 受限 AOT 全回归 | 未通过：大型 `timer.wings` 编译在仿真中返回 `-9`，符合既有 CPU 上限终止；未放宽上限重标通过 |
| 混合 release 编译器/debug VM 与示例 | 未通过全套：debug `frame_limits` 同样返回 `-9`；不能当全 debug 通过 |
| Linux 原生构建、clippy、MSRV、doc-tests、release TCP 单测 | 本次未执行，仍由后续原生 CI 分别验证 |

测试 harness 从 `cargo test --workspace --target x86_64-unknown-linux-gnu --no-run --message-format=json` 中选择 `profile.test` 且有 `executable` 的产物；在客体内逐个以 `timeout 120`、禁 core 执行。CLI 测试内嵌编译时二进制绝对路径，客体只在相同路径建立指向已复制二进制的链接，不修改测试断言。不把这次运行称为客体内完整 `cargo test` 或双平台 CI 已通过。

## 复现入口与后续

### ABI v12 增量：`6e390af`

同一 macOS 宿主通过 check/clippy/fmt、Rust 1.85.0 all-targets check、debug/release workspace 各 403 项测试、两种构建的 TCP 示例单测及完整受限 AOT。新增列表消息覆盖全部 64 元素、超限拒绝、混合载荷与 800 个单步 stop/Drop 边界。

复用上述隔离 Linux x86-64 客体，从 `6e390af` 重新交叉构建 release compiler/VM/examples、workspace 测试及 **release TCP 示例单测**。26 个 harness 共 405 项通过；完整 release AOT 通过，包含实际 `.so` 编译/链接/加载、列表源根/目标重建、跨线程入口和 TCP。每业务子进程仍为 CPU 20 秒、墙钟 30 秒、禁 core，独立测试仍为 `timeout 120`；未使用仿真时间/内存作排名。

此次增量未重跑 Linux debug、原生 clippy/MSRV/doc-tests，也不替代原生双平台 CI。表中的 debug AOT 上限失败仍是旧代码测量，不能据本次 release 通过重标 debug 成功。

### 跨线程模块增量：`3529ed7`

macOS 的 debug/release workspace 各 406 项测试、完整受限 AOT、TCP 示例单测，以及 check/clippy/fmt/MSRV 检查通过。模块代码、Library 与预算现在位于同一个 Arc 控制节点，不使用两个可能并发析构的独立 Arc。

同一隔离 Linux 客体重新运行 release workspace 独立测试及 release TCP 单测：26 个 harness 共 408 项通过。完整 release 受限 AOT 通过，新增四线程独立 worker 共享已计费代码、单步恢复与状态隔离、跨 VM 拒绝、最后 handler 跨线程释放；没有放宽既有子进程限制。Linux debug/native CI 与性能仍未验证，不将该用例称 M:N。

### 无活栈镜像增量：`70c4cb9`

macOS debug/release workspace 各 409 项测试及 check/clippy/fmt/MSRV、TCP 示例单测通过。debug 完整受限 AOT 通过；release 首次在既有 TCP `frame_limit` 读取首回复时遇到 ConnectionResetError，未修改该测试或截止时间，重跑完整脚本通过。保留此偶发失败，不将一次重跑当稳定性验收。

隔离 Linux release 独立测试含 release TCP 单测共 26 个 harness、411 项通过；完整受限 AOT 通过。新增镜像在三个新线程重绑同账户池，持续保存/更新 IntList 根，不重跑初始化；错 VM/账户/池及预算拒绝不丢镜像，取消后失败标志保留，未接回镜像在新线程 Drop 后资源归零。没有 unsafe Send、活栈迁移、worker 执行权路由或性能结论；Linux debug/native 门禁仍未补齐。

```sh
cargo build --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" python3 scripts/verify_scalar_aot.py
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py
```

交叉构建时指定 `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` 和 `--target x86_64-unknown-linux-gnu`；把目标二进制及源码复制到隔离 Linux，`WINGS_TARGET_DIR` 指向客体内的 `debug`/`release` 父目录。不要把跨架构仿真的 CPU 时间、内存占用或预留字节用于 BEAM 性能排名。

### 多 worker 调度增量：`eff95a4`

基于 `da57324` 的本次实现，macOS debug/release workspace 各 421 项测试通过；check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check 与两种构建的 TCP 示例单测通过。新增 12 项 scheduler 内部回归，覆盖唯一 owner、返回后轮转迁移、并发生产者、固定代次、schema/载荷拒绝、队满、start 前准入失败保留队首、请求终态和销毁后结果租约。

两种构建的完整受限 AOT 均通过。`multi_worker` 分别用 1/2/3 个 worker 运行六个父 actor 及六个语言创建的子 actor，检查跨 worker RecordInt/IntList 消息、请求完成、GC、持久状态、活栈固定及忙 actor 不阻塞其他 actor。每业务子进程仍为 CPU 20 秒、墙钟 30 秒、禁 core。

保留失败记录：release 第一次完整回归在已有 TCP 非法帧用例得到 `invalid=2, io_errors=1`，与预期 `invalid=3` 不同；后续完整重跑通过。补齐初始化控制预留后，最终 release 回归又在已有 TCP overflow 场景遇到 ConnectionResetError，未修改 TCP 代码、断言或限制，完整重跑通过。不能将通过重跑写成无偶发失败或稳定性门禁通过。

隔离 Linux 从 `eff95a4` 重新交叉构建 release compiler/VM/examples、workspace 测试与 TCP 示例单测，26 个 harness 共 423 项通过；完整受限 release AOT 通过，包含实际 Linux `.so` 的多 worker 运行、消息/请求、迁移与清理。首次测试打包遗漏独立 `wings-lsp` 二进制，CLI help 用例报 NotFound；补传该未改动二进制后完整重跑全部 harness 通过，不将首次运行标成通过。

多 worker 监督、负载自适应调度、全资源账本、原生双平台最新 CI 和同负载 BEAM 吞吐/整机内存双目标仍未完成；固定共享扫描、编号轮转与每 worker 全容量栈池没有性能结论。Linux debug/native clippy/MSRV/doc-tests 本次仍未执行。M0/M1/MVP 均未完成。

### 多 worker 最近失败诊断增量：`270bf3f`

基于 `9f864f2`，macOS debug/release workspace 各 423 项测试、两种构建 TCP 示例单测、check、clippy all-targets `-D warnings`、fmt 与 Rust 1.85.0 all-targets check 通过；依赖隔离检查通过。两种构建的完整受限 AOT 首次运行均通过，既有 TCP 偶发失败记录仍保留，不据此宣称已解决。

新增两项单测验证跨 worker 失败覆盖、旧提交及预算分类、引用 state 不暴露 handle、成功/主动 stop/关闭保留和诊断副本不持有业务资源。`multi_worker` 增加真实循环失败的旧 state、worker 轮转、算术故障、旧 Pid 退休及其他 actor 继续执行检查。macOS 另在有 CPU/墙钟限制的子进程中重复运行全部 14 项 scheduler 测试 20 次通过。

隔离 Linux 从 `270bf3f` 重新构建全部 release 工具/示例及测试，26 个 harness 共 425 项通过；完整受限 release AOT 通过。每业务子进程仍限制 CPU 20 秒、墙钟 30 秒、禁 core。Linux debug/native 门禁本次未执行；固定单槽不是可靠恢复事件，多 worker 监督及 MVP 剩余门禁不变。

### 多 worker 宿主监督增量：`7805470`

基于 `910851f`，macOS debug/release workspace 各 427 项测试通过；check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check 与两种构建 TCP 示例单测通过。全部 18 项 scheduler 测试在有 CPU/墙钟上限的子进程中重复 20 次通过。

新增四项监督单测覆盖全新 state/Pid、有限重启窗口、八个并发失败各自终态、Never/stop、不自动重试资源或 initializer 失败、创建拒绝的票据回滚。两种构建的完整受限 AOT 首次均通过；`multi_worker` 额外验证持久 IntList 在失败后重建为空、旧请求终结与重启强度上限。每业务子进程仍为 CPU 20 秒、墙钟 30 秒、禁 core；既有 TCP 偶发失败证据不删除。

隔离 Linux 从 `7805470` 重新构建全部 release 工具/示例与独立测试，26 个 harness 共 429 项通过；完整受限 release AOT 通过，包含真实 Linux 动态库的列表状态失败重建与强度上限。未放宽既有 CPU/墙钟上限，本次未执行 Linux debug/native clippy/MSRV/doc-tests。

终态在旧 actor 资源清理后发布，监督依赖宿主 poll；它不是语言监督、持久恢复、可靠故障日志或完整 M4。MVP 原生双平台与同业务 BEAM 双目标仍待验收。

### 计费不可变字节值增量：ADR-0057

基于 `36ac6ec`，macOS debug/release workspace 各 440 项测试通过（新增 13 项）；两种构建的 TCP 示例各 2 项测试、完整受限 AOT、check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check 和依赖隔离检查通过。文档相对链接、代码围栏及 diff whitespace 检查通过。本次两种完整 AOT 首次均通过，已有 TCP 偶发失败记录不删除。

ABI v13 的 Bytes 是 0..=64 KiB 不可变叶节点，固定所有权表和独立数据票据向原进程计费；源根在每个复制块存活，失败/取消释放未发布 builder。单测覆盖三级拒绝回滚、跨堆发布拒绝、传递根与代次、GC 后数据额度恢复及存活旧值阻止新副本超额；`byte_buffers` 验证空/最大 state、fn/条件/回边、小堆 GC、512 组最大值暂停后取消/Drop、低预算、跨 worker 迁移和监督重建为空。

独立 `--check-byte-roots` 用 Python 字节模型在 compiler 检查点/heap 回调前收集，检查跨 fn、分支、循环的源/候选根及非法参数/算术失败不写输出、正常解链。每业务子进程继续使用 CPU 20 秒、墙钟 30 秒、禁 core，无放宽限制。

本段只记录 macOS 本机结果；Linux 本增量另行验证。Bytes 不等于字符串、通用记录或引用消息/回复，逻辑字节不是 RSS，原生最新双平台与同负载 BEAM 性能/整机内存门禁仍未完成，M0/MVP 状态不变。

隔离 Linux 从 `71fb8e2` 重新交叉构建 release 全部工具/示例与测试，26 个 harness 共 442 项通过（含 TCP 示例 2 项）；完整受限 release AOT 首次通过，包含真实 Linux `.so` 的字节构造、取消、独立根模型、迁移及监督空 state 重建。仍用 QEMU TCG 的 Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载，未放宽 CPU/墙钟上限。Linux debug/native clippy/MSRV/doc-tests 未在本次执行，仿真不能替代原生 CI 或吞吐/整机内存门禁。

### Bytes signed i64 大端访问增量：ADR-0058

基于 `633b3ef`，macOS debug/release workspace 各 445 项测试通过（新增 5 项）；两种构建 TCP 示例各 2 项、check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离与文档链接/围栏检查通过。ABI v14 增加固定八字节读与不可变整对象复制替换，context/effect 布局及故障码不变。

单测覆盖完整 i64 两端、正负数、未对齐及末尾 offset、短 buffer/极端 offset 不分配/不写输出、旧对象不变、精确 GC 和复制低预算清理。完整 AOT 增加 64 KiB state 末尾连续更新与一步恢复，独立字节根模型验证跨 fn 读写与失败候选不提交。CPU 20 秒、墙钟 30 秒、禁 core 和 TCP 启动等待均未放宽。

保留失败：debug 完整 AOT 前两次均在既有 TCP normal 场景的 5 秒启动等待超时；单独受限子进程探测能输出监听地址，未修改 TCP 或限制，第三次完整 debug 通过，完整 release 首次通过。不能据此宣称启动偶发失败已修复或稳定性门禁通过。Linux 本增量另行验证；编码原语不是 Wings/BEAM 同协议业务或性能结果，MVP 剩余门禁不变。

隔离 Linux 从 `86cd40e` 重新交叉构建所有 release 工具/示例及测试，26 个 harness 共 447 项通过（含 TCP 示例 2 项）；完整受限 release AOT 首次通过，覆盖 i64 两端、末尾八字节替换、逐步恢复及独立根模型。环境仍为无宿主挂载的 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB，CPU/墙钟限制未变；未执行 Linux debug/native clippy/MSRV/doc-tests，不将仿真成功写成原生 CI 或性能通过。

### 固定字节 Counter 同协议正确性增量：ADR-0059

基于 `313f56a`，macOS debug/release workspace 各 447 项测试通过（新增两项固定 Bytes 诊断回归）；两种构建 TCP/Counter 桥接示例各 2 项、check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check 和依赖隔离通过。文档链接/围栏与 diff whitespace 检查通过，CI 新增桥接示例单测，但未声称最新远端原生 CI 已运行。

真实 `verify_counter_equivalence.py --compare-beam` 在两种构建均通过：Wings AOT、OTP 29/ERTS 17.1.1 JIT 和独立 Python checked 模型核对 45 组业务、28 组 JSON 拒绝及 Wings 14 组内部桥接拒绝。包含三种完整字节 state、32 请求窗口、i64 两端及溢出优先、失败位置 1/32/33/256、旧提交诊断、FIFO、失败队尾终结及最终账目归零。原 BEAM 单独 oracle 的 11 业务/23 拒绝也通过。固定前缀诊断不导出所有 padding，完整状态复制另由既有字节原语回归支撑；不把有界闭环正确性称为开环性能/内存结果。

完整 debug 受限 AOT 首次通过；release 首次在既有 TCP bounded_read 场景得到 `io_errors=2`，预期 1（业务回复与清理均成功），未改 TCP、断言或限制，第二次完整通过。保留全部历史偶发记录，不宣称稳定性门禁已过。Counter 业务子进程 CPU 5 秒/墙钟 10 秒/禁 core、actor 总 deadline 3 秒，其他 AOT 子进程仍为 CPU 20 秒/墙钟 30 秒。

Linux 本增量另行验证；当前隔离 Linux 未安装 OTP，不以 Wings/模型模式代替真实 BEAM 对照。多 actor/开环、公平性/过载、资源账本闭环及原生最新双平台性能/整机内存双门禁仍未完成，MVP 状态不变。

隔离 Linux 从 `f5caedd` 重新交叉构建全部 release 工具/示例与测试，27 个 harness 共 451 项通过（workspace 447、TCP 2、Counter 桥接 2）；完整受限 release AOT 首次通过，内含真实 Linux `.so` 的 45 组 Wings/独立 Python 业务和 28 组 JSON/14 组桥接拒绝。仍为 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载，Counter 和其他 AOT 的 CPU/墙钟限制均未放宽。未执行 Linux OTP、debug/native clippy/MSRV/doc-tests；不将仿真或模型通过写成真实 Linux BEAM 对照、最新原生 CI 或性能通过。

### 调度实例共享宿主结果表增量：ADR-0060

基于 `3b95446`，macOS 26.6.2 AArch64、Rust 1.99.1 的 debug/release workspace 各 450 项测试通过；两种构建 TCP/Counter 桥接示例各 2 项、check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离和文档链接/围栏检查通过。新增回归覆盖跨 actor 混合类型共享上限、拒绝无副作用、消费后复用、关闭后结果、原 process 请求计费、三级拒绝及取消双端租约。并发八个子进程失败用例将声明的请求总容量从 4 改为 8，保留全部并发失败和独立终态断言，未降低覆盖。

release 同一个真实标量 Counter，2 worker、64 宿主结果槽、每 actor mailbox 1、VM/tenant 各 1 GiB 上限，每组单次冷启动，记录如下逻辑预留（B）：

| actor 数 | 修改前空表 | 修改后空表 | 修改前填满 | 修改后填满 | 修改前 actor 增量 | 修改后 actor 增量 |
|---|---|---|---|---|---|---|
| 16 | 16059720 | 16065200 | 16368328 | 16091952 | 308608 | 26752 |
| 64 | 43076040 | 43080368 | 44310472 | 43187376 | 1234432 | 107008 |
| 256 | 151141320 | 151141040 | 156079048 | 151569072 | 4937728 | 428032 |

每 actor 空闲控制增量从 19288 B 降为 1672 B；256 actor 填满总预留减少 4509976 B。空表仍包含按最大 actor 数逐 worker 预留的栈池，未移除该主要成本。所有组退出回到各自空表、最终预留归零。此数字不是 RSS、物理内存或 BEAM 对照，不包含 Bytes 业务状态，不外推为 1000/10000 actor 密度。完整 AOT 加入 `scheduler_density`，仅断言空闲控制增量不超过每 actor 4096 B 及完整清理，保留容量探测的边界。

完整 debug AOT 首次在既有 TCP invalid 场景读取合法请求回复时遇到 ConnectionResetError，未改 TCP、断言或截止时间，第二次完整通过；完整 release 首次通过。两种构建另行执行真实 Wings/OTP 29/独立 Python 的 45 业务、28 JSON 拒绝和 14 桥接拒绝对照，均通过。Counter 业务子进程仍为 CPU 5 秒/墙钟 10 秒/禁 core、actor 总 deadline 3 秒，其他 AOT 子进程 CPU 20 秒/墙钟 30 秒/禁 core；密度退出 deadline 3 秒，不放宽历史限制。

Linux 本增量另行验证。`config.requests` 现为全实例宿主结果总槽数，语言 pending 表仍独立；不把共享表锁、固定 registry 扫描、逐 worker 全容量栈池或 256 actor 上限当最终可扩展方案。多 actor/开环、完整业务值/资源计费、原生最新双平台与同负载 BEAM 吞吐/整机内存门禁仍未完成，MVP 状态不变；待审语法草案不在本提交范围。

隔离 Linux 从 `596fb8b` 重新交叉构建 release 全部工具/示例与测试，27 个 harness 共 454 项通过（workspace 450、TCP 2、Counter 桥接 2）；完整受限 release AOT 首次通过，包含 16/64/256 actor 密度清理、跨 worker 业务及 45 组 Wings/独立模型 Counter。环境仍为 QEMU TCG Ubuntu 24.04 x86-64、2 vCPU/4 GiB、无宿主挂载，业务 CPU/墙钟限制不变；未执行 Linux OTP、debug/native clippy/MSRV/doc-tests，不将仿真结果当原生 CI、BEAM 比较或性能证据。
