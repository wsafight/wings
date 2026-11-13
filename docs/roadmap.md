# 风险与里程碑

[ADR-0045](adr/0045-concurrent-resource-admission.md) 使三级账户和票据可跨线程提交/转移/归还；VM 共享锁是正确性基线，仍无跨线程 inbox、actor 迁移或 M:N 调度，不降低 MVP 公平性与性能门槛。

[ADR-0044](adr/0044-native-module-leases.md) 修复模块永久泄留，增加 VM 级加载预算、代码租约和归属检查；旧不计费工具入口、allocator/动态装载器内部开销仍不在完整资源门禁内，M1 与 MVP 尚未完成。

[ADR-0043](adr/0043-chunked-runtime-gc.md) 接入 16 节点/根的 GC 工作块、runtime 预算/取消和原栈暂停；模块加载、allocator 与其他长操作仍须分别审计，不等于所有 runtime 操作已满足公平性或 M0 完成。

[ADR-0042](adr/0042-managed-function-frames.md) 用非递归受管函数帧替代完全内联，接入逐帧剩余栈检查与跨帧精确根；递归/动态派发、完整业务值、外部 I/O、M:N 和双平台业务/性能门禁仍未完成。

[ADR-0041](adr/0041-worker-deadline-parking.md) 将私有 OS 等待接到单 worker 空闲检查与请求 deadline；外部有界 inbox、I/O reactor 和 M:N 仍未实现，不降低 MVP 门槛。

[ADR-0040](adr/0040-language-request-events.md) 接入有界异步整数请求事件；一般回复类型、关联 state、自动 OS 事件泵与双平台门禁尚未完成，保留完整 MVP 门槛。

[ADR-0039](adr/0039-local-process-capabilities.md) 接入同 worker 同模块创建与单向发送；Pid 暂不跨 state/消息，异步整数请求另见 ADR-0040，跨 worker 迁移与完整业务值仍未完成，不降低 MVP 门槛。

[ADR-0038](adr/0038-self-send-effects.md) 先验证语言自发送的调用点副作用与计费，ABI v8；同模块创建/发送由 ADR-0039 扩展，异步整数请求另见 ADR-0040，引用消息仍须完成，不降低 MVP 门槛。

[ADR-0037](adr/0037-immutable-int-lists.md) 增加有界不可变 IntList 及传递根，ABI v7；字符串、通用记录、引用消息和三级账本全覆盖仍未完成，不降低 MVP 门槛。

[ADR-0036](adr/0036-bounded-inline-functions.md) 先开放有界内联 fn，后端已由 ADR-0042 的真实受管帧替代；完整 M0 仍未完成，不跳过语言消息、业务值、外部事件与双平台 BEAM 门槛。

失败定位按 [ADR-0035](adr/0035-bounded-exit-diagnostics.md) 增加 ABI 故障分类及单槽最近退出快照；仍无跨线程聚合、历史或语言级监督，不把宿主诊断视为完整 MVP。

[ADR-0034](adr/0034-record-state-loops.md) 补充唯一 RecordInt state 的循环根证明，不开放任意引用 phi，也不跳过双平台及完整 M0 门槛；有界源码函数另见 ADR-0036。

宿主诊断先按 [ADR-0032](adr/0032-safe-point-diagnostics.md) 暴露单 worker 最近安全点的预算/堆统计；跨线程、故障历史与完整可试用诊断仍在 MVP 门槛内。

M0/M1 后续切片 [ADR-0027](adr/0027-persistent-record-state.md) 开放有界 RecordInt 持久状态；不改变 MVP 门槛，引用消息、业务控制流、完整语言消息交互、真实 I/O、M:N 及双平台 BEAM 对照仍须完成。

宿主恢复切片 [ADR-0028](adr/0028-supervised-record-heaps.md) 接入带堆进程的显式配置与全新状态重建，不替代语言监督或完整诊断门槛。

M0 条件控制流按 [ADR-0029](adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](adr/0033-record-conditional-roots.md) 验证标量/RecordInt 惰性合流与路径根；任意引用 phi、递归及动态派发仍需独立设计与根/栈证明。

条件输入补充 [ADR-0030](adr/0030-scalar-comparisons.md) 的标量比较，不改变后续循环、完整受管执行和双平台性能门槛。

回边执行按 [ADR-0031](adr/0031-scalar-state-loops.md) 与 [ADR-0034](adr/0034-record-state-loops.md) 验证唯一标量/RecordInt state 的 while，不因此开放任意引用 phi、动态调用或宣称 M0 完成。

[文档目录](../docs.md) · [项目概览](../README.md)

## 风险登记

| 风险 | 等级 | 缓解 |
|---|---|---|
| 性能与内存无法同时优于 BEAM | 高（产品） | 固定代表性负载，联合检查吞吐、尾延迟与 RSS；单项领先不算目标达成，不拼接不同场景的最优值 |
| 双平台 ABI、装载或 OS 行为分叉 | 高 | M0 同时打通 Darwin AArch64 与 Linux x86-64，平台专用路径可关闭，分别保留正确性与性能基线 |
| 激进优化破坏根、所有权、额度或消息顺序 | 高 | 逐项消融、交错与溢出测试；语义变化单独版本化，失败退回正确性基线 |
| LSP 与编译器语义分叉，或分析链引入重型后端 | 中 | 共用源码、语法和语义库，测试未保存与不完整输入；检查传递依赖、版本快照和取消行为 |
| 编译器/runtime 缺陷破坏受管边界 | 高 | 封闭语言、受控内建、访问检查、fuzz 与独立审阅；未知原生内存错误终止 VM 进程，不伪装成局部恢复 |
| 配额外分配或共享引用绕过计费 | 高 | 统一多级账户，分配前准入，迁移与清理保持计费；故障注入检查守恒 |
| safepoint 或长 runtime 操作导致饿死 | 高 | 优化后检查覆盖、不可抢占区段审计、heartbeat 混合负载；不以取消强制抢占换吞吐 |
| shadow stack、原子检查和 codec 吞掉收益 | 高 | 分项 profiling，同负载基线；不达价值门就缩小场景或停止 |
| 编译后端接入与 ABI 维护超预算 | 高 | M0 验证双平台 Cranelift AOT、根发布、恢复和调试链路；单前端与最小平台层，不保留 Rust 子集旁路 |
| GC 长停顿或根遗漏 | 高 | 小堆完整 GC 起步、显式精确根，先正确性后增量；扩大堆限额前重测最大不可抢占区段 |
| 冬眠频繁唤醒或共享结构编码膨胀 | 中 | 默认关闭，空闲阈值和冷却，限制展开大小；同时比较普通 Idle 与 BEAM 冬眠 |
| 代码卸载、状态与消息 schema 不一致 | 高 | Idle 边界、CodeLease、失败回退、协议兼容测试；默认不卸载，超版本额度滚动重启 |
| 原生子进程池被误当作完整安全沙箱 | 高 | 独立权限与资源审计，声明 worker 级故障范围；交付前直接拒绝不可信原生代码 |
| 单人/小团队长期维护成本 | 高（产品） | 试点优先，诊断前置，OTP oracle 只覆盖声明子集；扩展功能逐项决策，不承诺全兼容 |

## 里程碑

当前处于 M0：已有前端与单文件 LSP，AOT 已从原始标量切片扩展至有界记录/IntList、非递归 fn 调用帧、条件表达式、比较和标量/RecordInt state while。历史标量 Counter 在 macOS AArch64 与 Linux x86-64 均通过编译、链接、可信装载、连续状态调用及错误边界的 [CI 验证](https://github.com/wsafight/wings/actions/runs/43089909858)，不覆盖后续扩展。完整 safepoint、全部业务根、跨线程切换、外部 OS 事件集成与 BEAM 基线仍未完成，不能视为 M0 已通过。原始原型范围见 [ADR-0009](adr/0009-scalar-aot-prototype.md)。

`0.0.2` 的十个包均已发布，此后专注开发，不安排新版本。未发布源码已有取消/总预算、原栈恢复、[剩余栈检查](adr/0024-scalar-stack-boundary.md) 和有限宿主 worker/消息/请求/监督；[ADR-0025](adr/0025-aot-record-roots.md) 接入 compiler 精确根及进程计费 GC，[ADR-0026](adr/0026-language-temporary-records.md) 开放受限记录内建，[ADR-0027](adr/0027-persistent-record-state.md) 升至 ABI v6、开放持久 RecordInt 状态，[ADR-0029](adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](adr/0033-record-conditional-roots.md) 增加标量/记录分支，[ADR-0031](adr/0031-scalar-state-loops.md) 与 [ADR-0034](adr/0034-record-state-loops.md) 增加有预算回边，[ADR-0042](adr/0042-managed-function-frames.md) 将源码函数接到真实受管帧。尚无引用消息根、任意引用 phi、递归或动态派发，不等于完整 safepoint/GC 或 M0 验证。

资源方面，[ADR-0014](adr/0014-resource-reservations.md) 的三级四维账户附带预留、转移、Drop 守恒的[有界确定性回归](evaluation.md#独立资源账户回归)，[ADR-0045](adr/0045-concurrent-resource-admission.md) 增加同 VM 门闩下的跨线程原子准入、净转移和票据归还。独立记录堆、根表及宿主 worker 的活栈、mailbox、请求表已接入容量准入，[ADR-0044](adr/0044-native-module-leases.md) 增加共享模块租约及 VM 级加载预算；完整业务堆、allocator 元数据和外部 I/O 仍未覆盖，线程安全账本也不等于 M:N 或 M1 三级计费完成。后续须逐项在真实分配前准入、失败回滚并与资源同寿命清理。

[ADR-0015](adr/0015-owned-scalar-tasks.md) 的 owned 执行槽只是后续进程表的所有权前置，不改变下述 MVP 的阶段顺序或验收门槛。

- **M0：双平台执行模型与 BEAM 基线。** 在 macOS AArch64 与 Linux x86-64 选定代表性负载，固定 BEAM 版本与配置、性能和内存验收协议。确定产物格式、入口、启动与退出约定、对外 I/O 范围及受管契约。完成两个平台的 Cranelift AOT + safepoint + shadow stack + 切换恢复原型，验证链接、程序装载和 `kqueue`/`epoll` 唤醒。
- **M1：单 worker 的最小实现与内存账本。** 在两个首要平台交付 VM 与编译工具，包含最小语言、逻辑独立 arena、有限 inbox、非移动精确 GC、栈限额和三级计费。最小程序要能编译运行，消息和状态正确，局部超限返回受管错误，失败后不泄漏。按平台发布内存账本和同状态 BEAM 对照，优先试验紧凑 PCB、内联值与分配快路径，同时交付基础诊断。
- **M2：M:N 调度与性能对照。** 在两个平台完成原子轮询抢占、worker 唤醒、唯一执行权和迁移测试。按实验协议验收死循环、递归与 runtime 长操作的混合负载，分别发布相对 BEAM 的持续吞吐、尾延迟、活跃进程内存，以及根维护和切换成本。试验消息批处理、worker 本地分配与有界资源额度缓存，保留强制检查。
- **M3：空闲内存与可选冬眠。** 实现 Idle 冬眠、额度内恢复并处理唤醒竞争，按测量数据决定是否采用增量 GC、压缩引用和稀疏状态优化。验收两个平台普通 Idle/冬眠的完整成本曲线、GC 总时长及停顿、唤醒延迟，以及消息到达和编码失败的竞态测试。与 BEAM 的对应状态分别比较，并报告 macOS 压缩内存的影响。
- **M4：监督子集与生产试点。** 对照 OTP 测试 link/monitor、请求超时/取消和三种重启策略，在文档中列出差异。提供每进程/租户的内存、队列、运行时间、超额执行、GC、退出与清理诊断。试点验证性能、内存、迁移与恢复成本，两个首要平台分别发布适用的验收结果，热更新不是试点前提。
- **M5（可选）：版本化热更新。** 仅在试点有明确需求时实现 Idle 迁移、消息 schema 兼容和失败回退。验收：只有成功接受的消息才进入投递契约，不因升级静默丢弃；超限有明确错误，旧代码有引用就不卸载。`dlclose` 默认关闭。
- **M6（可选）：原生扩展子进程池。** 实现 IPC、worker 配额、退出检测和权限策略，验收崩溃与 OOM 注入不破坏父 runtime，明确同 worker 任务的共同故障范围。不以该阶段代替专业安全审计。
- **M7（分别立项）：额外平台组合与分布式。** Linux AArch64、macOS x86-64 等扩展须重跑 ABI、原子序、装载与上下文测试；分布式须先定义认证、计费与断连语义。两类工作互不作为对方或单节点产品的交付前提。

工具链与上述阶段同步推进：M0 确定 workspace 边界和可查询的容错前端；M1 提供 `wings-vm`、`wings-compile` 与首版 `wings-lsp`，支持文档同步、诊断、跳转、悬停和基础补全；M2 起完善增量分析与取消，再扩展引用查找、重命名和语义高亮。详细拆分见 [Crate 设计](crates.md)，已采纳的选择见 [ADR](adr/README.md)。

下一步继续 M0 执行/根边界与 BEAM 基线测试；语言扩展只接入完成契约和验证的必要切片，不同时铺开可选语法、平台或热更新。持续无法达标时，缩小场景或停止开发，受管契约不能为性能数字让步。

OS 唤醒按 [ADR-0016](adr/0016-worker-wakeup.md) 提供独立等待/通知接口，平台实现分别采用 kqueue 与 epoll/eventfd；[ADR-0041](adr/0041-worker-deadline-parking.md) 已接入 worker 就绪重查和请求 deadline，尚无外部 inbox 或真实 I/O，须在本分支 CI 各自验证，不替代完整 M0 与 BEAM 负载冻结门槛。

## MVP 范围与开发顺序

[ADR-0020](adr/0020-single-worker-mailbox.md) 提供宿主单 worker、generation Pid、有限 mailbox 与手动时间片轮转，过程/消息/活栈先准入再执行；[ADR-0021](adr/0021-bounded-requests.md) 补充固定表的异步 request/reply、deadline 和取消。它的语言自发送/同模块创建与发送另见 ADR-0038/0039，异步整数请求由 ADR-0040 扩展，真实 I/O 尚未接入，也不跳过 compiler 根和动态栈门槛，不能把宿主消息演示称为完整 MVP。

[ADR-0022](adr/0022-worker-deadline-preemption.md) 可选在 worker 执行 deadline 到达后请求原子让出，仍由原线程强制检查点恢复/挂起；默认无计时线程。ADR-0031 补充标量循环中的计时请求与有限轮转回归，但仍不覆盖 runtime 长操作、真实服务尾延迟、M:N 唯一执行权或迁移。

[ADR-0023](adr/0023-bounded-host-supervision.md) 提供第 6 项的宿主 one-for-one 恢复前置：新实例重启、滚动窗口强度上限、旧请求终结和结构化退出事件。[ADR-0032](adr/0032-safe-point-diagnostics.md) 补充最近安全点 GC/预算快照；尚无语言级监督、link/monitor 或跨线程/故障历史诊断，不跳过 M0/M1 或性能门禁。

[ADR-0018](adr/0018-bounded-record-heap.md) 先验证 Rust 侧不可变定长记录、精确 mark-sweep 和作用域根表；[ADR-0025](adr/0025-aot-record-roots.md) 增加 AOT 临时引用的根发布与堆所有权闭环；[ADR-0026](adr/0026-language-temporary-records.md) 将局部 RecordInt 接入语言/CLI/worker，堆与执行栈保持同 process 计费；[ADR-0027](adr/0027-persistent-record-state.md) 增加已提交状态根与候选出口保活。容量构造前三级准入，但引用消息根和通用业务值尚未集成，不视为完整 GC 或 M0 精确根验收完成。

BEAM 对照先固化 [v1 最小协议与 oracle](beam-workloads.md)：已在本机检查有界 Counter 的 i64、FIFO 和失败退出，代表负载组合与采样要求单独列明。未来多 actor、超时、过载及 Wings 等价端尚待实现，不因协议落盘而称 BEAM 基线全部完成。

[ADR-0017](adr/0017-scalar-frame-admission.md) 限制当前标量业务帧，64 KiB 以上在写产物前拒绝；[ADR-0024](adr/0024-scalar-stack-boundary.md) 增加薄入口的实际剩余空间检查，栈不足正常返回受管错误。同步与 initializer 也使用有界池，worker 初始化先准入且失败回滚。源码调用按 ADR-0042 逐帧检查，仍依赖固定可信慢路径余量，不包含递归；compiler 根链见 ADR-0025 与 ADR-0042。表中第 2 项与 M0 尚未完成。

MVP 指可在单机、小范围试用的微进程系统，不是 Counter 能运行或包已经发布。它取 M0/M1 的完整执行与资源边界、M2 的公平调度与对照验证，以及 M4 的最小恢复和诊断；不等于 M0–M4 全部能力一次交付。第 1 项已有实现并通过历史双平台 debug/release CI；第 4 项已推进至语言 RecordInt 临时/持久状态，仍未覆盖完整业务值与全部分配。

| 顺序 | 技术切片 | 最小验收 |
|---|---|---|
| 1 | 同线程活栈让出与恢复 | 生成代码在强制检查点让出；保留原栈和活跃值；取消、丢弃挂起调用不跨 ABI unwind，不重放 handler |
| 2 | 完成受管执行与 M0 | shadow stack、根发布、栈限额、双平台 OS 唤醒；先限定栈与堆，再开放必要的控制流、函数及分块内建；同时冻结 BEAM 负载与测量口径 |
| 3 | 单 worker 多进程与有限 mailbox | spawn、send、异步 request/reply、超时；唯一执行权、FIFO 的明确范围、队列满显式失败、无饿死；副作用不随 handler 失败回滚 |
| 4 | 业务值、GC 与三级资源账本 | 最小字符串/记录等业务值、每进程逻辑 arena、非移动精确 GC；堆、栈、消息、请求表在分配前准入，失败回滚预留，退出完整清理 |
| 5 | 公平调度与外部事件 | 计时线程原子请求让出、长操作有界；先验收单 worker，再验证 M:N 唯一执行权/迁移；用受限 I/O 适配器跑一个真实服务场景 |
| 6 | 最小故障恢复与诊断 | 退出原因、受控重启及重启强度上限；定位队列、预算、GC 和 handler 失败；不承诺完整 OTP 兼容 |
| 7 | 双平台业务与性能门禁 | 在各平台同机同负载对照 BEAM，吞吐与内存两项目标同时验证；含尾延迟、清理、过载与受限子进程故障测试，不用单项微基准替代 |

工程上先完成 1–4，得到单 worker 内部演示；只有 5–7 的边界和采用价值也得到验证后，才称为可试用 MVP。性能或内存目标未达成时保留结果、缩小适用负载或停止，不因功能齐全就宣布达标。

暂不纳入 MVP：热更新、冬眠、分布式、不可信原生扩展、额外平台、完整 OTP、完整 IDE 重构能力。已发布 `0.0.2` 不变；在 `feat/mvp` 按可验证技术切片多次提交，不为每个切片发布 crate。活栈切片的契约见 [ADR-0011](adr/0011-scalar-stackful-resume.md)。

## 相关文档

- [评测与验收](evaluation.md)定义实验条件、基线与正确性检查。
- [产品定位](product.md)说明何时继续、缩小范围或停止自研。
- [激进优化](optimizations.md)安排 M0–M4 的优化实验及需要独立设计的长期候选。
- [Crate 拆分](crates.md)安排编译前端、VM 与 LSP 的落地顺序。
