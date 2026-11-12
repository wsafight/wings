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
| [0011](0011-scalar-stackful-resume.md) | 标量 ABI v3、同线程原栈让出恢复及无 unwind 取消清理 | 已采纳（M0 原型设计）；替代 0010 的 ABI 与阶段限制 |
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

## 维护方式

- 文件按四位递增编号命名，如 `0007-decision-name.md`；编号不复用。
- 每份记录写明日期、状态、背景、决定、替代方案、影响和重审条件。未决定的方案用“提议”，不写成已采纳。
- 已采纳记录允许纠正文字和补充链接。实质改变决定时新增 ADR，在旧记录中注明被哪份 ADR 替代，并在新记录中回链；不要重写历史理由。
- 主题文档描述当前设计与具体契约，ADR 保存选择理由。改变决定时，同时更新 ADR 索引、相应主题文档、验收要求和协作约定。
- 优化候选先留在[激进优化](../optimizations.md)。改变默认 GC、ABI、执行模型、平台范围或包边界时，再用 ADR 记录选择。

本批记录于 2026-11-12 整理，记录的是当日已有设计和本次命名、包边界决定，不追溯虚构更早的决策日期。
