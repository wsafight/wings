# 2026-11-16 宿主结果轮询优化验证

[文档目录](../docs.md) · [原始 BEAM 基线](validation-2026-11-16.md) · [ADR-0099](adr/0099-unready-result-poll-fast-path.md)

## 基线与假设

先提交 Bytes 回复和原五对测量，提交为 `f8cab8c`。用户的 `docs/proposals/` 保持未跟踪，不纳入提交或构建输入。优化只针对宿主 `IngressRequest::try_take` 在未就绪轮询时竞争共享结果表的路径，不认定这解释全部尾延迟。

以原冻结 release 执行文件和同一 mixed-4k 产物额外运行 30s 预热/60s 采样，三秒 macOS `sample` 在约第 32 秒采样，保留到 `target/optimization-reply-poll-20261116/baseline.sample.txt`。该次 60000 次采样全部按时回复，p99 0.724ms、p999 12.842ms；采样本身可能扰动运行，不能与未采样轮次直接判优化收益。profile 的主线程主要为 busy-yield，worker 多为 Condvar 等待，不能仅凭栈快照确定锁竞争占比或所有等待的原因。

## 实现与验证

按 ADR-0099 加入固定计费的每槽原子标志和 handle 原始 deadline 副本，期限前未就绪不取表锁，终态/复用/票据仍由原锁保护。原容量、ABI、预算、safepoint 和时限不变。新增四项测试覆盖持表锁轮询立即返回、成功/取消与复用、期限到达保留 publisher 容量、64 槽反向并发发布和逐级少一/精确准入。

check、all-targets clippy `-D warnings`、fmt 与 Rust 1.85.0 all-targets check 通过。debug/release 各 **88 个独立 harness、588 项测试通过**，包括新增四项和既有发布/取消/Drop/Condvar 竞争；workspace debug doc-tests 通过。两种构建完整受限 AOT 均通过；debug 第一次在 `observed_yield` 超过原 10s 墙钟，完整复跑通过，未提高时限，超时原因未确定。独立 harness 保持 CPU 20s/墙钟 30s/core 0，业务阶段保持 3s。

release 真实 Wings/OTP 29/Python 的 v5 开环 smoke（6 组计划、10 项配置拒绝）、v9 并发失败（27 组场景、96 条枚举历史、24 条伪造历史、18 项桥接/51 项 JSON 拒绝）、v7 fairness smoke/独立原子观测均通过。它们是有界正确性前置，不是性能门禁。

```sh
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/opt-poll-open-loop-smoke-20261116
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/opt-poll-fairness-smoke-20261116
```

`--no-run` 只产生测试清单，实际测试数来自成功构建后逐个隔离执行，不把编译通过当作测试执行。日志为 `/tmp/wings-opt-tests.log`、`/tmp/wings-opt-aot-debug.log`（失败）、`/tmp/wings-opt-aot-debug-retry.log` 和 `/tmp/wings-opt-aot-release.log`。

## 前后配对实验

使用与原 v6 相同的 mixed-4k：1000 actor × 4096 字节状态、100 活跃、mailbox 1、pending 256、32 B 逻辑业务记录、两个 worker、1000 次/秒。优化前后共五对，交替顺序，同一时刻只运行一端；每轮 30s 预热/60s 采样。业务编译产物、业务源码、运行协议和额度不变，不改变发生器忙等待规则以免混入另一优化。

原二进制来自已核对的冻结目录；优化后二进制和实际构建源码另行冻结/哈希。沿原 CPU 240s/墙钟 130s/阶段 120s/禁 core、每文件 10 GiB 和观测 RSS 1 GiB 安全停止，逐条重放事件、核对最终 state 与启动基线。保留所有轮次，不以原 BEAM 的历史最好值拼接比较。

配对驱动及其冻结副本位于 `target/optimization-reply-poll-20261116/compare.py` 和 `target/optimization-reply-poll-20261116/paired/frozen/compare.py`，复用现有 v6 采集器，不修改产品命令/协议。输出目录 `target/optimization-reply-poll-20261116/paired/` 保存原始事件、内存、stdout/stderr、环境/输入哈希和每轮 manifest；五对全部完成，712 项冻结输入最终匹配。比对两套冻结的构建/运行源码，唯一代码差异是 `crates/wings-vm/src/ingress_requests.rs`，业务产物与采集器一致。

### 五对结果

每端采样合计 300000 次、预热合计 150000 次，全部按时回复；零超时、队满或 pending 满。十轮逐条独立重放、最终状态、清空 pending 和 168 字节启动基线均通过。单位为 ms：

| 配对 | 原版 p99 | 快路径 p99 | 原版 p999 | 快路径 p999 |
|---|---:|---:|---:|---:|
| 1 | 6.964 | 8.686 | 19.019 | 17.031 |
| 2 | 0.116 | 0.098 | 12.048 | 3.569 |
| 3 | 11.721 | 8.038 | 39.871 | 15.143 |
| 4 | 0.605 | 0.073 | 10.102 | 0.630 |
| 5 | 0.532 | 0.043 | 20.172 | 0.072 |

快路径 p99 在 **4/5 对**较低，p999 在 **5/5 对**较低；第一对 p99 变差仍保留。五轮 p99 的中位数为原版 0.605ms、快路径 0.098ms；五轮 p999 的中位数为 19.019ms、3.569ms。这是每轮分位数的中位数，不是合并样本分位，也不将两中位数比值当作已证明的加速倍数。不同轮次发生器迟发也有变化，不能把全部差异归为锁优化。

### 内存与结论

原版进程树 RSS 采样峰值范围 55.14–77.06 MiB、physical footprint 47.19–50.84 MiB；快路径为 66.38–77.03 MiB、48.36–51.99 MiB。包含持续存活的 Python driver，不是 VM 独占内存或真实瞬时峰值，**没有内存下降证据**。快路径新增原子数组及控制元数据从原额度支付，caller handle 新增原 deadline 副本，不靠降低容量取得结果。

内存共 888 份样本，885 完整；candidate 第 2/3 轮、baseline 第 5 轮各有一份约第 90 秒采样读子进程失败（errno 3）。原记录保留完整 PID/身份/census、错误和 null 总数，未补零/插值，也不因这些轮次不完整而剔除其业务结果。无主机读取错误或已观测身份变化。峰值来自完整样本，不能据此断言缺失瞬间没有更高内存。

完整精度、输入/原始文件哈希、每轮延迟/迟发、分口径内存、PID 分项与三份缺失见 [JSON 摘要](optimization-result-poll-macos-2026-11-16.json)。源码变化局限于结果表；确定性测试证明免锁路径，五对结果提供初步改善信号，但不能推出稳定全场景收益。优化代码暂留工作区，未另行提交或推送。

本机无 CPU 硬配额或已验证隔离，尚无最新原生 Linux；试测不等于最大持续吞吐、完整整机内存或 BEAM 双目标验收。若延迟改善不稳定或无收益，应如实保留，不将消除某个锁路径自动换算为加速倍数。
