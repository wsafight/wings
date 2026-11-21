# ADR-0056：多 worker 有界终态交接与宿主监督

- 日期：2026-11-13
- 状态：已采纳（宿主监督集成切片；macOS 功能已验证，Linux 与完整门禁另记）
- 扩展：[ADR-0023](0023-bounded-host-supervision.md)、[ADR-0054](0054-bounded-multi-worker-scheduler.md)、[ADR-0055](0055-multi-worker-exit-diagnostics.md)
- 后续：[ADR-0064](0064-indexed-supervisor-slots.md) 部分替代创建/查询/停止扫描；终态与重启语义不变。

## 决定与理由

最近失败缓存可被其他 worker 覆盖，不能拿它决定哪个子进程需要重启。为每个宿主监督子进程在发布 Pid 前安装独立、固定、一次性的终态票据；在旧 actor 的镜像、mailbox、结果表引用与自身资源票据释放之后才发布终态。外部 IngressRequest 仍可合法延长原结果账户租约，不将其计为清理失败。

- SchedulerSupervisor 拥有 Scheduler 与固定子进程表；每个 child 使用稳定的初始 Pid 身份，current_pid 返回当前实例 Pid。普通宿主 handle 仍可发送和查询，语言创建的后代不自动纳入该监督表。
- 终态分为 Failed（按值结构化失败诊断）、Stopped 和 Closed。每个 actor 独占发布端，终态不可覆盖；票据不持有 actor、模块、heap、活栈或 process 账户。票据/历史表在 tenant 路径预留后构造，发布端是 Actor 最后释放的字段，不能在旧业务资源尚持有时触发重启。
- poll 只扫描固定 child 表，一次消费至多一个终态。不使用后台监督线程或无界退出队列；未 poll 时退出不丢失，也不自动重启。失败重启在调用 poll 的宿主线程重新初始化独立实例，不在 registry 锁中执行，不恢复失败 state/堆，不重放旧 mailbox，不转发旧 Pid。
- 复用已有 RestartPolicy 和有限滚动窗口历史；仅成功重启记录一次。关闭/主动 stop 不重启；失败策略禁用、强度超限或初始化/额度拒绝都终结该 child 并返回明确状态。不因失败创建而无限自动重试。
- 失败请求先按既有规则选择终态，清理后监督票据才可消费。重启不覆写最近失败缓存；共享 registry 中其他 actor 可继续执行，父 handler 已发布的副作用不回滚。
- supervisor.stop 请求当前实例停止并移除监督配置，不等待活栈同步结束。通过原始 handle.stop 的停止在 poll 中报告 Stopped。Drop 先关闭/取消/join scheduler，再释放监督表；不在已关闭 scheduler 上重启。

## 边界与验证

这是宿主 one-for-one 恢复，不包含语言 supervision/link/monitor、持久状态恢复、跨 tenant 移动或 OTP 兼容。固定表扫描没有性能结论；poll 必须由服务宿主驱动。

验证快速并发失败不丢各 child 终态、全新 state/heap 与新 Pid、旧请求终结、滚动强度、Never、主动停止、重启准入/initializer 失败回滚、其他 actor 继续推进和 Drop/最后结果释放后归零。真实受限 AOT 覆盖多 worker 失败循环、重建与强度上限。执行 workspace/debug/release/MSRV 和平台回归，不放宽 CPU/墙钟/故障隔离要求，不宣布完整 M4/MVP 达标。
