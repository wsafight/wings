# ADR-0150：开环入队后服务延迟摘要

- 日期：2026-11-20
- 状态：已采纳（M0 离线测量诊断）
- 补充：[ADR-0069](0069-streamed-open-loop-measurement.md)、[ADR-0147](0147-bounded-mailbox-full-attribution.md) 的原始事件与离线摘要

## 背景

v6 现有 p50/p99/p999 使用 `observed - planned`，同时包含发生器迟发、发送前准入、mailbox 排队、worker 执行、结果发布和调用者观察。发生器 OS 迟发已经关联部分 `mailbox_full`，但现有单一延迟口径不能分离迟发与入队后的 VM/BEAM 服务路径，容易把不同机制混为一个优化目标。

## 决定

- 保留原 40 B 事件、计划时钟、全部预热/采样事件和 `observed - planned` 端到端口径，离线 summary 另增 `service_latency`，对 kind 1/2 的真实回复计算 `observed - sent`。
- 新口径与既有 `generator_lateness` 并列，包含超时后的实际回复，使用相同的 nearest-rank、三轮固定 16-bit radix 桶和少于 10000 不报 p999 规则，不另存全量样本。
- `service_latency` 仍包含 mailbox 等待、worker 唤醒、业务执行、结果发布和调用者观察，不是 handler CPU、纯排队或 GC 时间。`sent` 是实际接受到达时间，不是计划时间；拒绝没有回复，不伪造服务样本。
- 新字段不参与拒绝/超时计数、`tail_candidate_pass`、饱和扫描停止或内存判定，不删除迟发样本，不改变 Wings/BEAM 业务、产物、资源上限、30s/60s 时段或五对协议。
- 新采集与历史冻结 trace 均可由同一离线验证器重放；历史报告文件不回写。性能候选必须并列报告两个延迟口径，不能只用服务时间改善掩盖发生器迟发或端到端退化。

## 验证与边界

合成事件覆盖 `sent != planned`、迟到回复、多字长分位和畸形 trace，核对端到端与服务时间分别选秩。随后重放冻结 hot-queue 原始事件，保留原拒绝关联、状态和尾候选结果。该摘要只做归因分界，不定位具体 VM 根因，不完成 CPU 硬配额、公平性、原生双平台或同业务 BEAM 吞吐/整机内存双门禁。
