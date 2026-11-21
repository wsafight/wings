# 开环 hot-queue 饱和扫描执行（2026-11-19）

基于 `fb70eb5`，执行 [ADR-0069](adr/0069-streamed-open-loop-measurement.md) 冻结的 v6 开环协议 `--scan`：hot-queue（100 actor×4096 B、mailbox 100、100ms deadline、256 pending），基准 10000/s 起翻倍，每档五对 Wings/BEAM 交替、30s 预热/60s 采样，任一尾候选失败即停。macOS 26.6.2 AArch64、M3 Pro、真实 OTP 29；原 CPU 240s/墙钟 130s/阶段 120s、trace/RSS 上限与 `maximum_sustainable_throughput_proven=false`、`memory_gate_proven=false` 语义不变。

## 两次失败保留

1. `target/openloop-scan-20261119/`：preflight 自身遇到新编译探针首次执行超过原 2 秒快照超时，业务未开始。此后两端 preflight 改为与探针编译一致的 30 秒设置阶段期限，**被测样本 2 秒期限不变**，见[公平性记录](validation-fairness-measurement-2026-11-19.md)。
2. `target/openloop-scan-final-20261119/`：执行者误用 `examples/beam-counter.wings` 产物（`Configure` 为 2 参），runner 按 3 参调用，0.83s 内以 `scheduler handler identity, phase or arguments mismatch` 退出（`oracle_error`/exit 3）。这是产物选择错误，不是 VM 缺陷；正确产物为 `examples/beam-multi-counter.wings`，与历史长测一致。后续可为测量入口补 handler 签名前置校验，本轮未改。

## 最终结果：基准档触发停止

`target/openloop-scan-multi-20261119/`（exit=0，`scan_sufficient=true`）在基准 10000/s 即停止：Wings 第 3 对出现 95 次 mailbox_full，尾候选未过，协议不再翻倍。十轮均收齐全部 600000 到达/结果、状态逐字节核对通过、零超时、零 pending_full：

| 对 | Wings 尾候选 | Wings mailbox_full | Wings p99/p999 (ns) | BEAM 尾候选 | BEAM p99/p999 (ns) |
|---|---|---|---|---|---|
| 1 | 通过 | 0 | 34750/69750 | 通过 | 5459/57209 |
| 2 | 通过 | 0 | 34458/72208 | 通过 | 5692/53221 |
| 3 | **未过** | 95 | 35708/80083 | 通过 | 13588/59046 |
| 4 | 通过 | 0 | 35583/70000 | 通过 | 14522/61980 |
| 5 | 通过 | 0 | 34750/72417 | 通过 | 13226/56493 |

Wings 按期回复其余各轮 10000.0/s、第 3 对 9998.4/s；进程树+驱动 RSS 峰值 Wings 46809088–67502080 B、BEAM 89980928–103497728 B。Wings 第 3 对 89 个内存样本全部完整（physical footprint 峰值 35865656 B），发生器迟发 p99=250 ns。逐轮原始事件/内存/manifest 在输出目录，`report.json` SHA `b329fbf3…4aac14`；fairness `report.json` SHA `01590a23…4026c84`。

## 解读边界

停止判据由单轮 95/600000 次拒绝触发，是间歇 mailbox 满的信号，不是最大可持续吞吐证明（报告字段保持 false）。Wings 的 mailbox_full 与 [ADR-0077](adr/0077-streamed-fairness-measurement.md) 公平性长测的 291 次、本轮 fairness 的 56 次队满同属待定位现象，不重跑消除、不归因。BEAM p99 约 5–15µs 与 Wings 约 35µs 的差异是无 CPU 硬配额、主机隔离未验证下的探索性对照，不是性能门禁结论。完整账本、启动根因、原生双平台与同业务 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。

## 复现入口

```sh
target/release/wings-compile build examples/beam-multi-counter.wings -o target/openloop-artifact
WINGS_PROFILE=release python3 scripts/measure_open_loop.py --case hot-queue --artifact target/openloop-artifact --output target/openloop-scan-new --scan
WINGS_PROFILE=release python3 scripts/measure_fairness.py --artifact target/fairness-artifact-20261119 --output target/fairness-measure-new
```
