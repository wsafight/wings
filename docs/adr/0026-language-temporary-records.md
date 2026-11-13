# ADR-0026：语言临时记录与显式计费执行

- 日期：2026-11-13
- 状态：已采纳（M0/M1 有界语言切片）
- 后续：[ADR-0049](0049-record-message-copies.md) 以复制载荷和目标入口根开放 RecordInt 消息参数，引用回复仍不支持。
- 补充：[ADR-0025](0025-aot-record-roots.md)、[ADR-0020](0020-single-worker-mailbox.md)
- 后续：[ADR-0027](0027-persistent-record-state.md) 开放持久 RecordInt 状态，替代局部限制并升至 ABI v6；消息与回复限制保留。

## 决定

先把已验证的临时记录根链接到业务源码，不同时开放引用状态或消息。

- 新增局部类型 `RecordInt` 与三个受限内建调用：`record_int(Int) -> RecordInt`、`read_int(RecordInt) -> Int`、`collect() -> Unit`。仅允许这三个名称的调用，不增加任意函数或阻塞调用。名称可作为普通绑定；被局部/参数/状态绑定遮蔽时不能再作为内建调用，避免解析与查询分叉。
- 语法支持带逗号的调用参数、嵌套和括号内换行，继续受每表达式 256 节点与源码大小限制。内建参数从左到右求值。记录不可变；局部可以复用引用但不能赋值。state、消息字段和回复仍只允许 Int/Bool/Unit；`RecordInt` 在这些位置静态拒绝。
- 降低为已有 `RecordInt`、`ReadInt`、`Collect` IR，不改变 ABI v5、输出协议或根计划。生成代码仍只发布当前 safepoint 的活跃 Ref；不保留所有历史对象。分配失败收集一次后重试，仍满时终止 handler/实例，不提交状态或成功回复。
- `instantiate_with_heap` 要求计费栈池属于指定 process 账户，实例记住该池，同步调用不能退回未计费默认池。显式 start/task 若传入其他 process 或未计费池，必须在执行前拒绝。堆与栈遵循同一 process/tenant/VM 账户；堆控制块、根表和槽容量先准入，失败回滚。无堆的既有实例行为不变。
- worker 增加显式 `spawn_with_heap`，在初始化前建立进程计费堆；普通 spawn 不带堆。队列仍只复制标量参数，不能携带本堆 handle。堆容量不随调用自动增长，退出与受控重启清理旧堆；监督默认接口仍不自动选择堆配置。
- CLI 增加可选 `--heap-records 1..=1024`，与 `--max-steps` 可任意先后但不得重复。指定后使用固定 16 MiB 的 VM/tenant/process 字节上限及单槽计费栈池；没有该参数不配置堆，调用内建明确失败。此固定额度是原型上限而非完整服务配置，CLI 仍是可信本机 JSONL 驱动，不是网络 VM。

## 验证与边界

前端/LSP 共用类型分析，回归合法嵌套、参数顺序、类型错误、错误数量、遮蔽、畸形/不完整调用、节点/源码上限、引用边界拒绝及局部 hover/跳转。源码到 AOT 的受限 OS 子进程验证 GC 保活、单槽复用、initializer、预算/算术/堆满失败不提交、CLI 参数拒绝、worker 多实例/请求/轮转/stop 与账户归零。分别执行 debug/release、macOS AArch64/Linux x86-64 及 MSRV。

不开放通用记录 schema、字符串、引用状态/消息、循环、语言 send/request/spawn、真实 I/O 或 M:N，不因此宣布 M0/MVP 完成。语言持久业务值与完整消息语义仍是后续必要工作。新增类型/调用语法是语言契约扩展，后续业务记录设计须保留迁移路径，不把受限内建当成最终通用对象模型。
