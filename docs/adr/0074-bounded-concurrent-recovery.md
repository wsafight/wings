# ADR-0074：有界并发取消、超时与监督恢复回归

- 日期：2026-11-14
- 状态：已采纳（现有契约的集成回归，不是新恢复协议）
- 依据：[ADR-0060](0060-shared-scheduler-reply-table.md)、[ADR-0054](0054-bounded-multi-worker-scheduler.md) 与 [ADR-0056](0056-multi-worker-supervision.md)

## 决定与边界

补充真实 AOT `concurrent_recovery`，分别在一/双 worker 中用两个保留完整 4 KiB Bytes 的 actor 验证现有请求与恢复契约。不改 ABI、语言、执行路径或公开 API，不增加重试、撤回或事务语义；本片不是 BEAM 对照或性能门禁。

目标 actor 在 Hold 中替换候选状态、执行 collect 后保留活栈循环；两个真实宿主发送线程各发送八条 Add，总计排满 mailbox 16。各线程前两条 deadline 为零，接着两条取消并消费结果，余下四条保持等待。已超时/取消结果不释放仍被消息 publisher 持有的请求额度；排队仍为 16，目标请求额度仍为 16。下一条发送必须 `mailbox_full` 且无净预留。总结果表 17 槽，剩下一槽让健康 actor +7 并正常提交；一 worker 下也须推进，不把被抢占 Hold 当作空闲或已提交。

随后两个消费者与宿主 stop 从固定屏障并发开始。剩余请求只允许 Cancelled 或 Failed，不强求某种线程交错；每个结果只消费一次，既有超时/取消不得被 stop 覆盖，所有 publisher 释放后请求额度归零。旧 Pid 拒绝，stop 不重启；新 child 复用后初始状态为零、旧排队 Add 不重放，健康 sibling 保持 +7。

新 child 的 Fail 在替换候选 Bytes 并 collect 后除零。宿主取消结果与实际失败竞争，结果允许 Cancelled/Failed，但取消不能撤回消息：两次都必须有原 Pid/Fail/division-by-zero 的监督退出，退出快照的 4 KiB 长度和 32 B 前缀对应最后提交的零值而非候选 99，不冒充完整状态导出。只允许一次全新 heap/state 重启，第二次强度超限，旧 Pid 拒绝。关闭 supervisor/结果/模块/tenant 后核对 VM 零。

固定两 sender、16 消息、17 结果槽、两 actor、每配置同一个 3 秒阶段截止；外层沿用完整 AOT CPU 20 秒/墙钟 30 秒/禁 core，不通过增加等待、降低工作量或重新发送失败消息规避失败。独立回复表单测以每种情况 16 次固定屏障竞争取消与发布/丢弃，覆盖零 deadline、单一终态、不可重复消费、请求归零及槽复用无旧结果。

## 验证与重审

后续 [ADR-0121](0121-persistent-cancellation-recovery.md) 复用原单轮业务验证持续 Scheduler 内的槽、取消和重启生命周期；普通三轮和显式长测分开，不改变本记录原容量/时限或恢复语义。

真实 AOT 必须通过 debug/release 完整入口与 Linux 隔离运行；workspace/check/clippy/fmt/MSRV 与依赖隔离保持。首次 fixture 误向活栈 actor 请求 idle Bytes snapshot，修正测试断言为必须拒绝，不能为测试放宽运行时查询边界。保留失败证据，不把短时回归称为完整并发故障、长时公平性、完整资源账本或原生双平台门禁。

只跑回复表单测无法覆盖活栈、GC、状态、mailbox 与监督集成；只跑顺序停止又无法覆盖取消/stop 竞争，故保留两层回归。若引入语言级监督、消息撤回、引用回复或新的超时语义，另立 ADR 并更新独立模型，不改本次既有契约来适配实现。
