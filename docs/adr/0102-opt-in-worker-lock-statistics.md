# ADR-0102：可选的 worker 调度锁区间统计

- 日期：2026-11-17
- 状态：已采纳（有界诊断切片，功能验证另记）
- 扩展：[ADR-0054](0054-bounded-multi-worker-scheduler.md) 的 worker 诊断；沿用 [ADR-0072](0072-scheduler-yield-observation.md) 的默认关闭、固定存储和末 handle 租约原则，不混用两类指标。
- 背景：[ADR-0100](0100-coalesced-deadline-timer-notifications.md)、[ADR-0101](0101-empty-language-request-maintenance.md) 的局部优化没有建立稳定收益，后续需要先获取具体路径证据。

## 决定

`SchedulerConfig::lock_statistics` 默认 false。显式启用时，创建前按 worker 数量向 tenant/VM 预留固定计数存储，再分配；失败回滚，不提高任何业务资源上限。不动态保留逐条事件、Pid、消息或业务对象，不增加诊断线程或诊断 mutex。

每个 worker 分别累计四类原有共享锁区间：claim（维护语言请求和取任务）、start（启动成功/失败后的状态交接）、snapshot（挂起后的执行快照）、finish（提交结果、退出清理和维护请求）。记录获取次数、获取前至获得锁的墙钟时间之和/最大值、获得锁至 guard 结束的墙钟时间之和/最大值。锁仍按原位置获取和释放，指标在释放原锁后用独立原子计数发布。

默认关闭时这些路径不额外读时钟、更新统计原子或分配诊断存储；仍有启用分支和 guard 包装，不承诺指令数或二进制完全不变。启用时每个区间增加三次单调时钟读取和固定原子更新，扰动须独立评估。计数和纳秒值饱和，不回绕；任何字段或时长转换饱和时置位该类的 `saturated`，不能继续将数值当精确总数。

`SchedulerHandle::lock_statistics(worker)` 返回复制的可选统计；非法 worker 拒绝、关闭状态仍可查询。只发布已结束的锁区间，尚未释放的区间连 acquisition 次数也不入统计；锁内时长止于释放前最后一次读取，不含 unlock 指令本身。活动查询为多个原子字段的独立读取，不是跨字段或跨 worker 一致快照；停止并 join 后才核对守恒，关闭不重置。存储随最后 handle 一起释放，不保留 actor/模块。

## 明确不覆盖

- 不测宿主 API、语言 effect、资源账本、结果表或 timer 自身的锁，也不完整覆盖 worker park 决策；不从四类值推算总锁开销。
- Condvar 等待不进入上述 guard，不能把睡眠算作持锁；唤醒后重新获锁也不计入本统计。
- 时长是经过的墙钟，含 OS 抢占与锁内其他等待，不是线程 CPU 时间、纯 mutex 自旋时间或业务执行时间；首次取任务/发布结果也不等于消息端到端时延。
- 累计值和最大值不是 p99；不保留直方图、事件时间线、CPU 核归属或观察丢样。默认关闭与启用运行不能直接作为未扰动性能对照。

ABI v15、语言语义、请求双端生命周期、强制检查点、资源容量、预算、quantum 和原测试时限不变。当前不据此修改已回退优化或宣布 M0/MVP/BEAM 双门禁通过。

## 验证与回退

检查默认 None、索引拒绝、四类计数、聚合/最大值、饱和、并发查询，以及最后 handle 的计费释放和 tenant/VM 准入回滚。确定性竞争测试用 barrier/channel 控制持锁，不用墙钟阈值断言性能；独立测试验证记录发生在原锁释放后。真实 AOT 一/双 worker 对比开启/关闭下相同请求/已提交值、停止后完整起止次数与清理。

运行 debug/release workspace 独立 harness、完整受限 AOT、check/clippy/fmt/MSRV 与真实 BEAM smoke，保持原 CPU/墙钟/业务阶段。额外观测只作为诊断演示，不能替代五对长测、可靠 CPU 隔离、原生双平台或同负载吞吐/整机内存验收。回退关闭配置即可；若修改默认路径、覆盖范围或发布一致性，须重新记录契约。

实际 macOS AArch64 的开关对照、测试数、原限制超时与重跑证据见[2026-11-17 验证记录](../validation-lock-statistics-2026-11-17.md)。
