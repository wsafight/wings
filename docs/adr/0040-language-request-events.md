# ADR-0040：语言异步整数请求与有界完成事件

- 日期：2026-11-13
- 状态：已采纳（单 worker 语言交互切片）
- 后续：[ADR-0095](0095-typed-scalar-request-events.md) 扩展为类型匹配的 Int/Bool/Unit 回复与完成第三参数，原请求生命周期保留。
- 补充：[ADR-0021](0021-bounded-requests.md)、[ADR-0039](0039-local-process-capabilities.md)
- 部分替代：[ADR-0078](0078-request-publisher-lifetimes.md) 修正完成事件入队或发起方退出后的提前回收；事件预留仍可转移/释放，requests 票据保持到目标消息结束。下文保留原决定的背景。

## 决定与理由

先以显式静态完成入口连接已有请求表，不引入阻塞等待、可变闭包或隐式续体。首个切片仅接受 Int 回复，后续一般类型与关联状态单独扩展。

- `request(pid, Message, Completed, timeout_ms, args...) -> RequestId` 只在 handler 内调用：Message 是同模块 `-> Int` 入口，Completed 是本模块单向 `on Completed(id: RequestId, status: Int, value: Int)`。三个事件参数以普通具名参数形式声明，但该签名被识别为系统事件，不能经普通 send/request、CLI 或独立实例伪造调用。请求业务参数仍只 Int/Bool/Unit。
- 成功调用在原调用点完成请求槽、发起方 requests 额度、完成事件消息及字节额度预留，然后向目标 mailbox 准入入队；失败完整回滚，返回 RequestRejected。生成代码下一片收到唯一确认并继续，返回的是不复用的请求能力号，不是业务结果。RequestId 仅允许局部/fn 值和完成事件首参数，禁止 state、普通消息、回复、整数转换或比较。
- 完成事件 status 固定为 0=Reply、1=TimedOut、2=Cancelled、3=Failed；仅 Reply 的 value 为结果，其余为 0。成功状态先提交，再选择结果；每个请求只选择一次。timeout_ms 限 0..=86,400,000 的整数，使用单调时钟，零值允许立即超时，不撤回已经准入的目标消息。
- `cancel_request(id) -> Unit` 只允许发起方当前仍在 pending 中的语言请求，不可取消其他实例、宿主请求、stale 或已转为 mailbox 事件的请求；拒绝为 RequestRejected。取消只终结一次，迟到回复/退出不覆盖。已终结但尚未入完成队列的请求再次取消保留原结果。
- 有限 mailbox 满时不丢完成事件：pending 保留终结结果和预留票据，下一次 tick 有空位后将预留直接移入事件消息，不再次收取额度。只把实际入队的事件排到既有队尾；不承诺终结时刻与其他发送者的全序。固定表饱和显式拒绝新请求，宿主查询不推进业务。
- 发起方退出时丢弃其未投递完成事件并归还请求/事件预留，但不撤回对端已接受消息；对端退出使仍有效请求 Failed。发起方被复用槽位或重启不会收到旧事件。完成事件正常处理或退出时释放消息计费。没有持久投递、事务或 exactly-once 业务保证。
- 回调使用发起方原消息 max_steps 作为独立预算，新的请求消息亦如此；不补当前预算。请求表、事件、栈仍经过三级准入。pending 的接收者、完成 schema、能力号和预算都是固定容量元数据，不建增长表。
- ABI v10 保留 context 88 字节，EffectRequest 尾增加 callback 序号和 timeout_ms，共 48 字节；按值交出与确认仍不保留 worker 内的 context/根/结果指针。Request/Cancel 是新增 effect 操作，旧 v9 产物须重编译。

## 验证与重审

静态和 IR 检查目标、回调签名、kind、参数及不可伪造 ID；检查器/LSP 共用类型。真实 AOT 验证调用点入队、返回后异步事件、成功提交顺序、零/挂起超时、取消和迟到回复、目标失败/stop、发起方失败/复用、完成队满保留、表满及三级额度拒绝零变化；effect 边界强制 GC、Drop 清理。debug/release、MSRV 与双平台独立执行。

没有 tick 时不会主动投递超时事件；OS reactor/自动唤醒另行接入。没有阻塞 receive/await/call，没有 Pid/RequestId 持久 state 或通用回复类型；该切片不代表完整请求协议、M0/M1 或 MVP。
