# 恢复区间长测验证（2026-11-18）

基于 `fd634af`，契约见 [ADR-0125](adr/0125-streamed-resume-fairness-diagnostics.md)/[0126](adr/0126-bounded-resume-trace-retention.md)。本机 Apple M3 Pro、36 GiB、macOS AArch64；这是显式启用的诊断，原 v7/v8、ABI v21、单 worker/片预算 4096/quantum 1ms、业务/CPU/墙钟均不变。

## v12 原始失败

首次五次计划停在第一轮，约 51.648s 返回 `fairness resume history limit`，未完成原 60s 采样，判为 invalid_experiment。8000000 条恢复保留上限没有提高，原始文件/冻结源码/程序/环境/内存探针在 `target/resume-fairness-macos-20261118/`；日志 `/tmp/wings-resume-fairness-measure-20261118.log` 和目录中的 `1-wings.stderr.txt`。没有原完整结果，不能补造最终状态、丢样总数或清理回执，也没有重跑该 v12 实验。

## 新 v13 保留策略

新增命令、协议与新目录。保持原 8000000 条/512000000 B、1024 FIFO 和 64 KiB 落盘缓冲，满历史后继续原业务，只累加独立 trace_dropped；FIFO dropped 仍分列。五次固定 Wings 运行不是 Wings/BEAM 五对性能比较，同时启用原子请求观测也不将两种时间混合。

```sh
cargo build -p wings-vm --example beam_fairness --release
target/release/wings-compile build examples/beam-fairness.wings -o target/resume-fairness-aot-new
WINGS_PROFILE=release python3 scripts/measure_slice_fairness.py \
  --artifact target/resume-fairness-aot-new \
  --output target/bounded-resume-fairness-new --observe
```

原业务仍完整核对 900000 次计划、真实结果/提交值与最后清理。恢复流独立核对序号、单 worker 区间不重叠、完整 Pid 映射与计数；没有缺样时逐次对应接受时间/完成时间，Configure 必须在业务起点前，已完成 handler 后的空闲不算可恢复等待。关联表固定上限 1280；发现 FIFO 缺口或已知历史前缀结束就停止后续配对，避免为不可见返回堆积无界历史。

报告恢复墙钟区间、Suspended 后到下次恢复的等待、跨采样终点的等待下界及每 actor 子集；缺样为 complete=false，分位明确仅针对保留数据。离线验证器从冻结源码在另一个 CPU 240s/墙钟 130s/core 0 的子进程执行，内存开销不当作 VM 业务堆或默认性能。

## 验证状态

独立模型拒绝 38 组伪造历史，覆盖区间/身份/状态/计数、业务前执行或宿主结果后返回、完整与缺样、前缀关闭及两类丢样拆分；日志 `/tmp/wings-bounded-resume-model-initial-20261118.log`。Rust 单测检查原上限、显式丢样、v12 满额失败和计数溢出，不分配 800 万条模拟额度。

v13 冻结五轮 Wings 结果位于 `target/bounded-resume-fairness-macos-20261118/`，统一报告为公平性/内存/BEAM 门禁均未证明。每轮计划 600000 个采样心跳，另有 300000 个预热心跳；拒绝仍按原协议计数，不重试。心跳延迟分位只包括成功回复，队满另列：

| 轮次 | 采样回复 | 队满 | 超时 | 心跳 p99 / p999 / max（ns） | 恢复 trace 丢弃 | trace 最后保留时间（观测起点后） |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 600000 | 0 | 0 | 629250 / 6139750 / 8492709 | 6100238 | 50.889 s |
| 2 | 600000 | 0 | 0 | 632625 / 5416875 / 8814958 | 6019179 | 51.411 s |
| 3 | 599929 | 71 | 0 | 630833 / 5625125 / 13242208 | 5995752 | 51.498 s |
| 4 | 600000 | 0 | 0 | 629458 / 3811666 / 8733333 | 5998281 | 51.475 s |
| 5 | 599914 | 86 | 0 | 626000 / 5086750 / 13293125 | 6038195 | 51.406 s |

每轮均保留 8000000 条恢复事件，FIFO dropped=0，trace dropped 为 5995752–6100238；五轮心跳合计 2999843 次成功回复、157 次队满、0 超时。全部业务终态、状态核对和清理通过，剩余根 232 B。恢复区间只覆盖观测起点后约 50.889–51.498 秒，剩余区间因达到固定 trace 上限而缺样；其完整 8M 前缀重放通过，但只描述该前缀。前缀中的 `resume_wall_time` p99 为每轮 5000 ns，`resumable_wait` p99 为 8709–9792 ns；不可外推到完整采样窗，也不是独占 CPU 时间。原子让出观测每轮仅 0/1/3/3/1 个样本，不能据此验收延迟。

每轮平台探针均有完整样本，scope 是驱动进程及其捕获的后代，不归因给 VM：RSS 峰值 29917184–35405824 B，macOS physical footprint 峰值 21808088–23266216 B。此次未做 BEAM 对照、CPU 硬配额或监测开销验收，不是内存门禁。

冻结 runner SHA256 `f293e8c92a9702c283b213c935d310c4691373d5b1bf521f3b5cb36470f138e1`，checker SHA256 `255720c72ad145eaa84507227b53d23210ba8035ae4a11d3c326357ddca4fb3a`。五轮 trace SHA256 依次为 `1f056c664887c04cf9c8a9140ed849c0875641c54bdb87c26d16861b7003d01e`、`ff64a1f0f937c67cf110b1e19c788bf744b6374419057f295a0f81ab077d4dc4`、`6b8cb1f37944c8aa9785e616cb44b2346e38f0d3b7cb0b6e85316a96a2916947`、`67d2dbc08c183b4d0a92ace87e3a85d3c7a6589afd89068d1ded8e61348e3399`、`aeaa0d88578f057136527eb253e4fc0cb712d9a6f7dcd4709b8ab68808d1105b`。原 v12 失败与这些 v13 独立实验均未覆盖或重命名。

本机验证全部通过：debug 与 release 各 103 harness/687 tests；Scalar AOT debug/release、workspace check、clippy `-D warnings`、bins/examples debug/release build、debug/release doctests、MSRV 1.85 workspace check、Linux x86-64 VM cross-check、`cargo fmt --check`，以及 workspace/AOT runner 的测试入口自检。关键报告为 `target/workspace-validation/bounded-resume-debug-20261118/report.json` 和 `target/workspace-validation/bounded-resume-release-20261118/report.json`；对应日志前缀 `/tmp/wings-bounded-resume-`。Linux 结果是 macOS 主机上的 cross-check，不是原生 Linux 验收。

原生 Linux、CPU 硬配额、监测开销、过载分组、完整公平性分布与同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
