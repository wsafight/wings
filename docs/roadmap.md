# 风险与里程碑

[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 推进第 4 项根/门闩控制存储：新增启动前计费根、强制其子节点准入，CLI 堆模式/TCP 清理回到存活根基线。旧工具入口、完整业务值/账本、长时公平性与原生双平台 BEAM 双门禁仍未完成，MVP 未完成。

[ADR-0078](adr/0078-request-publisher-lifetimes.md) 修正第 3 项内部请求在超时/取消结果取走、完成事件入队或发起方退出后的提前回收；槽和请求票据保持到目标消息完成/清理。仍缺完整业务值/账本、长时公平性、CPU 硬配额、最新原生双平台与 BEAM 吞吐/整机内存双门禁，MVP 未完成。

[ADR-0077](adr/0077-streamed-fairness-measurement.md) 为第 5/7 项接独立长时 CPU/突发心跳采样，保留原业务、255+1 pending 和短测边界。本机 30s/60s 五对及单独观测已完成；Wings 4/5、BEAM 5/5 通过尾延迟候选，但有 291 次队满、观测采样无数据，CPU 硬配额/过载分组/原生双平台仍不完整；MVP 未完成。

[ADR-0076](adr/0076-accounted-byte-messages.md) 推进第 3/4 项的 Bytes 消息：单条合计 64 KiB、源/目标分别准入、发布时净转移、分块复制、入口根及取消/超时/失败清理。ABI v15 不改类型拼写。字符串/通用记录、引用回复、全部资源账本、长时公平性和原生双平台 BEAM 双门禁仍待完成，MVP 未完成。

[ADR-0075](adr/0075-platform-memory-observation.md) 补第 7 项的 Linux PSS/macOS physical footprint 与主机压缩/swap 原始采样，保留身份/错误/缺失与监测成本。平台口径不混算，仍非完整整机内存、CPU 硬配额、原生双平台或 BEAM 双门禁验收，MVP 未完成。

[ADR-0074](adr/0074-bounded-concurrent-recovery.md) 补第 3/6 项的一/双 worker 有界并发取消/超时、排队保留、stop 竞争、候选 GC/旧提交状态与全新重启回归。仍未覆盖所有故障交错、完整业务值/账本、长时公平性和原生双平台 BEAM 双门禁，MVP 未完成。

[ADR-0073](adr/0073-reusable-initializer-stack-pool.md) 针对 Linux 隔离仿真大容量启动的原 3 秒失败，增加宿主显式复用计费初始化池。v2 的 actor 数、状态、窗口、配额与所有 deadline 保持不变，完整回归结果见验证记录；此资源复用不替代原生双平台、完整业务值/账本和 BEAM 吞吐/整机内存门禁，MVP 未完成。

[ADR-0072](adr/0072-scheduler-yield-observation.md) 接入第 5 项的可选 Scheduler timer 请求/真实返回观测、有限 FIFO 与完整丢样/清除计数，默认关闭。v7 原业务的独立 `--observe` 不提高步骤片或 quantum 来凑样本；仍缺长时公平性、诊断开销/样本完整性、CPU/平台内存和原生双平台门禁，MVP 未完成。

[ADR-0071](adr/0071-observed-atomic-yield.md) 补可选计费的原子请求/真实 coroutine 返回时间原语及独立 AOT 回归。默认不增加时钟/诊断锁，仍未接为完整 Scheduler 让出统计；长时公平性、样本覆盖与开销、CPU/平台内存和原生双平台门禁继续推进，MVP 未完成。

[ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 接入 fairness 行的短时 CPU/突发心跳业务前置，保留百万次 checked 加法、101×256 B、mailbox 1、原每 actor 10ms 心跳；不以低频或标量 state 替代。OS 单核硬配额、长时公平性、独立让出延迟和原生双平台仍未完成，不能据功能回归宣布 MVP 达标。

[ADR-0069](adr/0069-streamed-open-loop-measurement.md) 新增长时开环测量入口，固定 30s/60s/五次配对重复，不缩短验收；原始事件、离线精确分位及进程树 RSS 仍非完整性能门禁。CPU 硬配额、平台完整内存、fairness、故障竞争、完整业务值/账本和最新原生双平台继续推进，MVP 未完成。

[ADR-0068](adr/0068-bounded-open-loop-counter.md) 补四组代表负载的短时计划开环，保留迟发、拒绝、超时及迟到实际回复，核对最终提交与有界 pending；不是长时性能验收。预热 30 秒/采样 60 秒/五次重复、饱和扫描、fairness、并发失败竞争、完整业务值/账本、最新原生双平台与同负载吞吐/整机内存双门禁仍未完成。

[ADR-0067](adr/0067-beam-bounded-admission.md) 补齐可信 BEAM 发送前有限准入/开始归还/关闭，在 1/100/256 满队列及健康 actor 上与 Wings 做有界正确性对照；不绕开原热点 mailbox 100。开环、并发失败/超时、热点/公平性、完整业务值/账本、最新原生双平台和 BEAM 吞吐/整机内存门禁仍待验收。

[ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 补 hot-queue 所需的 mailbox 100 准入前置，Scheduler 可配置上限 256，保留固定槽计费/拒绝/活栈与取消清理。BEAM 有限准入正确性见 ADR-0067；开环采样及热点/公平性双门禁仍待推进，不能因可排满 100 条就宣称性能或 MVP 达标。

[ADR-0065](adr/0065-two-sender-counter-fifo.md) 补齐安全加法 Counter 的两个真实发送者、各自 FIFO 与合法交错正确性；不是跨发送者全序，也不覆盖并发失败/过载/超时。闭环功能通过仍不能跳过完整业务值/资源账本、开环/热点/公平性、最新原生双平台及同负载 BEAM 吞吐/整机内存门禁。

[ADR-0064](adr/0064-indexed-supervisor-slots.md) 消除监督批量创建/查询/停止的容量扫描，针对多 actor Linux 隔离仿真的 3 秒失败补充回归；poll 仍为固定容量扫描。M0/MVP、原生双平台和同业务 BEAM 双目标门槛不变。

[ADR-0063](adr/0063-multi-actor-byte-counter.md) 补齐代表性 actor 数量的完整 Bytes Counter 闭环正确性：三端核对各目标状态/FIFO/失败与清理，未改变 ABI 或语法。安全加法双发送者见 ADR-0065；开环/热点/公平性、完整业务值/资源账本、最新原生双平台及同负载 BEAM 吞吐/整机内存双门禁仍待推进，MVP 未完成。

[ADR-0062](adr/0062-sparse-ready-scheduler.md) 去除全 actor 派发与本地空执行槽扫描，使用固定 intrusive 就绪 FIFO、本地活栈 FIFO 和空槽链；actor 容量扩至 16384，请求表各最多 256 不变。1000/10000 actor 标量稀疏功能探测接入完整受限 AOT，完整 Bytes 多 actor 正确性见 ADR-0063；开环/热点/公平性、自适应负载、原生最新双平台及 BEAM 吞吐/整机内存双门禁仍待推进，不因容量可创建而称 MVP 完成。

[ADR-0061](adr/0061-on-demand-scheduler-stacks.md) 消除逐 worker 全 actor 容量的空闲栈映射，保留完整单栈/guard 及借入前三级准入、活栈线程固定和最多两个已返回栈的缓存。冷启动准入失败仍保留原 FIFO，额度释放后重试；未降低同时活栈上限。后续扫描/容量由 ADR-0062 改进，逻辑预留减少不替代代表性同业务 BEAM 吞吐/整机内存门禁。

[ADR-0060](adr/0060-shared-scheduler-reply-table.md) 先消除多 actor 空闲元数据的重复宿主结果表，固定实例总容量并保留逐目标三级请求计费；有界密度探测接入完整 AOT 回归。后续栈安排及扫描/容量由 ADR-0061/0062 改进，不据此宣布 M:N、M0 或 MVP 完成。

[ADR-0059](adr/0059-byte-state-counter-equivalence.md) 接入 Wings/BEAM v1 的同协议正确性对照：真实固定字节 state、有限异步请求窗口、FIFO、checked i64、失败退出与清理，不再用标量/列表替代业务。独立模型与真实 OTP 结果另行核对，未完成多 actor/开环、热点/公平性、原生双平台最新 CI 或同负载性能/整机内存门禁，MVP 状态不变。

[ADR-0058](adr/0058-byte-i64-big-endian-access.md) 为固定字节 state 增加完整 i64 大端读写，避免业务以多次整对象单字节复制模拟一次更新。ABI v14，沿原根/预算/额度契约；后续仍需真正的异步窗口、失败退出、BEAM 同协议及双平台性能/内存验证，不提前提高里程碑状态。

[ADR-0057](adr/0057-accounted-immutable-bytes.md) 推进第 4 项的实际字节状态：0..=64 KiB 不可变 Bytes、精确叶节点 GC、原账户数据预留、分块构造/复制、取消清理与迁移/监督回归。ABI v13，旧产物重编译，语法大小写不变。Bytes 消息另由 ADR-0076 接入；字符串、通用记录、引用回复、全部资源账本及原生双平台/同业务 BEAM 双目标仍待完成，不改变 M0/MVP 验收门槛。

[ADR-0056](adr/0056-multi-worker-supervision.md) 接入第 6 项的宿主有界 one-for-one 恢复：独立终态交接、全新实例、有限滚动强度与失败回滚。依赖宿主 poll，不覆盖语言监督、link/monitor 或可靠日志；原生最新双平台、完整资源/业务值和同负载 BEAM 双目标仍待验收，MVP 状态不变。

[ADR-0055](adr/0055-multi-worker-exit-diagnostics.md) 补第 6 项的跨 worker 最近失败定位，不保留旧堆/栈/账户，也不把可覆盖缓存当可靠恢复事件。有界终态交接与宿主恢复由 ADR-0056 独立接入；该切片不改变 MVP 验收门槛。

[ADR-0054](adr/0054-bounded-multi-worker-scheduler.md) 实现第 5 项的有界 M:N 集成切片：稳定 Pid、唯一活栈 owner、返回后迁移和跨 worker 语言消息/请求。初始扫描/容量由 ADR-0062 替代，仍无自适应窃取；宿主监督集成见 ADR-0056。原生双平台最新验证、完整资源与业务值和同负载 BEAM 双目标仍是独立门禁，不能据此宣布 M0/MVP 完成。

[ADR-0053](adr/0053-detached-process-images.md) 提供无活栈进程镜像的独占线程转交，验证原 state/堆/额度在目标同账户栈池继续执行；镜像本身不携带 worker Pid/mailbox/request，不转交挂起活栈。调度集成由 ADR-0054 扩展，完整第 5 项仍未验收。

[ADR-0052](adr/0052-cross-thread-module-leases.md) 去除计费模块 Rc 租约的原线程限制，只读模块/handler 自动 Send/Sync，可供不同线程创建的独立 worker 共享；该模块切片本身没有 actor 唯一执行权转移，调度另由 ADR-0054 接入，不提前改变第 5 项整体验收状态。

[ADR-0051](adr/0051-bounded-int-list-messages.md) 接入 ABI v12 的有界 IntList 消息、源遍历检查与目标重建根：每消息合计最多 64 个整数元素，不是任意业务图或引用回复。该切片须独立执行各平台回归，旧 Linux 记录不覆盖此变更；M:N、完整业务值及同业务 BEAM 双目标仍未验收。

[2026-11-13 验证记录](validation-2026-11-13.md) 保存 Linux x86-64 隔离仿真功能证据：`6e390af` 的 release 独立测试与 AOT 全回归通过；较早源码的 debug AOT 大型编译触及既有 CPU 上限，未重标成功。原生双平台和 BEAM 门禁仍未完成，不放宽既有资源限制。

[ADR-0050](adr/0050-account-node-admission.md) 接入子账户节点的父路径预留与末持有者清理，worker 默认计费进程账户；[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 接入根/门闩计费入口。旧工具树、执行入口统一及 allocator/动态装载器内部开销仍未闭环，M1 与 MVP 资源门禁仍未完成。

[ADR-0049](adr/0049-record-message-copies.md) 接入 ABI v11 的 RecordInt 语言消息复制、目标入口根及整数请求参数，列表消息由 ADR-0051 扩展，仍无引用回复、通用业务值或 M:N；M0/M1 和同业务 BEAM 双目标门禁仍未完成。

[ADR-0048](adr/0048-bounded-loopback-service.md) 增加真实 loopback TCP Counter 验证示例，有界连接/帧/截止时间及预算清理；单适配线程、Int state 尚不等价于 BEAM 状态/到达负载。外部 I/O 已从原语推进至可运行示例，完整服务公平性、M:N 和双平台性能/内存仍未验收。

[ADR-0047](adr/0047-cross-thread-ingress-requests.md) 为外部有界入口增加 Int/Bool/Unit 请求结果与双端清理；受限适配器由 ADR-0048 验证，通用业务值、M:N 与同业务服务的双平台性能门禁仍须完成。

[ADR-0045](adr/0045-concurrent-resource-admission.md) 使三级账户和票据可跨线程提交/转移/归还；VM 共享锁是正确性基线，有界入口由 ADR-0046 接入，仍无 actor 迁移或 M:N 调度，不降低 MVP 公平性与性能门槛。

[ADR-0044](adr/0044-native-module-leases.md) 修复模块永久泄留，增加 VM 级加载预算、代码租约和归属检查；旧不计费工具入口、allocator/动态装载器内部开销仍不在完整资源门禁内，M1 与 MVP 尚未完成。

[ADR-0043](adr/0043-chunked-runtime-gc.md) 接入 16 节点/根的 GC 工作块、runtime 预算/取消和原栈暂停；模块加载、allocator 与其他长操作仍须分别审计，不等于所有 runtime 操作已满足公平性或 M0 完成。

[ADR-0042](adr/0042-managed-function-frames.md) 用非递归受管函数帧替代完全内联，接入逐帧剩余栈检查与跨帧精确根；递归/动态派发、完整业务值、外部 I/O、M:N 和双平台业务/性能门禁仍未完成。

[ADR-0041](adr/0041-worker-deadline-parking.md) 将 OS 等待接到单 worker 空闲检查与请求 deadline；ADR-0046 增加有界标量入口，完整 I/O reactor 和 M:N 仍未实现，不降低 MVP 门槛。

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

[ADR-0046](adr/0046-bounded-cross-thread-ingress.md) 接入单 actor 单入口的有界计费跨线程发送与 OS 通知租约，仍由原 worker 执行业务；不代表 M:N、真实服务或完整外部 I/O 验收。

当前处于 M0：已有前端与单文件 LSP，AOT 已从原始标量切片扩展至有界记录/IntList、非递归 fn 调用帧、条件表达式、比较和标量/RecordInt state while。历史标量 Counter 在 macOS AArch64 与 Linux x86-64 均通过编译、链接、可信装载、连续状态调用及错误边界的 [CI 验证](https://github.com/wsafight/wings/actions/runs/43089909858)，不覆盖后续扩展。完整 safepoint、全部业务根、跨线程切换、外部 OS 事件集成与 BEAM 基线仍未完成，不能视为 M0 已通过。原始原型范围见 [ADR-0009](adr/0009-scalar-aot-prototype.md)。

`0.0.2` 的十个包均已发布，此后专注开发，不安排新版本。未发布源码已有取消/总预算、原栈恢复、[剩余栈检查](adr/0024-scalar-stack-boundary.md) 和有限宿主 worker/消息/请求/监督；[ADR-0025](adr/0025-aot-record-roots.md) 接入 compiler 精确根及进程计费 GC，[ADR-0026](adr/0026-language-temporary-records.md) 开放受限记录内建，[ADR-0027](adr/0027-persistent-record-state.md) 升至 ABI v6、开放持久 RecordInt 状态，[ADR-0029](adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](adr/0033-record-conditional-roots.md) 增加标量/记录分支，[ADR-0031](adr/0031-scalar-state-loops.md) 与 [ADR-0034](adr/0034-record-state-loops.md) 增加有预算回边，[ADR-0042](adr/0042-managed-function-frames.md) 将源码函数接到真实受管帧。已有 RecordInt 复制消息/入口根，列表消息根由 ADR-0051 接入，尚无引用回复、任意引用 phi、递归或动态派发，不等于完整 safepoint/GC 或 M0 验证。

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

OS 唤醒按 [ADR-0016](adr/0016-worker-wakeup.md) 提供独立等待/通知接口，平台实现分别采用 kqueue 与 epoll/eventfd；[ADR-0041](adr/0041-worker-deadline-parking.md) 已接入 worker 就绪重查和请求 deadline，ADR-0046/0047 增加外部有界标量 ingress/结果，ADR-0048 验证有限 loopback I/O。各平台须验证最新源码，不替代完整 M0 与 BEAM 负载冻结门槛。

## MVP 范围与开发顺序

[ADR-0020](adr/0020-single-worker-mailbox.md) 提供宿主单 worker、generation Pid、有限 mailbox 与手动时间片轮转，过程/消息/活栈先准入再执行；[ADR-0021](adr/0021-bounded-requests.md) 补充固定表的异步 request/reply、deadline 和取消。它的语言自发送/同模块创建与发送另见 ADR-0038/0039，异步整数请求由 ADR-0040 扩展，真实 I/O 尚未接入，也不跳过 compiler 根和动态栈门槛，不能把宿主消息演示称为完整 MVP。

[ADR-0022](adr/0022-worker-deadline-preemption.md) 可选在 worker 执行 deadline 到达后请求原子让出，仍由原线程强制检查点恢复/挂起；默认无计时线程。ADR-0031 补充标量循环中的计时请求与有限轮转回归，但仍不覆盖 runtime 长操作、真实服务尾延迟、M:N 唯一执行权或迁移。

[ADR-0023](adr/0023-bounded-host-supervision.md) 提供第 6 项的宿主 one-for-one 恢复前置：新实例重启、滚动窗口强度上限、旧请求终结和结构化退出事件。[ADR-0032](adr/0032-safe-point-diagnostics.md) 补充最近安全点 GC/预算快照；尚无语言级监督、link/monitor 或跨线程/故障历史诊断，不跳过 M0/M1 或性能门禁。

[ADR-0018](adr/0018-bounded-record-heap.md) 先验证 Rust 侧不可变定长记录、精确 mark-sweep 和作用域根表；[ADR-0025](adr/0025-aot-record-roots.md) 增加 AOT 临时引用的根发布与堆所有权闭环；[ADR-0026](adr/0026-language-temporary-records.md) 将局部 RecordInt 接入语言/CLI/worker，堆与执行栈保持同 process 计费；[ADR-0027](adr/0027-persistent-record-state.md) 增加已提交状态根与候选出口保活。容量构造前三级准入，RecordInt/IntList 消息根按 ADR-0049/0051 接入，但引用回复和通用业务值尚未集成，不视为完整 GC 或 M0 精确根验收完成。

BEAM 对照已固化 [v1–v8 协议与代表负载](beam-workloads.md)：ADR-0059 接入单 actor，ADR-0063 增加三种完整 Bytes 状态的代表性多 actor 数量，显式三端核对 i64、每目标 FIFO、独立失败退出和旧提交值；ADR-0065 增加安全加法下两个真实发送者的各自 FIFO/合法交错，ADR-0067 增加有限准入/满队列关闭，ADR-0068 增加短时计划开环，ADR-0069 增加独立长时采样入口，ADR-0070/0077 增加同 worker CPU/突发心跳短时业务及独立长测。正确性和部分测量仍不等同完整基线；并发失败、超时竞争、CPU/平台内存口径和双平台性能门禁尚未完成。

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
