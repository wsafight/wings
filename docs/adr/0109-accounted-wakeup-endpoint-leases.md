# ADR-0109：OS 唤醒端点随最后通知者计费

- 日期：2026-11-17
- 状态：已采纳（外部事件资源生命周期修正）
- 延续：[ADR-0016](0016-worker-wakeup.md)、[ADR-0046](0046-bounded-cross-thread-ingress.md) 与 [ADR-0107](0107-accounted-control-token-leases.md)。
- 部分替代：ADR-0046 将等待票据与 notifier 分别保存在 worker/ingress 的做法。

## 问题与决定

等待端点原来是独立 `Arc<OwnedFd>`，原 4096 B 票据由 worker 和 ingress 另持有。普通 ingress 生命周期有票据，但 worker 内取得的原始 notifier 可以比 worker 存活更久；回归先保留 notifier 再销毁 worker，旧代码 tenant used 为零而通知仍能使用 FD。把生命周期约束写在所有者类型中，而不是要求每个使用者同时复制两份独立租约。

增加 `WorkerParker::new_accounted(account)`，先向账户及祖先预留原 4096 B，再创建 kqueue 或 eventfd/epoll、共享端点。失败正常关闭已创建 FD 并归还预留；额度拒绝不进入 OS 创建路径。旧 `new()` 保留不计费工具入口，不代表完整 VM 资源计费。

共享端点内部按顺序持有 `OwnedFd` 和可选 `Reservation`，通知者 clone 只共享这一份 Arc。最后 waiter/notifier/ingress 所有者先关闭端点再释放票据；Linux waiter 的 epoll FD 排在 notifier 前析构。waiter 退出但 notifier 存活时保守保留全部原逻辑额度，不把 kernel FD 成本、RSS 或 epoll 提前关闭当作可精确减账的数据。

`ScalarWorker::enable_wait` 使用新计费构造，不再另建 `Arc<Reservation>`；ingress 只复制 notifier，移除平行等待票据。原 4096 B 没有叠加收费，通知在发布后失败仍是已接收、不得盲目重试；等待/唤醒语义、超时、停止和末 sender 清理不变。ABI v20、4352 B 执行预留、消息/堆/栈/请求及所有时限不变。

## 验证与边界

实际命令、结果与失败证据见[验证记录](../validation-wakeup-leases-2026-11-17.md)。

先在旧代码复现 worker 销毁后 notifier 存活却退账，再验证修复。检查 process/tenant/VM 的零/少一/精确余量、OS 工厂调用顺序、注入创建失败回滚、clone 不重收、原账户句柄退出、跨线程最后释放和弱引用清理。固定结构上界仅描述逻辑控制存储，不声称计量内核对象或 allocator 内部元数据。

OS 示例增加计费根下反复创建、跨线程通知、waiter 提前退出和末通知者释放。真实 AOT 的 worker wait/ingress/request/TCP、停止、失败和控制租约回归继续使用原 CPU/墙钟/阶段限额，分别检查 debug/release 与 MSRV。最新原生 Linux、完整账本、公平/故障和同负载 BEAM 吞吐/整机内存双门禁仍待独立完成，不能据此宣布 MVP 达标。
