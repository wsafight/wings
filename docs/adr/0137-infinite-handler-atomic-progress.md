# ADR-0137：无限 handler 的原子让出与同 worker 进展回归

- 日期：2026-11-18
- 状态：已采纳（公平性前置验证；不是长测门禁）
- 延续：[ADR-0072](0072-scheduler-yield-observation.md)、[ADR-0124](0124-bounded-scheduler-slice-observation.md)

## 缺口与决定

原 scheduler_yield 用两个忙 actor 验证计时请求、身份、FIFO 丢样与取消，但没有同 worker 的健康 actor；scheduler_slices 用一步片验证进展，不能独立证明进展来自原子计时让出。新增独立真实 AOT 回归，两者不替代或改动原百万加法/心跳协议。

复用 SchedulerSlices 的 Hold（先把候选 count 写成 99，然后永久空循环）与 Work/Read。显式设置一步片为 `u64::MAX`，总预算为 `u64::MAX`，量子仍 1ms；每 worker 放一个 Hold，另有八个健康 actor。先等待每 worker 确认至少一次真实原子让出，再顺序执行十六轮、每健康 actor 一次 Work，核对精确结果和最后提交状态。单 worker 保证忙/健康共享唯一 worker；双 worker 每个都持有自己的无限活栈，健康 handler 正常返回后仍沿原迁移规则。

每 worker 仍是固定 256 条计费让出 FIFO，不增加现有容量上限。结果包含逐条心跳发送/观察时间、完整 Pid 匹配后的 actor 序号、请求/实际确认时间及停止后全部计数；满额显式计 dropped，不伪造完整历史。检查忙 actor 仍 Suspended、旧提交为 0、未迁移、未耗尽预算；停止必须正常取消清理。Scheduler 保持开放直到全部旧 actor 退场，再复用槽创建新实例，核对旧身份拒绝和新提交 0，避免将关闭后的统一拒绝误当身份验证；最终回到计费根基线。

## 验证与边界

独立验证器检查时钟、请求顺序、actor/worker 归属、有限数量、守恒和资源回执，拒绝伪造历史；与原 Debug/Release 完整 AOT 一起运行。保留原 CPU 20s/墙钟 30s/core 0 及每配置总 3 秒限制，不因发生器、装载或计时扰动放宽。

这不是原代表负载的性能测量，不是单核 OS 硬配额，也不是完整公平性分布或 p99 <5ms 门禁。没有长时、过载分组、监测开销、原生双平台或 BEAM 吞吐/整机内存通过结论；实现/ABI v22、4352 B、默认调度和语言语义均不变。若在原期限失败，保留原始输出并定位，不重试擦掉证据。
