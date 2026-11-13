# ADR-0067：BEAM 对照端的有界发送前准入

- 日期：2026-11-14
- 状态：已采纳（有界准入对照切片）
- 扩展：[ADR-0066](0066-bounded-scheduler-mailbox-capacity.md)
- 协议：[BEAM 工作负载](../beam-workloads.md)

## 决定

BEAM 原生 `!` 没有有限 mailbox，不能用无界队列对照 Wings 的显式拒绝。为可信测试宿主增加独立 `beam_bounded_mailbox` 模块：固定目标 Pid/容量及一个 signed atomics 单元；发送者以 CAS 在发送前占一个排队位置，actor 在收到消息、开始业务前以 CAS 归还。容量 1..256，满时返回 `mailbox_full`，关闭后返回 `closed`，两者都不发消息。无代理串行代发、无等待者队列、不自动重试；actor/发送者不绕过该入口投递业务消息。

占槽成功的 CAS 是准入线性化点，不是业务提交或 exactly-once 承诺。占槽与实际 `!` 之间允许关闭/退出竞态：这些请求仍按已接受处理，宿主须根据目标 DOWN 结束其 pending；关闭把单元设为终态，不因迟到发布或 start 回退重新开放。`started` 只允许固定目标自己调用，关闭后不开始新业务，旧 gate 不改变绑定、不重定向复用目标。发送者必须是可信受限驱动，不覆盖 native 崩溃在占槽后永久停住的恢复。

该计数包括已准入尚未开始的消息，不含当前 handler；发送前占槽与 Wings starting 保留位置对齐。计数/固定结构成本必须计入未来 BEAM 整机内存，原生 mailbox/信封/分配仍保留正常优化。不是 OS 沙箱、完整资源账本或任意应用可直接复用的公共 SDK。

## 有界验证

新 v4 固定场景在 Wings/BEAM 各启动两个保留完整 4 KiB Bytes/binary 的业务 actor。一个进入有界测试时段内的 CPU Hold，确认开始后排满 1/100/256 队列，下一条显式拒绝且计数不变；另一个以单向消息正常提交 +7。满队列 stop/关闭后所有已接受请求选择失败、旧入口拒绝、子进程结束并清理。Wings 独立检查三级逻辑归零；BEAM 检查 gate 关闭及 monitor DOWN，不伪称 BEAM 有 Wings 资源账本。`initial_value` 只表示 Hold 前实际状态为零，不冒充退出时完整状态导出。

BEAM 模块另做 1/100/256 槽的 FIFO/归还后复用、重复 started 拒绝、非目标 started 拒绝、两个真实发送进程竞争不超额、关闭后不重开及迟到投递检查。并发竞争只要求各发送者到同目标 FIFO，不要求跨发送者全序。独立 Python 核对相同业务结果，真实 BEAM 须显式 `--compare-beam`，默认完整 AOT 仅 Wings/模型。

业务子进程 CPU 5 秒/墙钟 10 秒/禁 core、actor 阶段共享 3 秒、AOT CPU 20 秒/墙钟 30 秒不变。执行 workspace/check/clippy/fmt/MSRV、debug/release 完整 AOT 与平台回归。此处只完成有限准入/关闭正确性，不替代固定 10000 request/s、计划时刻延迟、开环、超时竞争、预热/采样、原生双平台或 BEAM 吞吐/整机内存门禁。
