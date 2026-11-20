# ADR-0151：无等待者结果完成通知省略候选

- 日期：2026-11-20
- 状态：实验后回退（M0 性能候选；保留测量驱动与原始证据）
- 关联：[ADR-0099](0099-unready-result-poll-fast-path.md) 的未就绪轮询快路径、[ADR-0150](0150-open-loop-service-latency.md) 的双延迟口径

## 背景

hot-queue 稳态通过 `IngressRequest::try_take` 轮询结果，不在结果表 Condvar 上等待；`wait` 主要用于 Configure。当前 `ReplyTable::finish` 每次发布仍无条件 `notify_all`，但零等待者通知的实际成本尚未测量。Scheduler finish 广播和 ByteBuffer 复用是独立候选，不与本片混改。

## 候选实现

- 将结果表锁内存储改为 `ReplyState { slots, waiters }`。`wait` 必须在持有同一结果表锁、调用 `wait_timeout` 前增加计数，返回并重新持锁后立即减少；finish 在同一锁内读取计数，解锁后仅在计数非零时广播。
- 计数是整表保守值：其他槽有等待者时仍允许广播，不按槽拆分。注册与完成共用一把锁，完成不能穿过注册窗口；已通知但尚未重新获锁不减少终态，也不改变 deadline、cancel 或 generation。
- `cancel` 的无条件通知、原子 readiness、`try_take`、双端票据、终态优先级、发布者 Drop 的 Failed、槽复用和资源清理不变。新增 `usize` 计数通过变化后的 `size_of::<ReplyTable>()` 计入原三级字节准入，不另加重复项，也不提高额度。
- 不改变业务、消息/回复、Scheduler、timer、mailbox、pending、ABI v22、40 B trace、30s/60s 时段或五对协议。默认路径没有新增诊断开关或逐请求动态分配。

## 验证与回退

定向回归覆盖零等待者发布、两个已注册等待者同时完成、计数归零、正 deadline、publisher Drop、cancel/完成竞争和槽复用；精确字节测试显式包含计数存储。真实 AOT 保留跨线程 ingress、监督与清理回归。

性能使用同一 `beam-multi-counter` 产物和 hot-queue 10000/s，分别冻结基线/候选 Release runner，五对交替，保留端到端与 `observed-sent` 服务 p50/p99/p999、全部拒绝/超时、迟发、RSS/物理占用。任何漏醒、终态或清理回归都回退；即使正确，只要五对没有稳定服务/端到端收益、观察扰动上升或同轮内存恶化，也恢复无条件 finish 通知并保留原始数据。候选不修复发生器迟发，不据此宣称最大吞吐、整机内存或 BEAM/MVP 门禁通过。

## 实验结果与回退

首次部分 A/B 在独立复核发现 `waiters` 被 `size_of::<ReplyTable>()` 与额外 `size_of::<usize>()` 重复计费后作废，修正后使用新 runner 和新目录完成五对。正确性回归通过不代表性能可用：候选服务 p99 中位数 80.333µs，基线 48.667µs；候选第 2/4 对分别出现 170/135 次 mailbox_full 并触发尾候选失败，基线五对全部通过且零拒绝。候选只有第 4/5 对服务 p99 略低，其余三对显著更高，同轮内存也无稳定改善。

因此恢复 `ReplyTable::finish` 的原无条件通知，不保留 waiters 计数或额外账单。冻结候选源码、两个 runner、完整事件与部分作废实验均保留；不把后续基线复测或 fairness 结果解释为该候选已修复。实际数据见[验证记录](../validation-reply-notification-candidate-2026-11-20.md)。
