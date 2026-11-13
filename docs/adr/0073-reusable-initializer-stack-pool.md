# ADR-0073：显式复用宿主初始化栈池

- 日期：2026-11-14
- 状态：已采纳（批量启动的显式资源复用，不是性能门禁）
- 背景：[ADR-0072](0072-scheduler-yield-observation.md) 的 Linux 大容量回归仍触发原 3 秒 deadline

## 决定与边界

Scheduler 宿主每次 spawn 默认创建独立一槽初始化栈池，执行同步 initializer 后销毁；大量 actor 顺序创建会重复 OS mapping 和池元数据。语言 spawn 已借用当前 worker pool，证明初始化可用显式绑定，不必为每个同步 initializer 创建 mapping。增加宿主 `SchedulerHandle::spawn_with_heap_in_pool` 和 `SchedulerSupervisor::spawn_with_heap_in_pool`，调用者提供同 tenant 的计费 StackPool；已有方法与其生命周期保持不变。

显式池仅在调用线程运行 initializer，容量/缓存上限固定，仍逐 actor `for_process`，借入前将完整栈额度从池转到目标 process、返回后转回 tenant。保留原模块 VM 校验、进程/heap/调用帧准入、总预算、栈限额和 initializer 不得挂起规则。错误 tenant/未计费/已占满池、quota 或 initializer 失败按原路径回滚 actor slot/预留，不执行半初始化 actor。复用缓存不是共享业务状态，未初始化栈不能被业务读取；缓存不等于 RSS 消失。

函数只借用池，不保存到 Scheduler、handle 或 supervisor，不把 `Rc`/活栈变成 Send/Sync。initializer 正常返回并 detach 后才发布 actor。后续 handler 使用对应 worker 池，监督自动重启仍用原临时初始化池；未引入后台初始化队列、每线程 TLS cache 或跨线程池迁移。调用者要在批量创建后释放缓存可 drop/trim 自己的池，最后释放仍完整归零。

v2 Counter 只将同步初始化改为调用者显式一槽、最多缓存一槽的计费池，保留原输入、10000×256 B/1000×4 KiB/100×64 KiB、100 活动、32 窗口、原 CPU 5 秒/墙钟 10 秒/阶段 3 秒及完整 AOT 20/30 秒。池创建仍计入原阶段 deadline，不提高配额，不把初始化排除计时，不重试失败业务。最终账本包含显式池，停止全部 actor 后 drop 池才核对零。

## 验证

回归覆盖同池顺序初始化得到独立 state、process/tenant/VM 资源转移和失败回滚、外来 tenant/未计费池拒绝、池被占满时拒绝及恢复、初始化失败/监督重启和最后归零，确认 Scheduler/handle/supervisor 原线程可用性不变。完整 v2 和 AOT 原规模重跑，不以专项通过替代；本机与隔离仿真记录失败及前后版本证据，仿真 wall time 不作为 BEAM 性能结论。

## 替代方案与重审

保持每次独立临时池最简单，保留为默认和自动重启路径，但不复用批量启动成本。隐式 TLS 缓存会延长资源生命周期并隐藏宿主保留，跨线程共享池则破坏现有 Rc/活栈所有权，均不采用。提高 deadline、降低 actor 数或把初始化移出计时不能修复资源成本，也不能作为替代。若以后改为异步初始化、自动重启批量复用或更改池所有权，应另立 ADR，重新验证账户转移、取消与线程边界。
