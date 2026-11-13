# ADR-0055：多 worker 最近失败诊断

- 日期：2026-11-13
- 状态：已采纳（有界诊断切片；macOS 功能已验证，Linux 与完整门禁另记）
- 扩展：[ADR-0035](0035-bounded-exit-diagnostics.md)、[ADR-0054](0054-bounded-multi-worker-scheduler.md)

## 决定与理由

Scheduler 的 actor 受管失败后立即退休旧 Pid，仅观察不到 actor 不能定位故障。沿用单 worker 的固定单槽原则，在共享 registry 中保存最近一次非主动停止的 handler 失败，不引入无界事件队列或保留已退出业务资源。

- 新增按值、可跨线程读取的 SchedulerExitSnapshot，包括旧 Pid、执行 worker 编号、固定 256 字节 handler 名、state 类型、旧标量提交、执行快照、排队数和清理前账目。引用 state 不暴露 handle，不保存模块、参数、账户、堆或活栈所有权。
- SchedulerHandle::last_exit 在 registry 锁下复制该值，不驱动程序、GC 或等待 owner；关闭后仍可读取。每 scheduler 只有一个最近失败槽，跨 worker 写入在 registry 锁下串行；后失败覆盖前失败，不承诺故障历史、物理发生时间全序或每次失败必达。
- 原生 ABI 故障从 ExecutionSnapshot.fault 分类为 Managed；timer/执行控制错误若无 ABI fault，归为 ExecutionControl，不解析错误文本。handler 正常完成、主动 stop、全局关闭、start 前准入失败和宿主参数错误不覆盖最近失败。
- 记录发生在正常退出/取消返回、状态提交判定完成之后，actor 及 mailbox 清理之前。旧标量状态仍是最后一次正常提交，不是失败候选；resources_before_cleanup 不是泄漏统计。缓存本身随 Shared 的 size_of 在构造前计费。

## 边界与验证

该槽不能作为可靠监督事件：快速失败会覆盖，完整恢复接入仍需有界且不丢终态的独立生命周期协议。未知 native 内存错误、panic/unwind 跨 ABI、OS kill 不伪装成受管失败。

内部测试验证不同 worker 的失败覆盖、成功/stop 保留、旧 Pid 失效、旧提交、预算及算术分类、关闭后读取和结果不延长业务资源租约。真实受限 AOT 检查失败循环不提交候选、原 state/预算及退出后其他 actor 可继续；不放宽子进程限制。运行 workspace/debug/release/MSRV 全检查，Linux 结果另记，不宣称 M4/MVP 完成。
