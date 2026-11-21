# ADR-0066：Scheduler mailbox 的有界 256 槽上限

- 日期：2026-11-14
- 状态：已采纳（有界准入切片）
- 部分替代：[ADR-0054](0054-bounded-multi-worker-scheduler.md) 的 mailbox 64 槽上限
- 负载依据：[BEAM 热点协议](../beam-workloads.md)

## 决定与约束

已冻结的 hot-queue 负载要求 mailbox 100，当前 Scheduler 只接受最多 64，不能删改负载绕过。将 Scheduler 的可配置 mailbox 上限扩至 256，默认/调用方显式容量不变，单 worker ScalarWorker 的上限不变；同模块语言 spawn 继承原目标的已配置容量，监督重启保留同容量。

不扩大全局宿主结果/语言 pending 的各 256 槽上限。每 actor 固定环缓冲在创建前按实际槽数向 process/tenant/VM 预留，消息/请求票据仍分别在投递前三级准入。参数校验在创建资源前拒绝 0/257/极大值，满队列返回原 `mailbox_full`，失败不修改 state/队列或泄留预留。

mailbox 计尚未开始的已接受消息，不包括已成功开始的 handler。认领但尚未 start 的一条消息以 `starting` 额外占原队列位置，以便启动准入失败可恢复队首，不允许借窗口突破容量；既有 FIFO、ready 去重、活栈原 owner 和 stop/关闭语义不变。没有增添无界等待者或自动重试。

## 验证要求

64/100/256 槽在活 handler 暂停时可准确排满，额外 send/request 拒绝且三级计费不变；共享结果表不突破 256、其他 actor 可推进、满队列仍可停止、所有请求终结一次、旧 Pid 拒绝、释放最后 handle 后逻辑归零。配置 0/257/usize::MAX 无部分创建；低 process/tenant/VM 配额失败回滚。

固定表内直接检查 100/256 槽的 FIFO 回环与 starting 保留位置：认领后尚未 start 不能额外准入，失败恢复后原队首次序不变，弹出/补满跨环后顺序及消息票据恢复基线。

真实受限 AOT 使用完整 4 KiB Bytes state、强制检查点 Hold 与满 100 条队列，验证其他 actor、显式拒绝、取消/退出与最终清理。沿原 CPU 20 秒/墙钟 30 秒/禁 core，执行 workspace/check/clippy/fmt/MSRV、debug/release 完整 AOT 和平台回归。这只是 hot-queue 准入的实现前置，尚无等价 BEAM 有限准入层、开环、10000 request/s、尾延迟或吞吐/整机内存结论，不宣布 MVP 完成。
