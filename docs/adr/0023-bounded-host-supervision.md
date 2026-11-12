# ADR-0023：宿主 one-for-one 受限重启

- 日期：2026-11-12
- 状态：已采纳（单 worker 恢复切片）
- 补充：[宿主 mailbox](0020-single-worker-mailbox.md)、[异步请求](0021-bounded-requests.md)

## 决定

- `ScalarSupervisor` 独占一个 `ScalarWorker`，固定最多 256 个子项；`ChildId` 绑定首次创建的完整 Pid，作为跨重启的宿主逻辑句柄。每次重启产生新 generation Pid；旧 Pid 不能投递到新实例，不开放可变 worker 以绕过监督所有权。
- 策略仅有 `Never` 和 `OnFailure { max_restarts, within }`。后者要求 1..=64 次、非零且单调时钟可表示的窗口；首次 spawn 不计重启。保留窗口内成功重启的时刻，恰好达到窗口边界的旧记录过期。重启次数在 `within` 内最多为指定值，不随成功处理消息清零。
- 一次 tick 最多执行原 worker 的一个时间片。handler 受管失败先按既有契约清理活栈/队列、终结旧 pending，再从同一模块 initializer 创建全新状态。不会保留失败实例状态、重放队列、重试请求或撤销已发生的副作用。其他子项不重启。
- 失败事件同时携带原 ChildId、退出 Pid、原因和 `RestartStatus`：新 Pid、策略禁用、强度耗尽或重建准入失败。达到上限或重建失败立即移除监督项，不在后续 tick 无限重试；原始退出事件不被重启错误覆盖。此时 ChildId 失效，宿主可明确决定是否重新创建。
- 主动 `stop` 不触发重启；绕过普通 mailbox 正常取消挂起调用后移除监督项。worker 和 supervisor Drop 不重启，正常取消并归还容量。旧请求的 Failed/timeout/cancel 和尚未取走的成功结果仍按原 ID 保留到消费或 Drop，不让新实例冒领。
- 子项槽表与按策略精确分配的重启历史在分配前归 tenant/VM 计费，历史只保存配置上限的槽，不增长。子进程、消息、请求、执行栈继续走原 worker 的三级准入。模块、allocator、OS 线程等开销边界不变，不声称完整物理账本已经完成。

## 验证与边界

Rust 回归核对策略边界、精确窗口过期、强度不被成功处理重置、状态重新初始化、Pid/ChildId 生命周期、其他子项隔离、旧请求终结、满 mailbox/挂起 stop、重启准入失败、历史与槽表计费以及 Drop 守恒。真实 AOT 产物在受限 debug/release 子进程验证失败、重建、恢复业务和达到上限，两个首要平台分别运行。

这仅是宿主 one-for-one 试用入口，没有语言级 supervisor、link/monitor、控制消息广播、backoff、持久化状态、rest-for-one 或 one-for-all，不宣称 OTP 兼容。该恢复切片不跳过 M0 compiler 根、动态栈、语言业务堆和同负载 BEAM 性能/内存验收。
