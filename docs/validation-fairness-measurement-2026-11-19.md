# 公平性长测执行与探针首执行修正（2026-11-19）

基于 `fb70eb5`，执行 [ADR-0077](adr/0077-streamed-fairness-measurement.md) 冻结的 v8 公平性长测协议（30s 预热/60s 采样、五对、百万 checked 加法热负载 + 100 actor 每 10ms 同刻心跳、mailbox 1、255+1 pending），macOS 26.6.2 AArch64、真实 OTP 29 对照。原业务、时段、重复、CPU 240s/墙钟 130s/阶段 120s 限制与 `fairness_gate_proven=false` 语义不变。

## 首次失败保留与探针首执行修正

首轮（`target/fairness-measure-20261119/`）在第一个 Wings 样本的原生内存探针处失败：新编译探针的首次执行超过原 2 秒快照超时，`1-wings.json` 记录 `invalid_experiment`、wall=2.02s，业务未开始，不重跑该目录。

对照实验（证据 `target/probe-first-exec-20261119/`）：两个分别编译到全新路径的探针，首次执行 real=0.74s/0.39s，随后两次均 0.00s。首次执行成本真实存在且可变；本记录不据此断言 TCP 启动超时的根因。

`measure_open_loop.py` 原本就在测量前做 preflight 快照，`measure_fairness.py` 没有。本轮为后者补上相同的 `memory-setup.json`/`memory-preflight.json` 前置：探针首次执行移出被测阶段，2 秒快照超时等所有原限额不变。

随后 open-loop 扫描的首轮又在 preflight 自身遇到探针首执行超过 2 秒（`target/openloop-scan-20261119/`，业务未开始，目录保留）。因此两端 preflight 进一步改为与探针编译一致的 30 秒**设置阶段**工具期限（`measure_memory.snapshot(..., timeout=30)`，默认参数仍为 2）；**被测样本的 2 秒快照期限不变**。本记录的五对数据在此改动前取得，被测阶段行为相同，未重跑。

## 最终五对结果

修正后顺序执行完成全部十轮（`target/fairness-measure-final-20261119/`，exit=0）：

| 对 | Wings 尾候选 | BEAM 尾候选 | Wings mailbox_full | Wings p99/p999 | BEAM p99/p999 |
|---|---|---|---|---|---|
| 1 | 通过 | 通过 | 0 | 637708/696000 ns | 328283/414241 ns |
| 2 | **未过** | 通过 | 56 | 640417/1274000 ns | 330292/478295 ns |
| 3 | 通过 | 通过 | 0 | 629000/845625 ns | 376292/1073255 ns |
| 4 | 通过 | 通过 | 0 | 676042/1769875 ns | 490159/4551979 ns |
| 5 | 通过 | 通过 | 0 | 644625/1419666 ns | 347108/527650 ns |

全部十轮 600000 条心跳回复/零超时/零 pending_full；Wings 第 2 对保留 56 次 mailbox_full（600000 条中 599944 条按期回复），不重跑、不删除。Wings 进程树+驱动 RSS 峰值 31178752–210583552 B，BEAM 113868800–261963776 B；两端峰值随对次上升，未归因于 VM 业务或缓存，口径为驱动+捕获后代的非原子采样，探针/helper 另计。逐轮事件、内存与汇总见输出目录 `report.json` 及各 `*.json`。

## 范围

CPU 硬配额、公平性过载分组、监测开销验收、原生双平台与同业务吞吐/整机内存门禁仍未完成；本机结果为探索性集成测量，不是公平性或 MVP 门禁通过。启动根因、完整账本与 BEAM 双目标状态不变。
