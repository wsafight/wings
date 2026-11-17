# ADR-0124：Scheduler 的实际恢复区间观测

- 日期：2026-11-18
- 状态：已采纳（公平性诊断前置，尚未通过公平性门禁）
- 延续：[ADR-0072](0072-scheduler-yield-observation.md) 的固定计费诊断和 [ADR-0077](0077-streamed-fairness-measurement.md) 的独立公平性测量。

## 背景与决定

原子让出样本只覆盖已消费的原子请求。步骤预算或 effect 可以先让执行返回 worker，因此没有原子样本不能说明没有切回，也不能据此推导公平性。增加独立的 `SchedulerConfig::slice_samples`，默认 0；显式 1..=65536 时，每个 worker 在启动前创建固定 FIFO，记录每次实际 `ScalarTask::resume` 调用前与返回后的单调时间、完整 SchedulerPid、worker、该 worker 的连续序号和 Suspended/Complete/Failed 结果。不修改原子让出观测，不把所有 Suspended 都叫作原子抢占，也不猜测步骤预算与 effect 的具体原因。

开始时点位于 timer arm 成功之后，返回时点位于 timer disarm 之前；发布样本在 disarm 之后。timer arm 失败没有调用 resume，因此不生成假样本。区间包括恢复调用内的切栈、受管/runtime 执行和返回处理，也包括 OS 暂停该线程的时间；不是独占 CPU 时间。它不覆盖 task 创建、handler 入队等待、返回后的 effect 处理或全部 Scheduler 锁区间，不将相邻样本间隔全部归因给 VM。

默认路径不分配 FIFO、不增加诊断时钟或诊断锁；Shared 中新增的配置和空描述符仍按实际大小计费。启用时，在分配前向 tenant/VM 预留 worker 控制、固定槽和原生 mutex 后备存储，初始化后再发布；启动任一失败回滚原预留。最后 SchedulerHandle 持有诊断容量和账户租约，样本不保留 actor、模块、业务值、回复或活栈。

满 FIFO 拒绝新样本并累加 dropped，不覆盖旧样本、不等宿主腾槽；序号仍按全部返回推进。宿主通过 `take_slice_sample(worker)` 消费，`slice_statistics(worker)` 查询 retained/taken/dropped/published 和三种结果总数。无效 worker 显式拒绝，关闭模式返回 None；取样不重置序号或累计计数。计数饱和置 overflowed，不静默回绕。单队列查询是一致快照，完全停止业务后核对 `published = retained + taken + dropped = suspended + completed + failed`；溢出时不作完整性声明。

ABI v21、manifest v2、业务状态/消息、强制检查点、预算/取消、原有片预算与 quantum 均不变。新配置是未发布源码 API 增量，不把观测模式冒充默认性能。

## 验证与边界

检查关闭/容量边界、固定 FIFO 顺序/丢样/循环复用、完整身份、时钟区间、计数饱和、tenant/VM 准入失败回滚以及末 handle 清理。真实 AOT 在一/双 worker 上比较开关，步骤片下确认实际 Suspended 样本、持续热 actor 存在时健康 actor 推进、正常完成和失败/停止后的清理。测试保持原有受限子进程，不提高业务时限。

这些是观测来源与有界调度正确性检查，尚非 30s/60s 五对公平性实验；没有给 p99、尾延迟、过载分组、CPU 硬配额、原生双平台或 BEAM 吞吐/整机内存门禁作通过声明。后续若纳入长测，必须新增冻结协议和独立重放，保留原 v8 原子请求事件及历史结果。

## 替代方案

提高片预算或 quantum 以制造更多原子样本会改变被测工作；将步骤片伪装成原子请求延迟会混淆起点；无界保存全部调用会改变内存负载，均不采用。诊断以后若要测入队等待或 effect 时长，应独立定义边界，而不是扩张本区间的含义。
