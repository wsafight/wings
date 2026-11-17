# ADR-0047：有界跨线程请求与结果租约

- 日期：2026-11-13
- 状态：已采纳（外部请求切片）
- 后续：[ADR-0115](0115-typed-cross-thread-ingress.md) 扩展固定入口的类型化参数，不改变超时不撤回及双端租约。
- 扩展：[ADR-0046](0046-bounded-cross-thread-ingress.md) 的单向入口限制
- 相关：[ADR-0021](0021-bounded-requests.md)、[ADR-0045](0045-concurrent-resource-admission.md)
- 后续：[ADR-0099](0099-unready-result-poll-fast-path.md) 为未就绪且未到期限的宿主轮询增加计费原子快路径；终态仍由原锁保护。

## 决定与理由

外部 I/O 适配器必须取得对应请求的结果，不能读取 actor state 猜测回复，也不能共享可变 actor 或用无界通道保存完成消息。保留每 actor 一个固定协议入口，可选择单向或请求模式，不改语言或 ABI。

- `open_request_ingress` 绑定本模块的 Int/Bool/Unit 回复入口，参数仍限 Int/Bool/Unit。返回同一 IngressSender；`send` 只允许单向模式，`request(arguments, timeout)` 只允许请求模式，返回唯一 IngressRequest 和 IngressWake。两种 wake 均表示已接收，发布前错误才可按未接收处理。系统完成入口、引用和能力仍拒绝。
- 请求入口在创建前按 process/tenant/VM 预留固定 ring 与固定结果表的控制容量，各 1..=64 槽。每次请求预留一个 requests 单位及消息单位/字节，才尝试占用结果槽并发布；无槽、队满或关闭回滚全部预留。结果表与入口分别持有预算，不为每次请求分配 Arc、通道、线程或动态任务。
- 消息携带结果表租约及槽号/代次，调用者持唯一结果 handle，不依赖 worker 的宿主 pending 表或语言 RequestId。结果表不持 ring、模块、actor 或 native 回调，不形成引用环。代次耗尽退休，不允许迟到回复命中新请求。
- `try_take` / `wait(max_wait)` 返回 Reply/TimedOut/Cancelled/Failed；取走后 handle 不可复用。wait 只阻塞外部调用线程，以 Condvar 和单调时钟等待 min(max_wait, 请求 deadline)，worker 绝不能在 handler 中调用它。零时限轮询，超大时限在操作前拒绝；虚假唤醒重查条件。单向 actor 语义不变，不增加语言 await/call。
- 回复仅在正常返回、状态提交后发布；deadline 已到则保存 TimedOut，不覆盖先前终结的结果。cancel、超时或丢弃 handle 不撤回排队/已经执行的消息，不自动重试。失败、stop、worker Drop 对剩余消息发布 Failed，但不覆盖已终结结果。
- 槽位及 requests 预留保持到调用者已取走/丢弃结果且消息发布者已完成/清理两者都成立。超时后目标仍排队时不提前释放容量，不用反复超时/重试绕过上限。结果表最后 handle 销毁前保持控制预算；sender/worker 销毁不要求外部先取结果。消息和结果分别计费并清理，进程退出可仍有外部结果控制预算。
- 资源门闩与 ring/结果锁不嵌套；锁内不分配、格式化错误、执行业务或归还 Reservation。结果表操作有固定槽数上限，完成通知在解锁后执行。ring 与结果表锁不嵌套。共享锁/Condvar 是正确性基线，争用与尾延迟未测。

## 选择与边界

采用预付费固定结果槽，而非每个请求一个堆分配的 channel 或无限完成队列。保留原单向入口和宿主/语言请求机制；三者共享提交/副作用边界，但不混用其身份。取消不是中断目标 handler，不能把网络断开写成副作用撤回。一个外部调用者可跨线程移动唯一 handle，但 actor、代码租约与活栈仍不跨线程。

## 验证

检查协议/参数/容量/三级额度拒绝零变化；固定结果容量、未取结果背压、超时/取消后消息继续执行、恰好一次终结、迟到回复/槽复用、丢弃 handle、完成前后 Drop 顺序、worker/目标退出、外部等待与通知竞争、Bool/Unit 回复以及最后租约归零。可信 fixture 只模拟受管入口，生成代码另由受限 debug/release AOT 子进程验证。

执行 workspace check/test/clippy/fmt、MSRV、debug/release workspace 与 AOT 回归；首要两平台独立记录。真实网络适配器、完整业务值、M:N 及同业务 BEAM 吞吐/内存门禁仍未完成，不据此称 MVP 达标。

2026-11-13 在 macOS AArch64 通过上述 workspace 检查、Rust 1.85.0 all-targets 检查及 debug/release workspace 测试和受限 AOT 回归；Linux x86-64 尚未验证此切片。无性能或内存领先结论。
