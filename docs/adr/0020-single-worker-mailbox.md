# ADR-0020：有界单 worker 的宿主消息入口

- 日期：2026-11-12
- 状态：已采纳（标量调度切片，非完整 MVP）
- 相关决定：[owned task](0015-owned-scalar-tasks.md)、[资源账户](0014-resource-reservations.md)、[栈计费](0019-accounted-stack-pools.md)

## 背景与决定

先用当前标量 AOT 验证多实例、消息排队、唯一执行权和清理，不提前开放语言内阻塞调用或声称完整 M0 已完成。

- `ScalarWorker` 在同一线程拥有固定最多 256 个进程槽，所有 actor 使用同一计费栈池；最多同时有一个执行中的 resume，但多个 actor 可以各自保留挂起调用。按固定槽位轮转，每次 `tick` 至多开始/恢复一个 handler 的一个非零步数时间片，不忙扫无限队列。
- `Pid` 绑定 worker 身份、槽号和 generation；退出后复用槽位递增代次，耗尽退休。宿主所有寻址检查完整标识，旧 Pid 或其他 worker 的 Pid 不得投递给新实例。
- `spawn` 在创建账户、实例和 mailbox 前预留 process 数及所请求的控制结构容量；失败不占槽、不改变进程数。每 actor mailbox 固定 1..=64 条，ring 精确分配容量且不增长。每条消息最多 64 个标量参数，按模块绑定 HandlerId 校验 schema 后入队。
- `send` 只接受无回复入口，排队是调用点副作用，满时返回 `mailbox_full`，错误不改变队列或计费。当前宿主调用等价于一个发送者，按入队顺序 FIFO；没有跨发送者全序承诺。请求入口在后续固定 pending 表切片接入，不能用单向 send 静默丢弃回复。
- 每条已接受消息预留一个 messages 单位和控制容量，在该条 handler 完成/失败/退出清理后归还。启动前为参数副本、token 和 invocation 控制预留保守固定空间，并从计费池向 process 转移活栈容量。队列槽容量和消息逻辑占用分开计，不把固定 ring 或缓存字节漏掉。
- 开始入口的资源失败返回错误，不丢掉队首、不终止尚未执行的实例；恢复时算术/取消/总预算失败令该 actor 退出，清理活栈和整个 mailbox 后返回结构化退出事件。`stop` 不入普通 mailbox，满队列或挂起时也能正常取消返回后销毁，不跨 ABI unwind。
- 外部只能通过 worker 驱动 actor，不能取得可变实例；状态/候选提交复用 ScalarTask 原有契约。pid/queue/计费是宿主原型，不是语言 `spawn/send`，没有真实 I/O、计时线程或跨 worker 迁移。

## 验证与边界

受限 debug/release 子进程验证：多实例独立状态、FIFO、队列满拒绝、错误 schema/入口拒绝无变化、轮转无饿死、逐步挂起恢复、受管失败清理、满队列 stop、stale/跨 worker Pid、进程/消息/栈限额回滚和 Drop 归零。双平台分别运行本分支 CI；不以标量演示称已完成根链、动态栈检查、消息副作用或 BEAM 性能验收。

槽表和共享栈池由 tenant 预留；actor 控制/ring 和在途消息归其 process。标准 allocator 的内部开销、共享模块、资源账户节点与 OS 内核仍非完整物理账本，不能宣称资源隔离已全面完成。GC 记录堆尚未接入 scalar actor，后续编译器根与消息跨堆复制需独立 ABI 决策。
