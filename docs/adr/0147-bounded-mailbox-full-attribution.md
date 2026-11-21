# ADR-0147：有界离线 mailbox_full 关联摘要

- 日期：2026-11-19
- 状态：已采纳（离线证据派生；不改变拒绝或门禁判定）
- 延续：[ADR-0069](0069-streamed-open-loop-measurement.md)、[ADR-0077](0077-streamed-fairness-measurement.md)

## 背景

开环 run-2 的 211 次 mailbox_full 已用一次性离线分析关联到发生器数十毫秒迟发后的追时刻补发，但结构化 manifest 只有拒绝总数和全局迟发分位。fairness 的 56 次拒绝只有与该轮全局迟发峰值同现的证据，无法逐条判断是否为同一机制。重复手写分析不利于复核，也不能把全局同现当作逐事件因果。

## 决定

v6 open-loop 与 v8 fairness 的离线 trace 验证器在 summary 增加 `mailbox_full_attribution`：

- `version=1`、`total` 记录 warmup 与 sample 的全部 kind 3 到达；`retained`、`truncated` 和 `events` 表示派生摘要的保留范围。
- 按 trace 顺序最多保留前 1024 条；超出后只增加 `total` 并设置 `truncated=true`，不覆盖旧条目。原始 trace 仍是完整权威证据。
- 每条保留 `sequence`、1 起始 `actor`、`phase`、`planned_ns`、`sent_ns`、`generator_lateness_ns=sent-planned` 和 `previous_arrival_gap_ns`。首条到达的前一间隔为 null，其余为当前与上一条 admission（kind 0/3/4）的 sent 差，不把完成事件当作到达。
- open-loop 按计划速率重算 planned，fairness 按原 100 actor/10ms 同刻 burst 公式重算 planned；阶段继续由 planned 是否进入采样段决定，不使用完成时刻改判。

解析只在测量后的离线验证发生，不在 Rust/BEAM 运行端或发生器热路径增加时钟、分配和输出。mailbox_full/pending_full/timed_out、延迟分位、拒绝比例、`tail_candidate_pass` 和扫描停止判据均不读取该派生字段，保持原值。

## 范围与限制

这些字段是**观测关联**，不是因果标签，不定位 OS 延迟来源，也不能仅凭密集到达把拒绝归给发生器或 VM。门禁解读必须结合实际迟发、同刻 burst、actor 路由和原始前后事件；当前拒绝迟发很小并不能排除前一 per-actor burst 迟发或未排尽，需要时须回到原 trace 核对前次同 actor 到达/完成。新增字段有界但可以为空；空数组不证明没有背压以外的故障。

验证模型覆盖首次/采样拒绝、非零迟发、前一到达间隔、阶段和 1024 条截断，并重放既有 211 次与 56 次拒绝 trace，不重跑长测。完整公平/过载/服务故障、CPU 硬配额、完整账本、原生双平台和 BEAM/MVP 门禁保持开放。实际结果见[记录](../validation-mailbox-full-attribution-summary-2026-11-19.md)。
