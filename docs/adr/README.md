# 架构决策记录

[文档目录](../../docs.md) · [架构设计](../architecture.md) · [Crate 拆分](../crates.md)

ADR 记录为什么做出某项架构选择、考虑过哪些替代方案，以及什么情况下需要重审。当前都属于设计决策，采纳不表示实现已经完成或性能已经验证。

## 当前决策

| 编号 | 决策 | 状态 |
|---|---|---|
| [ADR-0141](0141-bounded-allocator-failure-injection.md) | 受限子进程真实 null 分配、回滚、重试及健康进展验证 | 已采纳（故障验证；非全局 OOM 隔离） |
| [ADR-0140](0140-bytes-tcp-service-equivalence.md) | 完整 Bytes 状态的受限 TCP 服务与显式 BEAM 正确性对照 | 已采纳（真实服务前置；非性能门禁） |
| [ADR-0139](0139-regular-file-manifest-loading.md) | 清单普通文件/长度检查、固定读取与拒绝退账 | 已采纳（可信静态产物输入边界） |
| [0138](0138-native-thread-stack-startup-boundary.md) | worker/timer 原生可用栈的原预留桶核对、启动结果与失败 join | 已采纳（宿主栈边界；非完整账本） |
| [0137](0137-infinite-handler-atomic-progress.md) | 无限步骤片下的原子让出、同 worker 健康进展、有限观测与停止清理 | 已采纳（短时进展前置；非公平性门禁） |
| [0136](0136-fixed-buffer-native-module-paths.md) | 计费加载的固定来源/原生输出缓冲与已知长度后构造 Rust 路径 | 已采纳（模块暂存账本切片） |
| [0135](0135-bounded-accounted-module-paths.md) | 计费加载的原始/解析路径字节边界、固定错误与暂存分配回归 | 已采纳（资源账本修正） |
| [0134](0134-opt-in-worker-cpu-observation.md) | 默认关闭的 worker 函数 CPU 区间、原子终态发布和独立诊断范围 | 已采纳（CPU 诊断；非性能门禁） |
| [0133](0133-open-loop-worker-lock-diagnostics.md) | 原开环长测显式启用 worker 锁统计、join 后独立核对与两对开关诊断 | 已采纳（默认关闭；非 CPU 归因/性能门禁） |
| [0132](0132-bounded-multi-counter-failure-evidence.md) | 多 actor 验证宿主的读时输出限额、绝对期限、用例/输入指纹和失败清理 | 已采纳（取证修正；原超时根因未定） |
| [0131](0131-equivalent-beam-cancellation-recovery.md) | BEAM 有限请求双端生命周期、取消不撤回与独立 child 监督恢复对照 | 已采纳（恢复正确性对照） |
| [0130](0130-readonly-cgroup-cpu-quota-observation.md) | 可选只读核对独占测量作业的 cgroup v2 CPU 配额、成员和变化 | 已采纳（本机模型通过；原生 Linux 未验证） |
| [0129](0129-native-module-dependency-boundary.md) | 计费产物固定系统依赖、加载重定向拒绝与装载前退账 | 已采纳（模块账本切片） |
| [0128](0128-native-module-image-admission.md) | 原生模块段表预检、映像跨度/文件页较大者准入与末 owner 退账 | 已采纳（模块账本切片） |
| [0127](0127-accounted-string-indexing-and-slicing.md) | String 按 Unicode scalar value 索引、切片及独立计费复制 | 已采纳（定向 AOT 通过；完整门禁未完成） |
| [0126](0126-bounded-resume-trace-retention.md) | 原恢复历史上限内显式丢样并完成业务，区分两类缺口 | 已采纳（有界诊断保留） |
| [0125](0125-streamed-resume-fairness-diagnostics.md) | 独立恢复区间流、业务时钟配对与精确子集重放 | 已采纳；满额行为部分由 0126 替代 |
| [0124](0124-bounded-scheduler-slice-observation.md) | 默认关闭的计费恢复区间 FIFO、结果计数和丢样守恒 | 已采纳（公平性诊断前置） |
| [0123](0123-status-only-resource-rejection.md) | 单一准入事务返回失败级别、记录回复拒绝跳过丢弃文本 | 已采纳（资源拒绝路径修正） |
| [0122](0122-bounded-handler-lookup-diagnostics.md) | 宿主 handler 名称沿清单规则预检、有界错误来源与零分配成功路径 | 已采纳（资源拒绝路径修正） |
| [0121](0121-persistent-cancellation-recovery.md) | 持续 Scheduler 的超时/取消、stop 竞争和一次重启强度验证 | 已采纳（持续恢复验证） |
| [0120](0120-persistent-concurrent-failure-soak.md) | 持续 VM 的双发送者失败生命周期、槽复用与独立重放 | 已采纳（长时故障验证切片） |
| [0119](0119-inline-small-invocation-arguments.md) | 调用参数分配诊断、小参数内联候选与原存储回退 | 实验后回退（仅保留诊断） |
| [0118](0118-native-sync-storage-admission.md) | 原生同步后备存储先准入与发布前初始化 | 已采纳（资源账本修正） |
| [0117](0117-streamed-cli-record-replies.md) | CLI 具名记录按字段流式输出 JSON、固定遍历控制与原预算 | 已采纳（命令行业务值交付） |
| [0116](0116-bounded-named-record-replies.md) | 具名记录回复的名义 schema 校验、有界叶复制与请求完成导入 | 已采纳（业务值切片） |
| [0115](0115-typed-cross-thread-ingress.md) | 固定跨线程入口的类型化复制及原双端请求生命周期 | 已采纳（外部事件业务值切片） |
| [0114](0114-scalar-process-account-binding.md) | 标量实例与脱栈镜像保留原进程账户身份 | 已采纳（资源隔离修正） |
| [0113](0113-bounded-named-record-message-expansion.md) | 具名记录消息有界字段展开、目标重建与原复制账本 | 已采纳（消息参数切片） |
| [0112](0112-bounded-tcp-startup-evidence.md) | TCP 完整握手绝对 deadline、失败前缀及进程组清理 | 已采纳（验证可靠性） |
| [0111](0111-bounded-string-content-equality.md) | String 完整内容相等、分块预算检查与精确根 | 已采纳（业务值能力） |
| [0110](0110-reproducible-bounded-workspace-validation.md) | 全目标测试受限执行、指纹清单与失败证据 | 已采纳（验证基础设施） |
| [0109](0109-accounted-wakeup-endpoint-leases.md) | OS 唤醒端点与末 notifier 的同寿命计费 | 已采纳（外部事件资源生命周期修正） |
| [0108](0108-bounded-named-record-values.md) | 有界具名不可变记录、名义布局与嵌套精确根 | 已采纳（本地/状态/函数业务值切片） |
| [0107](0107-accounted-control-token-leases.md) | 共享取消/让出标志末 owner 计费与常驻 worker 控制 | 已采纳（资源账本切片） |
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
| [0019](0019-accounted-stack-pools.md) | 租户预留栈池容量、活栈净转移到进程及缓存计费 | 已采纳；控制准入由 0080 扩展 |
| [0020](0020-single-worker-mailbox.md) | 有限标量 mailbox、generation Pid 与单线程轮转执行 | 已采纳（宿主调度切片） |
| [0021](0021-bounded-requests.md) | 固定 pending 表、generation 请求 ID、回复与超时终结 | 已采纳；提前回收部分由 0078 替代 |
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
| [0040](0040-language-request-events.md) | ABI v10、异步整数请求、完成事件预留与唯一终结 | 已采纳；提前回收部分由 0078 替代 |
| [0041](0041-worker-deadline-parking.md) | 私有计费 OS 等待、就绪重查与请求 deadline | 已采纳（等待集成切片） |
| [0042](0042-managed-function-frames.md) | 私有受管函数、逐帧栈检查与聚合精确根上界 | 已采纳（调用栈切片） |
| [0043](0043-chunked-runtime-gc.md) | 固定块 GC、runtime 预算/取消及常量时间堆快照 | 已采纳（runtime 长操作切片） |
| [0044](0044-native-module-leases.md) | 代码租约、加载预算与 VM 归属校验 | 已采纳（模块资源切片） |
| [0045](0045-concurrent-resource-admission.md) | VM 级并发多维准入、净转移与跨线程归还 | 已采纳（资源并发切片） |
| [0046](0046-bounded-cross-thread-ingress.md) | 固定跨线程入口、计费消息与 OS 通知租约 | 已采纳（外部事件前置切片） |
| [0047](0047-cross-thread-ingress-requests.md) | 固定外部请求结果表、超时/取消与双端租约 | 已采纳（外部请求切片） |
| [0048](0048-bounded-loopback-service.md) | 有限 loopback TCP、绝对 I/O 截止时间与服务清理 | 已采纳（真实 I/O 验证切片） |
| [0049](0049-record-message-copies.md) | ABI v11、RecordInt 语言消息按值复制与目标入口根 | 已采纳（引用消息切片） |
| [0050](0050-account-node-admission.md) | 子账户控制节点预留、父路径计费与末持有者释放 | 已采纳；bootstrap 部分由 0079 替代 |
| [0051](0051-bounded-int-list-messages.md) | ABI v12、有界列表序列信封、目标重建与入口根 | 已采纳（列表消息切片） |
| [0052](0052-cross-thread-module-leases.md) | 线程安全只读代码租约、并行独立 worker 与末持有者清理 | 已采纳（多 worker 前置切片） |
| [0053](0053-detached-process-images.md) | 无活栈独占进程镜像、线程转交与同账户重绑定 | 已采纳（迁移前置切片） |
| [0054](0054-bounded-multi-worker-scheduler.md) | 固定多 worker、稳定路由、唯一执行权与 handler 边界迁移 | 已采纳；由 0060–0062、0066、0078 部分替代 |
| [0055](0055-multi-worker-exit-diagnostics.md) | 多 worker 最近失败的固定按值诊断，不延长业务资源租约 | 已采纳（有界诊断切片） |
| [0056](0056-multi-worker-supervision.md) | 独立终态票据与多 worker 有界 one-for-one 宿主重启 | 已采纳；创建/查询/停止扫描由 0064 替代 |
| [0057](0057-accounted-immutable-bytes.md) | ABI v13、计费不可变字节叶节点与有界构造/复制 | 已采纳（业务值集成切片） |
| [0058](0058-byte-i64-big-endian-access.md) | ABI v14、固定八字节 signed big-endian 读取与不可变替换 | 已采纳（状态编码切片） |
| [0059](0059-byte-state-counter-equivalence.md) | 固定字节 Counter、有限请求窗口与 Wings/BEAM 正确性对照 | 已采纳（同协议正确性切片） |
| [0060](0060-shared-scheduler-reply-table.md) | 调度实例共享宿主结果容量、逐目标请求计费与空闲 actor 元数据回归 | 已采纳；部分替代 0054 的宿主结果表 |
| [0061](0061-on-demand-scheduler-stacks.md) | worker 按需 guarded stack、借入前三级准入与有界缓存 | 已采纳；部分替代 0054 的栈预留 |
| [0062](0062-sparse-ready-scheduler.md) | 固定就绪/活栈 FIFO、空槽链及 16384 actor 容量 | 已采纳；部分替代 0054 的扫描与容量 |
| [0063](0063-multi-actor-byte-counter.md) | 多 actor 完整 Bytes Counter 与无租约提交状态副本 | 已采纳（有界正确性切片） |
| [0064](0064-indexed-supervisor-slots.md) | 固定监督空槽链、稳定身份索引与复用拒绝 | 已采纳；部分替代 0056 的扫描 |
| [0065](0065-two-sender-counter-fifo.md) | 两个真实发送者的各自 FIFO 与合法提交交错 | 已采纳（有界正确性切片） |
| [0066](0066-bounded-scheduler-mailbox-capacity.md) | mailbox 256 槽上限与热点 100 槽准入/清理 | 已采纳；部分替代 0054 的容量 |
| [0067](0067-beam-bounded-admission.md) | BEAM 发送前有界准入、开始归还与终态关闭对照 | 已采纳（有界准入对照切片） |
| [0068](0068-bounded-open-loop-counter.md) | 计划时刻开环、逐条迟发/拒绝/超时及最终提交核对 | 已采纳（短时开环正确性切片） |
| [0069](0069-streamed-open-loop-measurement.md) | 独立长时开环、冻结输入、固定缓冲原始事件与离线精确分位 | 已采纳（测量入口；完整性能门禁未通过） |
| [0070](0070-bounded-fairness-heartbeats.md) | 同 worker 百万次受管加法、同刻突发心跳与每 actor 等待核对 | 已采纳（短时公平性业务前置） |
| [0071](0071-observed-atomic-yield.md) | 可选计费原子请求时间与真实 coroutine 返回后确认 | 已采纳（有界观测切片） |
| [0072](0072-scheduler-yield-observation.md) | Scheduler 每 worker 固定让出 FIFO、请求/合并/清除与丢样计数 | 已采纳（调度观测切片） |
| [0073](0073-reusable-initializer-stack-pool.md) | 宿主同步初始化显式借用同 tenant 计费栈池，不持有跨线程池 | 已采纳（批量启动资源复用） |
| [0074](0074-bounded-concurrent-recovery.md) | 并发超时/取消保留队列、stop 竞争、旧状态与全新监督重启 | 已采纳（既有恢复契约回归） |
| [0075](0075-platform-memory-observation.md) | 长测原生分口径内存、PID 身份、缺失/null 与观测成本 | 已采纳（平台内存采样入口） |
| [0076](0076-accounted-byte-messages.md) | Bytes 消息暂存计费、目标净转移、分块导入与入口根 | 已采纳（有界业务消息切片） |
| [0077](0077-streamed-fairness-measurement.md) | 连续 CPU/突发心跳长测、流式重放、每 actor 与让出时基 | 已采纳（长测入口；公平性门禁未通过） |
| [0078](0078-request-publisher-lifetimes.md) | 内部请求槽/票据保持到结果端与目标消息端都结束 | 已采纳（请求资源生命周期修正） |
| [0079](0079-accounted-vm-bootstrap.md) | VM 根/准入门闩启动计费、强制子节点准入及基线清理 | 已采纳（VM 控制资源切片） |
| [0080](0080-central-execution-admission.md) | 执行核心统一控制准入、显式标量池保留与准备失败回滚 | 已采纳（执行资源准入修正） |
| [0081](0081-accounted-default-cli.md) | CLI 默认标量与堆模式共用计费根、模块与单槽栈池 | 已采纳（CLI 资源集成切片） |
| [0082](0082-bounded-tcp-segments.md) | macOS 连接分段限制、持续缓冲检查与失败峰值诊断 | 已采纳（受限 TCP 资源边界修正） |
| [0083](0083-accounted-module-execution.md) | 计费模块初始化、执行与镜像接回禁止未计费池降级 | 已采纳（模块执行资源边界修正） |
| [0084](0084-accounted-root-module-admission.md) | 计费根拒绝未计费工具模块，保留独立工具执行与镜像 | 已采纳（模块加载与执行资源边界修正） |
| [0085](0085-module-path-admission.md) | 模块路径解析前准入原控制预算，再追加原库页预算 | 已采纳（模块加载资源顺序修正） |
| [0086](0086-accounted-cli-json.md) | CLI 请求缓冲与有界 JSON 解析准入、固定回复和重复字段拒绝 | 已采纳（CLI 请求适配层资源切片） |
| [0087](0087-accounted-beam-workload-roots.md) | 六个 BEAM 对照驱动的计费根、清理回执与新采集严格校验 | 已采纳（业务对照资源集成切片） |
| [0088](0088-owned-cli-stdio.md) | CLI 业务 stdin/stdout 的独立预留、固定缓冲、描述符与清理 | 已采纳（CLI I/O 资源切片） |
| [0089](0089-bounded-cli-startup-arguments.md) | CLI 原生参数借用、数量/字节边界及目录副本的三级准入 | 已采纳（CLI 启动资源切片） |
| [0090](0090-bounded-cli-reporting.md) | CLI 帮助/诊断的 VM 级预留、有界输出和断管处理 | 已采纳（CLI 控制输出切片） |
| [0091](0091-manifest-string-preflight.md) | 清单字符串在类型错误构造前按既有名称边界预检 | 已采纳（产物错误来源边界修正） |
| [0092](0092-concurrent-failure-counter.md) | 双发送者失败前缀、逐请求终态与独立 BEAM 对照 | 已采纳（并发失败正确性切片） |
| [0093](0093-scheduler-ready-before-park.md) | 等待锁内重查本 worker 的 ready，保留启动失败退避 | 已采纳（等待竞争修正） |
| [0094](0094-typed-host-message-values.md) | 宿主类型化 RecordInt/IntList/Bytes 复制消息 | 已采纳（直接宿主消息切片） |
| [0095](0095-typed-scalar-request-events.md) | 语言异步 Int/Bool/Unit 回复与精确类型完成入口 | 已采纳（标量完成事件切片） |
| [0096](0096-copied-record-replies.md) | RecordInt 回复的目标检查、内容复制和完成导入 | 已采纳（固定记录回复切片） |
| [0097](0097-accounted-int-list-replies.md) | IntList 出口根、分块复制和计费结果所有权 | 已采纳（计费列表回复切片） |
| [0098](0098-accounted-byte-replies.md) | Bytes 出口根、计费结果租约及分块完成导入 | 已采纳（有界字节回复切片） |
| [0099](0099-unready-result-poll-fast-path.md) | 计费每槽原子就绪标志、期限前未就绪轮询免锁 | 已采纳（局部轮询优化） |
| [0100](0100-coalesced-deadline-timer-notifications.md) | 按实际等待期限合并 arm/disarm 冗余计时通知 | 实验后回退（无稳定收益，保留失败证据） |
| [0101](0101-empty-language-request-maintenance.md) | 锁内存活计数与空语言请求表维护快路径 | 实验后回退（无稳定收益，保留失败证据） |
| [0102](0102-opt-in-worker-lock-statistics.md) | 默认关闭、固定计费的 worker 四类锁区间墙钟统计 | 已采纳（有界诊断切片） |
| [0103](0103-accounted-utf8-string-values.md) | 独立计费 UTF-8 String、本地/状态/函数和分块内建 | 已采纳（业务值切片） |
| [0104](0104-bounded-string-literals.md) | 有界 UTF-8 字面量、只读模块常量与分块计费副本 | 已采纳（字符串构造切片） |
| [0105](0105-accounted-string-messages.md) | String 参数独立类型复制、共享 Bytes 上限与三级计费 | 已采纳（消息参数切片） |
| [0106](0106-accounted-string-replies.md) | String 独立计费结果、出口根与匹配完成事件 | 已采纳（字符串交互切片） |

## 维护方式

- 文件按四位递增编号命名，如 `0007-decision-name.md`；编号不复用。
- 每份记录写明日期、状态、背景、决定、替代方案、影响和重审条件。未决定的方案用“提议”，不写成已采纳。
- 已采纳记录允许纠正文字和补充链接。实质改变决定时新增 ADR，在旧记录中注明被哪份 ADR 替代，并在新记录中回链；不要重写历史理由。
- 主题文档描述当前设计与具体契约，ADR 保存选择理由。改变决定时，同时更新 ADR 索引、相应主题文档、验收要求和协作约定。
- 优化候选先留在[激进优化](../optimizations.md)。改变默认 GC、ABI、执行模型、平台范围或包边界时，再用 ADR 记录选择。

本批记录于 2026-11-12 整理，记录的是当日已有设计和本次命名、包边界决定，不追溯虚构更早的决策日期。
