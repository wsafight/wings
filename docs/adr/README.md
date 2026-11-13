# 架构决策记录

[文档目录](../../docs.md) · [架构设计](../architecture.md) · [Crate 拆分](../crates.md)

ADR 记录为什么做出某项架构选择、考虑过哪些替代方案，以及什么情况下需要重审。当前都属于设计决策，采纳不表示实现已经完成或性能已经验证。

## 当前决策

| 编号 | 决策 | 状态 |
|---|---|---|
| [0001](0001-vm-boundary-and-naming.md) | VM 的职责与命令入口 | 已采纳（设计） |
| [0002](0002-language-and-aot.md) | 单前端、Cranelift AOT 与独立编译工具 | 已采纳；消息语法部分由 0007 替代 |
| [0003](0003-process-and-resource-model.md) | 返回式 handler、独立业务状态与多级资源准入 | 已采纳（设计） |
| [0004](0004-safepoints-and-gc.md) | safepoint 轮询、显式 shadow stack 与精确非移动 GC | 已采纳（设计） |
| [0005](0005-platforms-and-benchmarks.md) | macOS/Linux 并列首要平台，以 BEAM 为性能和内存主基线 | 已采纳（设计） |
| [0006](0006-workspace-and-lsp.md) | workspace 按编译、执行与分析拆分，LSP 共用语言分析库 | 已采纳（设计） |
| [0007](0007-message-handlers-and-state.md) | 统一 `on`、类型化消息、具名状态与尾表达式回复 | 已采纳；前端子集已实现 |
| [0008](0008-wings-naming.md) | Wings VM 的项目、包、工具与源码命名约定 | 已采纳；前端已采用 |
| [0009](0009-scalar-aot-prototype.md) | 有界标量 IR、Cranelift AOT、可信本机装载与状态提交原型 | 已采纳（M0 原型）；完整受管模型待验证 |
| [0010](0010-scalar-execution-control.md) | 标量 ABI v2、强制原子取消检查与执行步数预算 | 已采纳（M0 原型）；替代 0009 的 ABI v1 |
| [0011](0011-scalar-stackful-resume.md) | 标量 ABI v3、同线程原栈让出恢复及无 unwind 取消清理 | 已采纳；布局与同步宿主栈安排由 0024 替代，恢复契约保留 |
| [0012](0012-bounded-stack-pool.md) | 自主管理 mmap、guard page、准入、缓存与回收的有界栈池 | 已采纳（M0 原型设计）；替代 0011 的独立栈分配策略 |
| [0013](0013-shared-scalar-modules.md) | 共享只读模块、独立实例状态与预解析 handler ID | 已采纳（M0 原型设计） |
| [0014](0014-resource-reservations.md) | 单 worker 三级资源预留、回滚与净额度转移 | 已采纳（实现契约） |
| [0015](0015-owned-scalar-tasks.md) | 按值持有实例与 invocation 的同线程执行槽，无自引用 | 已采纳（M0 实现契约） |
| [0016](0016-worker-wakeup.md) | epoll/eventfd 与 kqueue 用户事件的单 waiter 通知 | 已采纳（M0 实现契约） |
| [0017](0017-scalar-frame-admission.md) | 机器码生成后、写产物前的标量编译帧准入 | 已采纳（M0 有界标量切片） |
| [0018](0018-bounded-record-heap.md) | 有界不可变记录、精确 mark-sweep 与作用域根帧原语 | 已采纳（独立原型，未接入 AOT） |
| [0019](0019-accounted-stack-pools.md) | 租户预留栈池容量、活栈净转移到进程及缓存计费 | 已采纳（单线程接入契约） |
| [0020](0020-single-worker-mailbox.md) | 有限标量 mailbox、generation Pid 与单线程轮转执行 | 已采纳（宿主调度切片） |
| [0021](0021-bounded-requests.md) | 固定 pending 表、generation 请求 ID、回复与超时终结 | 已采纳（宿主请求切片） |
| [0022](0022-worker-deadline-preemption.md) | 可选 worker 计时线程、单调 deadline 与原子让出 | 已采纳（标量计时切片） |
| [0023](0023-bounded-host-supervision.md) | 宿主 one-for-one 恢复、有限重启窗口与新实例身份 | 已采纳（单 worker 恢复切片） |
| [0024](0024-scalar-stack-boundary.md) | ABI v4、业务帧前剩余栈检查及统一池内入口 | 已采纳；context 由 0025 扩展，栈边界保留 |
| [0025](0025-aot-record-roots.md) | ABI v5、内部 IR 临时记录、编译器精确根及进程计费堆 | 已采纳（M0 集成切片）；替代 0024 的 context 布局 |
| [0026](0026-language-temporary-records.md) | 语言临时 RecordInt、受限内建调用与同账户堆/栈执行 | 已采纳（M0/M1 语言集成切片） |
| [0027](0027-persistent-record-state.md) | ABI v6、RecordInt 状态、宿主持久根与候选提交 | 已采纳；替代 0025/0026 的状态限制，0037 升至 v7 |
| [0028](0028-supervised-record-heaps.md) | 显式监督堆配置、全新堆重建及失败回滚 | 已采纳（宿主恢复集成切片） |
| [0029](0029-scalar-conditional-expressions.md) | 无回边标量 if 表达式、结构化分支与检查点 | 已采纳（M0 控制流前置切片） |
| [0030](0030-scalar-comparisons.md) | 标量相等、有符号整数比较与规范 Bool | 已采纳（M0 表达式切片） |
| [0031](0031-scalar-state-loops.md) | 标量 state while、回边检查及跨迭代临时根 | 已采纳（M0 回边执行切片） |
| [0032](0032-safe-point-diagnostics.md) | 按值复制安全点预算/堆统计及单 worker 只读诊断 | 已采纳（M0 宿主诊断切片） |
| [0033](0033-record-conditional-roots.md) | RecordInt 惰性条件合流、路径根恢复与合流前发布 | 已采纳；替代 0029 的标量结果限制 |
| [0034](0034-record-state-loops.md) | RecordInt state header/回边/出口根及零次路径 | 已采纳；替代 0031 的标量 state 限制 |
| [0035](0035-bounded-exit-diagnostics.md) | ABI 故障分类与无业务所有权的单槽退出快照 | 已采纳（M0 宿主失败定位切片） |
| [0036](0036-bounded-inline-functions.md) | 有界非递归 process 函数、单次实参求值与内联根 | 已采纳（M0 源码函数切片） |
| [0037](0037-immutable-int-lists.md) | ABI v7、不可变 IntList 节点、类型边界及传递根 | 已采纳（M0/M1 业务值切片） |
| [0038](0038-self-send-effects.md) | ABI v8、静态自发送、按值 effect 交接与调用点准入 | 已采纳（消息副作用前置切片） |
| [0039](0039-local-process-capabilities.md) | ABI v9、局部 Pid 能力、同模块创建与跨进程发送 | 已采纳（跨进程副作用切片） |
| [0040](0040-language-request-events.md) | ABI v10、异步整数请求、完成事件预留与唯一终结 | 已采纳（语言交互切片） |
| [0041](0041-worker-deadline-parking.md) | 私有计费 OS 等待、就绪重查与请求 deadline | 已采纳（等待集成切片） |
| [0042](0042-managed-function-frames.md) | 私有受管函数、逐帧栈检查与聚合精确根上界 | 已采纳（调用栈切片） |
| [0043](0043-chunked-runtime-gc.md) | 固定块 GC、runtime 预算/取消及常量时间堆快照 | 已采纳（runtime 长操作切片） |
| [0044](0044-native-module-leases.md) | 代码租约、加载预算与 VM 归属校验 | 已采纳（模块资源切片） |
| [0045](0045-concurrent-resource-admission.md) | VM 级并发多维准入、净转移与跨线程归还 | 已采纳（资源并发切片） |
| [0046](0046-bounded-cross-thread-ingress.md) | 固定跨线程入口、计费消息与 OS 通知租约 | 已采纳（外部事件前置切片） |
| [0047](0047-cross-thread-ingress-requests.md) | 固定外部请求结果表、超时/取消与双端租约 | 已采纳（外部请求切片） |
| [0048](0048-bounded-loopback-service.md) | 有限 loopback TCP、绝对 I/O 截止时间与服务清理 | 已采纳（真实 I/O 验证切片） |
| [0049](0049-record-message-copies.md) | ABI v11、RecordInt 语言消息按值复制与目标入口根 | 已采纳（引用消息切片） |
| [0050](0050-account-node-admission.md) | 子账户控制节点预留、父路径计费与末持有者释放 | 已采纳（资源元数据切片） |
| [0051](0051-bounded-int-list-messages.md) | ABI v12、有界列表序列信封、目标重建与入口根 | 已采纳（列表消息切片） |
| [0052](0052-cross-thread-module-leases.md) | 线程安全只读代码租约、并行独立 worker 与末持有者清理 | 已采纳（多 worker 前置切片） |
| [0053](0053-detached-process-images.md) | 无活栈独占进程镜像、线程转交与同账户重绑定 | 已采纳（迁移前置切片） |
| [0054](0054-bounded-multi-worker-scheduler.md) | 固定多 worker、稳定路由、唯一执行权与 handler 边界迁移 | 已采纳；结果表/栈预留/扫描容量由 0060–0062 部分替代 |
| [0055](0055-multi-worker-exit-diagnostics.md) | 多 worker 最近失败的固定按值诊断，不延长业务资源租约 | 已采纳（有界诊断切片） |
| [0056](0056-multi-worker-supervision.md) | 独立终态票据与多 worker 有界 one-for-one 宿主重启 | 已采纳（宿主监督集成切片） |
| [0057](0057-accounted-immutable-bytes.md) | ABI v13、计费不可变字节叶节点与有界构造/复制 | 已采纳（业务值集成切片） |
| [0058](0058-byte-i64-big-endian-access.md) | ABI v14、固定八字节 signed big-endian 读取与不可变替换 | 已采纳（状态编码切片） |
| [0059](0059-byte-state-counter-equivalence.md) | 固定字节 Counter、有限请求窗口与 Wings/BEAM 正确性对照 | 已采纳（同协议正确性切片） |
| [0060](0060-shared-scheduler-reply-table.md) | 调度实例共享宿主结果容量、逐目标请求计费与空闲 actor 元数据回归 | 已采纳；部分替代 0054 的宿主结果表 |
| [0061](0061-on-demand-scheduler-stacks.md) | worker 按需 guarded stack、借入前三级准入与有界缓存 | 已采纳；部分替代 0054 的栈预留 |
| [0062](0062-sparse-ready-scheduler.md) | 固定就绪/活栈 FIFO、空槽链及 16384 actor 容量 | 已采纳；部分替代 0054 的扫描与容量 |
| [0063](0063-multi-actor-byte-counter.md) | 多 actor 完整 Bytes Counter 与无租约提交状态副本 | 已采纳（有界正确性切片） |

## 维护方式

- 文件按四位递增编号命名，如 `0007-decision-name.md`；编号不复用。
- 每份记录写明日期、状态、背景、决定、替代方案、影响和重审条件。未决定的方案用“提议”，不写成已采纳。
- 已采纳记录允许纠正文字和补充链接。实质改变决定时新增 ADR，在旧记录中注明被哪份 ADR 替代，并在新记录中回链；不要重写历史理由。
- 主题文档描述当前设计与具体契约，ADR 保存选择理由。改变决定时，同时更新 ADR 索引、相应主题文档、验收要求和协作约定。
- 优化候选先留在[激进优化](../optimizations.md)。改变默认 GC、ABI、执行模型、平台范围或包边界时，再用 ADR 记录选择。

本批记录于 2026-11-12 整理，记录的是当日已有设计和本次命名、包边界决定，不追溯虚构更早的决策日期。
