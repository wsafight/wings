# ADR-0041：单 worker 空闲等待与请求 deadline

- 日期：2026-11-13
- 状态：已采纳（OS 等待集成切片）
- 补充：[ADR-0016](0016-worker-wakeup.md)、[ADR-0040](0040-language-request-events.md)

## 决定与理由

将已有 kqueue/epoll 等待接到单 worker 的就绪检查与请求 deadline，不以 sleep 轮询代替 OS 等待，也不提前开放跨线程消息入口。

- `enable_wait()` 显式创建 worker 私有 WorkerParker，重复配置拒绝；先向 tenant/VM 预留 4096 字节的固定控制预算，再创建描述符，失败归还预留。结构本体已包含在 worker 的 size_of 计费；4096 是保守逻辑控制预算，不是 kernel 内存/RSS 精确值。
- `wait(max_wait) -> WorkerWait` 只在原 worker 线程调用，必须先 enable。它验证单调时钟上限，扫描到期请求并尝试将完成事件入队，但不执行任何 handler。已有挂起入口或 mailbox 时返回 Ready，绝不为就绪工作睡眠。
- 无工作时在私有 OS 等待端等待 min(max_wait, 最早未终结请求 deadline 的剩余时间)。返回后再次扫描 deadline 和完成事件；有就绪工作返回 Ready，否则新到期请求返回 Deadline，其余返回 TimedOut。Deadline 是提示调用者查询宿主结果，不是请求计数；已经终结未取走的宿主结果不反复触发立即返回。
- 零上限只轮询，过大上限执行前拒绝；中断/提前超时的剩余等待沿用底层单调时钟契约。正常等待不消耗入口步数、创建计时线程或执行忙循环；系统调度可能延迟返回，不承诺硬实时。
- 私有 parker 不导出 notifier，因此不存在 worker 已清理而外部 notifier 继续持有未计费 FD 的路径。wait 期间外部不能直接修改同线程 worker；调用方使用有限 max_wait 周期处理自身控制事件。跨线程有界 inbox、通知与最后一个 notifier 的计费寿命须另行设计，不能把当前等待称为完整 reactor/I/O 或 M:N。
- 请求 deadline 已到但完成 mailbox 满时，pending 继续保存结果；队列或活栈使 Ready，无丢事件或阻塞完成投递。发起方退出/取消/Drop 沿用请求契约。等待失败不清空队列或回滚已发生副作用。

## 验证与重审

受限子进程检查未配置/重复/额度拒绝零变化、空闲零与有限等待、预先就绪和挂起入口立即 Ready、宿主请求到期而不执行目标、语言完成事件到期及满队列、终结结果不重复唤醒、超大时间拒绝和 Drop 归零。两平台独立验证 debug/release、MSRV；本机结果不代替 Linux 后端或尾延迟实验。

当前已接收请求的目标通常总有排队消息或活栈，因此先返回 Ready。无就绪工作但存在 pending 的最早 deadline 等待由内部 fixture 单独覆盖，不将该 fixture 写成已支持外部异步服务。

增加外部事件源、通知转移或多消费者时必须重审发布/等待竞争、FD 注册/注销及计费租约。该切片不改变 ABI，也不代替受管动态栈、完整 M0、真实 I/O 或 MVP 性能门禁。
