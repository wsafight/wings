# 2026-11-16 验证记录

[文档目录](../docs.md) · [评测与验收](evaluation.md) · [前次记录](validation-2026-11-15.md)

## Bytes 回复：ADR-0098

实现 [ADR-0098](adr/0098-accounted-byte-replies.md)，保持 ABI v15、4352 字节执行控制预留、三级额度及原请求生命周期。可运行源码为 [`examples/bytes-replies.wings`](../examples/bytes-replies.wings)，驱动为 [`bytes_replies.rs`](../crates/wings-vm/examples/bytes_replies.rs)。结果分块复制后独立计费，完成信封持不可变存储租约，在发起方另计费导入；不直接暴露业务堆编号。

## 验证状态

当前环境为原生 macOS AArch64、Rust 1.99.1、MSRV Rust 1.85.0 和 OTP 29/ERTS 17.1.1。未列出的命令不视为通过；Linux 与双平台门禁见末节。

- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、`cargo +1.85.0 check --workspace --all-targets --locked` 通过。
- debug/release 分别构建全部测试后，在每项 CPU 20s/墙钟 30s/core 0 的独立子进程执行，各 **88 个 harness、584 项测试通过**。debug 最终复跑覆盖扩展后的满队列/发起方退出回归；workspace debug doc-tests 通过。
- debug/release 完整 `scripts/verify_scalar_aot.py` 均通过，含 Bytes CLI、真实 AOT 驱动和既有栈、GC、消息、请求、监督、OS 唤醒、容量压力及拒绝回归。debug 最终在长测结束后原限制复跑通过，先前两次失败仍保留。
- Bytes 真实 AOT 驱动另执行八次受限重复，共 **576 组语言场景**：长度 0/1/65536 × 成功/零超时/取消/目标失败 × 自己/子进程 × 单 worker/Scheduler1/Scheduler2；另覆盖同步、分片恢复、宿主类型化消息、固定 ingress、监督结果保留、目标停止后结果和发起方导入失败。每阶段三秒，驱动经 CPU 20s/墙钟 30s/core 0 子进程执行。
- 结果对象覆盖长度 0/1/255/256/257/65536、三级存储/控制拒绝、共享结果末拥有者释放、出口精确 514 步预算、暂停/取消/Drop 与候选状态不提交。worker/Scheduler 的六种回复交叉矩阵、完成队满保留路由和发起方退出清理通过。
- 依赖边界、协程禁用 unwind feature、Markdown 相对文件链接/围栏、Python AST 和 diff whitespace 检查通过。

可复用构建与验证命令：

```sh
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
cargo build --workspace --locked --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

`--no-run` 只生成 harness 清单，不代表测试已执行；上述测试数来自成功构建后逐个隔离执行。未提高原资源限额、CPU/墙钟上限或三秒业务阶段。

## BEAM 对照与测量前置

以下使用 release 驱动和真实 OTP，不回退到模型；正确性仍由独立 Python 模型核对。

| 验证 | 本次结果 | 证据边界 |
|---|---|---|
| v5 计划开环 smoke | 6 组计划、10 组配置拒绝通过 | 1–100ms 短测，不是持续吞吐 |
| v9 双发送者并发失败 | 27 组场景、96 条枚举历史、24 条伪造历史、18 项桥接/51 项 JSON 拒绝通过 | 有界并发正确性，不是完整故障验收 |
| v7 CPU/突发心跳 smoke | 10ms/30ms 两组精确计划通过，无拒绝/超时 | 让出观测为零样本，不报告零延迟 |
| v6 长测采集器 | 原生内存 probe、90000 次合成到达、精确分位、失败与配置拒绝通过 | 此命令本身不运行长测 |
| v8 fairness 采集器 | 模型、20 条伪造历史、严格 CLI 与采集器通过 | 本次没有执行 v8 五对长测 |

```sh
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/mvp-open-loop-smoke-20261116
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/mvp-fairness-smoke-20261116
WINGS_PROFILE=release python3 scripts/verify_measurement.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness_measurement.py --compare-beam
```

四组基础短测均全部回复。两组过载仍保留差异：hot-queue 在 1ms 内计划 1280 次，Wings 回复 537/队满 743，BEAM 回复 1280/队满 0；dense-64k 在 10ms 内计划 10000 次，Wings 回复 1946/队满 8054，BEAM 回复 6640/队满 3360。这些数字仅描述本次短时准入和完成，不用作吞吐排名。

## 内存与持续吞吐试测

从 `mixed-4k` 固定 **1000 actor × 4096 字节状态、100 活跃 actor、mailbox 1、全局 pending 256、32 字节逻辑业务记录** 开始，Wings 使用两个 worker，BEAM 使用两个 scheduler；信封成本另计。固定 1000 次/秒，每轮预热 30 秒、采样 60 秒，五对交替顺序；不是最大持续吞吐扫描。

```sh
target/release/wings-compile build examples/beam-multi-counter.wings -o target/mvp-mixed4k-artifact-20261116
WINGS_PROFILE=release python3 scripts/measure_open_loop.py --case mixed-4k --artifact target/mvp-mixed4k-artifact-20261116 --output target/mvp-mixed4k-measure-20261116
```

编译在 CPU 20s/墙钟 30s/core 0 子进程执行。长测沿原 CPU 240s/墙钟 130s/阶段 120s、每文件 10 GiB 与观测 RSS 1 GiB 安全停止线；RSS 停止线不是 OS 内存配额。冻结目录包含实际构建使用的未提交源码、工具/产物及哈希，排除未采纳语法提案。原始事件、逐秒内存、stdout/stderr、环境及每轮结果保留在输出目录，五对结果汇总到 `report.json`。

### 实际结果

Apple M3 Pro、macOS AArch64 上五对均完成，十轮逐条重放、最终状态、pending 清空与 **358 项冻结输入哈希**复核通过。参与构建/运行的 228 项当前源码与冻结副本一致，后补验证文档不属于原冻结版本。每端采样合计 300000 次、每轮按时回复 60000 次，即 **1000 次/秒，零超时、零队满、零 pending 满**；预热每轮 30000 次也全部回复。Wings 每轮释放后回到 168 字节启动基线，不把它解释为 RSS 已回收。

下表延迟由计划到达算至真实结果观察，包含迟发；内存是各轮整个运行期间的逐秒 **Python 驱动加捕获后代**完整采样峰值，不是 VM 独占内存或真实瞬时峰值。单位分别为 ms 和 MiB（1048576 字节）。

| 配对 | Wings p99 / p999 | BEAM p99 / p999 | Wings RSS / footprint | BEAM RSS / footprint |
|---|---|---|---|---|
| 1 | 16.890 / 27.117 | 0.016 / 0.084 | 53.73 / 44.52 | 86.08 / 67.63 |
| 2 | 0.048 / 0.117 | 0.028 / 0.458 | 64.05 / 44.81 | 88.72 / 66.75 |
| 3 | 16.907 / 27.085 | 0.022 / 0.109 | 67.98 / 46.94 | 97.38 / 66.89 |
| 4 | 11.351 / 20.861 | 0.033 / 2.506 | 68.77 / 43.80 | 97.39 / 66.53 |
| 5 | 0.079 / 12.184 | 0.019 / 0.090 | 70.91 / 45.42 | 105.80 / 69.77 |

两端各 5/5 满足既定尾延迟候选 p99≤20ms、p999≤50ms，但 **Wings 每轮 p99 都高于 BEAM**，多轮差距明显；候选通过不能解读为更快。本次固定速率没有找到饱和点，两端完成率相同，不能推导吞吐上限。内存采样口径下 Wings 较低，但仍不足以宣布整机内存门禁通过。各轮 driver 也占用内存，保留的 PID 分项显示 driver RSS 随轮次改变，不能把进程树 RSS 增长直接归为 VM 泄漏。

892 份内存样本全部完整（Wings 445、BEAM 447），无身份变化/主机读取错误；其中 initializing 10、warmup 291、sampling 591。C probe 自报合计 user CPU 0.691s/system CPU 0.902s、单次最大 wall 23.531ms、max RSS 1.344 MiB；这不含完整 `ps` census 成本，监测偏差仍未验收。主机 swap 约 1904.6 MiB、压缩 resident 约 10916.7–11553.7 MiB，仅反映同期主机状态，不归因给 VM，也不与 footprint 相加。

完整精度、环境/限制、十轮数据、每 PID 分项与原始文件哈希保存在 [JSON 摘要](open-loop-mixed-macos-2026-11-16.json)。原始目录为 `target/mvp-mixed4k-measure-20261116/`，须保留其冻结副本、事件和内存文件才能重放。运行期间没有有意并行启动本项目构建/测试，但存在其他任务，CPU 硬配额、主机隔离与电源/热状态未验证。本次只作为探索性基线，不宣称 Wings 优于 BEAM、最大持续吞吐已证明或内存门禁通过；其余三组最新源码长测、fairness、过载与原生 Linux 仍待验收。

## 失败记录

开发中修正新增结果变体的穷尽匹配、协程返回元组和错误类型、IR/Scheduler 遗漏的 Bytes 回复准入及旧拒绝测试。测试 fixture 曾将最大字节生成/出口总预算少算一个步骤、占满子账户时未区别父路径余量；修正后精确检查 514 步及逐级拒绝。导入失败 fixture 曾让一槽目标先分配第二个字节对象，实际测到目标失败；改为返回既有初始值后才验证发起方导入失败。无效试验不计入通过证据。

- 第一次 debug 完整 AOT 在 Flag CLI 超过原 30s 墙钟；该 fixture 随后单独重复八次通过，原失败原因未确定。
- 第二次 debug 完整 AOT 在 TCP 服务启动超过原 5s 就绪期限，未修改时限绕过。长测完成后第三次 debug 完整 AOT 在原限制内全部通过；两次历史失败原因仍未确定，不隐去失败。
- 原生内存 probe 前两次校验在首次启动新编译 C helper 时超过原 2s，随后独立 probe 及完整 v6 采集器校验通过；保留失败，不确定归因为主机争用。

## 剩余 MVP 门禁

| 门禁 | 当前状态 |
|---|---|
| Bytes 回复 | 本切片补齐，验证范围见本记录 |
| 字符串/通用记录等业务值 | 未完成；Bytes 不冒充字符串 |
| 完整启动、装载和全部分配资源账本 | 未完成；逻辑预留不等于 allocator/装载器及 OS 实际占用 |
| 长时公平/过载/并发故障与恢复诊断 | 仍需独立验收，既有局部正确性不替代全门禁 |
| 最新源码原生 macOS AArch64/Linux x86-64 与 MSRV | 逐项列证据；当前无可用原生 Linux x86-64 主机 |
| 同平台、同负载 BEAM 持续吞吐与整机内存 | 尚未验收；探索性结果不等于最大持续吞吐或更省内存 |

本机没有已配置的 Lima 实例，且 Apple Silicon 上的 Linux x86-64 仿真不能补原生 Linux 验收。未推送分支、触发远程 CI、合并或发布。MVP 保持未完成，不能通过删除门禁或提高资源/时间上限宣布通过。
