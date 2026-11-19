# 开环 mailbox_full 归因：发生器追时刻突发（2026-11-19）

基于 `b29f3b3`，对[开环扫描](validation-open-loop-scan-2026-11-19.md)与[公平性长测](validation-fairness-measurement-2026-11-19.md)中间歇出现的 mailbox_full 做诊断。未修改 VM、ABI、业务协议或任何期限；新增仅为 `target/mailbox-full-diag-20261119/driver.py` 的有界诊断驱动（复用 `measure_open_loop` 原限额/校验，Wings-only、10000/s、`--observe-locks`、两轮）。

## 复现与锁证据

两轮 hot-queue 10000/s：run-1 零拒绝、run-2 **211 次 mailbox_full**（采样段；run-1 预热段另有 263 次，按协议不计入采样统计）。run-2 的 worker 四阶段锁统计：acquisitions 约 180 万/worker，**max_held_ns ≤ 72834、max_wait_ns ≤ 94167**——没有任何接近 11ms 的锁区间，排除调度锁竞争这一机制。worker 函数 CPU 各约 5.5s/90s。

## 逐条 trace 归因（run-2）

- 211 次拒绝全部落在 **0.849ms 的单一窗口**（sent=32230854042..32231703083 ns）。
- 拒绝事件的前一到达间隔中位数 **1000 ns**、最大 17416 ns：发生器正在以约百万条/秒补发。
- 首次拒绝时发生器迟发（sent−planned）已达 **28.85ms**；全轮最大迟发 **43.48ms**，244 次 >20ms。
- hot-queue 90% 流量到 actor 1（9000/s、mailbox 100）：发生器停顿 D 毫秒后补发约 9·D 条，而热 actor 排水约 35µs/条；**D≈11ms 即达到满槽阈值**，与观测完全吻合（正常各轮迟发峰值 6.7–11.5ms 均零拒绝；29.26ms→95 次、43.48ms→211 次）。

结论：拒绝链为"发生器（或其所在进程）被 OS 延迟数十毫秒 → 追时刻密集补发 → 热 actor 满槽 → 有界准入按设计拒绝"。不是 VM 内部锁或长操作停顿；排水速率本身不是瓶颈（正常时延 p99≈35µs）。扫描报告各轮对照：BEAM 五轮迟发峰值均 ≤11.5ms 故零拒绝，并不证明 BEAM 在同等补发下不会拒绝。

## 公平性长测的初始候选与后续修正

fairness 第 2 对 Wings 的 56 次 mailbox_full 与该轮最高发生器迟发峰值（9.86ms，其余 Wings 轮 0.9–8.8ms 零拒绝）同现；本轮未做逐事件归因，当时只列为同类机制候选，不断言阈值关系。[ADR-0147 的后续重放](validation-mailbox-full-attribution-summary-2026-11-19.md) 已补齐逐事件证据：56 次全在同一 planned 的 burst，迟发仅 0.103–0.224ms，没有数十毫秒迟发后补发特征，因此不再把全局峰值同现当作同一机制证据。

## 边界与后续

- 数十毫秒级 OS 延迟的来源**未定位**（不归因 Spotlight/安全扫描/压缩内存等）；两次大迟发恰在 Wings 轮出现不构成 Wings 导致 OS 延迟的证据。
- 发生器追时刻补发是冻结开环协议的既定行为；是否改为有界补发/跳过过期计划属于协议变更，须另行 ADR 并重立基线，本轮不改。
- 性能门禁解读时应把"发生器迟发关联的拒绝"与 VM 自身背压分开；本记录不关闭公平性、过载分组、CPU 硬配额、原生双平台或 BEAM/MVP 门禁。
- 证据：`target/mailbox-full-diag-20261119/`（驱动、两轮 events/manifest/result）、`target/openloop-scan-multi-20261119/`、`target/fairness-measure-final-20261119/`。
