# ADR-0046：单 worker 的有界跨线程入口

- 日期：2026-11-13
- 状态：已采纳（外部事件前置切片）
- 后续：[ADR-0115](0115-typed-cross-thread-ingress.md) 增加类型化内容复制入口，原标量 API 保留，不开放原引用整数。
- 补充：[ADR-0020](0020-single-worker-mailbox.md) 的宿主发送入口
- 替代：[ADR-0041](0041-worker-deadline-parking.md) 的私有 notifier 限制
- 前置：[并发资源准入](0045-concurrent-resource-admission.md)
- 后续：[ADR-0047](0047-cross-thread-ingress-requests.md) 允许将同一固定入口配置为有界标量请求模式，不解除单 actor 单协议或原线程执行限制。

## 决定与理由

先让外部适配线程向原线程 worker 投递有界标量消息，不移动 actor、受管栈或含 Rc 的模块租约。当前不引入通用 reactor、网络协议、引用消息、跨线程请求或 M:N。

- worker 已配置 OS 等待后，可给每个 actor 注册一个 ingress，绑定该 actor 的完整 Pid、一个已校验的本模块单向入口和固定执行预算。容量 1..=64，参数只接受 Int/Bool/Unit、最多 64 个；请求、系统完成事件、引用及能力参数拒绝。注册失败不改变 actor 或账目，重复注册拒绝。
- worker 保存 HandlerId 和接收端；可克隆的 IngressSender 只持 Arc 共享的固定 ring、标量 schema、计费账户及 notifier，不包含模块或 actor 引用，不添加 unsafe Send/Sync。每个 ingress 的 FIFO 以共享锁下的发布顺序为准，不承诺不同 ingress、普通 send 或多个发送者的墙钟全序。
- 创建 ring 前向目标 process/tenant/VM 预留固定控制容量。每次发送先校验参数、预留一个 messages 单位及最终 Message 逻辑字节，再持 ring 锁检查 closed/full 并发布；拒绝归还预留。额度预留先于 ring 锁，不在锁内申请内存、持资源门闩、执行 native 通知或业务。准入与并发 close 竞争时可能出现短暂未发布预留，不保证生产者公平性。
- 发布后在锁外通知。返回成功即已接收，不代表已执行；IngressWake::Notified 与 WakeFailed 都是已接收，后者明确要求宿主继续 tick/有限 wait 或终止，不得按拒绝重试造成重复。只有发布前的错误是未接收。保留此区别，而不是通知错误后删除可能已被消费的消息。
- 每次 tick 在选择 actor 后至多搬运一条 ingress 消息到有空位的 mailbox。保留原票据，不重新准入或复制引用；mailbox 满时保留 ingress 队首，不丢已接收消息。搬运不执行 handler，后续仍由原有轮转/检查点驱动。wait 前后把 ingress 非空算作 Ready，通知先于 OS wait 也不能漏掉工作；无 ingress 时沿用旧契约。
- stop、受管失败及 worker Drop 关闭对应 ingress、清理尚未搬运的消息票据，已经发布的消息仍可像普通 mailbox 一样随目标退出丢弃，不承诺 exactly-once。close 与 send 在同一 ring 锁下线性化，close 后旧 sender 永远拒绝，进程槽复用不重定向旧 sender。停止不等待外部线程退出。
- ring 及 notifier 的逻辑预算保留到最后 sender/receiver 销毁；OS 等待预留改为 Arc 租约，worker 先销毁也不能留下未计费 notifier。已关闭入口可继续占用控制预算，直到宿主释放全部 sender；不把 actor 退出误报为全部字节已归零。最后 owner 先销毁 ring/FD，再归还对应预算。预算仍不是实际内核/RSS 计量。

## 对照与重审

不使用无限 mpsc、外部共享 HandlerId、unsafe Send worker 或每发送分配一个任务。单 actor 单入口限制避免动态路由表、模块跨线程寿命和路由代次协议；需要多协议入口时另行设计固定路由与计费，不静默解除限制。共享 Mutex ring 是正确性基线，性能与争用未测。

## 验证

有界测试验证 Send/Sync、固定容量、参数/额度/重复注册拒绝、FIFO、并发生产者、预先与等待中通知、mailbox 满保留、逐 tick 搬运、挂起/失败/stop 清理、旧 sender 与进程槽复用、worker 先 Drop 及最后 notifier 租约归还。通知失败用私有受控 fixture 检查已接收语义，不在宿主制造 FD 耗尽或非法访存。

执行 workspace check/test/clippy/fmt、MSRV 和 debug/release 受限 AOT 回归；两平台独立记录。该切片不是实际服务 I/O、M:N 或完整 MVP，不能替代同负载 BEAM 性能与内存门禁。

2026-11-13 在 macOS AArch64 通过上述 workspace 检查、Rust 1.85.0 all-targets 检查，以及 debug/release workspace 测试与受限 AOT 回归；Linux x86-64 尚未验证最新切片。没有服务尾延迟、争用吞吐或内存领先结论。
