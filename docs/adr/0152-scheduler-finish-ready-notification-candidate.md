# ADR-0152：Scheduler finish 新增 ready 才广播候选

- 日期：2026-11-20
- 状态：实验后回退（M0 性能候选；保留完整配对证据）
- 关联：[ADR-0093](0093-scheduler-ready-before-park.md) 的停车前发布、[ADR-0150](0150-open-loop-service-latency.md) 的双延迟口径、[ADR-0151](0151-reply-completion-notification-candidate.md) 的独立回退实验

## 背景

worker 完成一个 handler 后，当前 `finish` 无论是否重新入队或投递语言 completion 都执行 `shared.changed.notify_all()`。hot-queue 热 actor 经常仍有后续消息，因此该路径可能仍有大量必要广播；只有没有新增 ready 的完成才可能省略。结果表 Condvar 候选已经因端到端退化回退，本片不改结果表，也不把两个实验的数据混合。

## 候选实现

- `State::enqueue` 返回是否实际把 actor 加入 ready 队列。closed、stale、空 mailbox、owned、stopping 和 already-ready 都返回 false，不改变队列选择、FIFO、迁移或容量。
- `State::maintain` 返回本次是否至少新增一个 ready。每个可投递 completion 都必须独立执行并入队，再聚合布尔值，不能用短路表达式跳过后续 completion。
- `finish` 分别执行当前 actor 的 `enqueue` 和语言请求 `maintain`，只有任一项确实新增 ready 时才在解锁后广播。必须先完成两个状态修改，不能短路。
- 只改 `finish` 的附带广播。宿主 send/request/spawn、语言 effect、stop/close/cancel、worker setup failure 和 close 的通知保持原样；外部结果仍由独立 `ReplyTable` 负责。`claim` 内维护和 actor start error 的原 1ms 重查/退避不在本片扩大修改，timer、4096 steps 片、1ms quantum、mailbox、pending、deadline、ABI v22 和业务均不变。

## 验证与回退

ready 单测核对首次/重复入队、stale、owned、stopping 和替换 generation 的返回值。completion 回归覆盖 mailbox 满、starting、可投递及同次两个 completion 全部入队；第二次维护不得伪报新增 ready。保留 stop/close/cancel、并发失败恢复、cold-stack 拒绝与停车前发布回归。

正确性通过后，复用同一 `beam-multi-counter` 产物和 hot-queue 10000/s，冻结原 runtime 与候选 Release runner，五对交替，保留端到端与 `observed-sent` 服务 p50/p99/p999、全部拒绝/超时、迟发及同轮 RSS/物理占用。任何漏醒、终态或清理回归都回退；没有稳定服务/端到端收益、尾候选退化或内存无改善时也恢复原无条件 `finish` 广播，保留两个 runner、完整 trace 和失败轮次。候选不证明通知是尾延迟根因，不完成 CPU 硬配额、公平性、最大持续吞吐、完整整机内存或 BEAM/MVP 门禁。

## 实验结果与回退

候选通过 425 项 VM 测试和 Clippy，但正确性不等于性能可保留。五对中候选服务 p99 中位数 49.084µs、基线 80.542µs；候选在第 2–5 对服务 p99 较低，第 1 对较高。基线第 4 对发生 35615 次 mailbox_full 和 490 次超时，是整体中位数差异的重要组成，不能删去该轮或用中位数证明稳定机制收益。

候选自身只有 3/5 尾候选通过，第 1/5 对分别 281/460 次 mailbox_full；基线仅 1/5 通过。候选本轮拒绝/超时少于基线，但仍未满足全轮稳定通过。候选 RSS 在五对中四升一降，物理占用两升三降，没有同轮内存稳定改善。综合预定双目标与回退条件，不保留该候选。

已恢复 `enqueue`/`maintain` 的原返回类型和 `finish` 无条件广播，原 runtime 424 项 VM 测试与 Clippy 通过。冻结候选源码、两个 runner、十份完整 trace 和全部失败轮次保留；不重跑换取全绿，不把后续结果解释成候选已修复。实际数据见[验证记录](../validation-scheduler-finish-notification-2026-11-20.md)。
