# 2026-11-17 worker 锁区间统计验证

[文档目录](../docs.md) · [ADR-0102](adr/0102-opt-in-worker-lock-statistics.md) · [前轮优化实验](validation-empty-request-optimization-2026-11-16.md) · [机器可读证据](worker-lock-statistics-macos-2026-11-17.json)

## 基线与实现

按用户要求，先将已回退的空请求维护实验及证据提交为 `8b6e77e`，未推送；用户的 `docs/proposals/` 保持未跟踪，不参与本轮修改。

本轮按 M0 的基线 profiler 和最小诊断范围推进 [ADR-0102](adr/0102-opt-in-worker-lock-statistics.md)，不继续无归因地改动调度算法。`SchedulerConfig::lock_statistics` 默认 false，显式启用后固定分配每 worker 四类区间累计：claim/start/snapshot/finish 的次数、获取前至获得锁的墙钟总和/最大值、获得锁至释放前最后读取的墙钟总和/最大值。只发布结束区间，原锁释放后更新统计原子；饱和置标志，不回绕。

存储在分配前纳入原 tenant/VM 控制准入，最后 handle 释放才归还；关闭/join 后仍可查询。默认不额外读时钟或更新统计原子，但仍增加 guard/启用分支及固定控制字段，不声称默认代码零开销。ABI v15、timer、语言请求扫描/双端票据、业务资源上限、预算、quantum 与强制检查点均保持。

## 定向验证

六项新增单测在 CPU 20s/墙钟 30s/core 0 隔离子进程通过：

- 固定聚合与最大值、phase 分离；累计溢出/超长 Duration 转换饱和不回绕。
- 四个宿主线程各写 1000 次并查询，停止后核对 4000 次/纳秒总和。
- 默认关闭 None、非法 worker、持原锁时未发布区间、释放后可重获原锁与可见统计；开启/关闭两种模式清理。
- 受控原锁竞争保持互斥与单次终态，不用耗时阈值判断性能。
- 开启存储增量等于两个固定 observation 的尺寸；关闭后最后 handle 仍可读，tenant/VM 原额度少一拒绝并完整回滚。

真实 debug/release AOT fixture 各独立重复五次，每次覆盖一/双 worker、关闭/开启四种配置：两个 actor 各做八次累加，核对 16 个回复和最终 `[8, 8]`，再对一方注入受管整数溢出，检查失败及旧提交值保留。步骤片仍为 1，业务每组原 3s、CPU 20s/墙钟 30s/core 0 不变。停止/join 后启用组 start/finish 均精确 17，snapshot 非零，非法 worker 拒绝，清理回到 168 字节根基线。总计 40 组真实开关/worker 配置通过；六项单测两种 profile 各重复八次，共 96 次测试执行通过。不替代吞吐配对实验。

开发首次 all-targets 构建发现已有完整 `SchedulerConfig` literal 缺新字段，同时 Rust 1.99 的 `fetch_update` 弃用告警与 MSRV 1.85 兼容要求冲突；已补齐 literal，并使用兼容的 CAS 饱和更新。上述初次构建失败保留在 `/tmp/wings-lock-check-initial.log`，不算验证通过；未放宽编译器或测试限制。

## 全量验证命令

debug/release 各 **89 个独立 harness、594 项测试通过**，两种构建完整受限 AOT 均通过。release 首次完整 AOT 在原 `record_messages` 超过 30s 墙钟，随后同源码/原限制单独重复八次通过，完整重跑通过；原因尚未确定，不能归因于新统计或简单认作主机争用。check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 与依赖/禁 unwind 边界检查通过；workspace debug doc-test 检查通过，各 harness 无实际测试，不额外计数。`--no-run` 只产生清单，测试数来自逐个隔离实际运行；新 fixture 已接入完整 AOT 脚本。

release 真实 Wings/OTP 29/Python 的开环 smoke 6 组计划/10 项配置拒绝、并发失败 27 组场景/96 条枚举历史/24 条伪造历史/18 项桥接与 51 项 JSON 拒绝、fairness 两组精确计划和独立原子观测全部通过。旧 fairness Scheduler 让出仍零样本，不报告零延迟。两种 profile 的十份 fixture 原始输出、实际源文件及 25 份验证日志哈希见机器可读证据；这些短测没有代表负载的持续吞吐结论。

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
target/debug/wings-compile build examples/counter.wings -o target/lock-statistics-counter-20261117
target/debug/examples/scheduler_locks --trusted target/lock-statistics-counter-20261117
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/lock-stats-open-loop-20261117
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/lock-stats-fairness-20261117
```

命令中的 fixture 也须通过 `verify_scalar_aot.command` 在隔离子进程运行，不在宿主无约束执行注入。日志使用 `/tmp/wings-lock-{tests,focused,focused-aot,focused-aot-release,repeat,record-repeat,check,clippy,fmt,msrv,doctests,open-loop,failure,fairness}.log`、`/tmp/wings-lock-aot-{debug,release}.log` 与 `/tmp/wings-lock-aot-release-retry.log`；首次失败不覆盖。

## 解释边界

四类区间只覆盖 worker 指定的 Scheduler 锁位置，不含 park/Condvar 等待、宿主 API、语言 effect、资源账户、结果表或 timer 自身的锁；所有值是墙钟，含 OS 抢占，不是线程 CPU。启用增加每区间三次时钟和固定原子更新，活跃查询不是跨字段一致快照。总和/最大值不构成 p99，不能从小 fixture 的某类较大推断实际业务唯一瓶颈。

本轮不重写原长测协议或发生器，也不宣称吞吐提高/内存降低。观察开销、可靠 CPU 隔离、最新原生 Linux、完整业务值/资源账本及同负载 BEAM 吞吐/整机内存仍未验收，MVP 未完成。

本轮新代码与记录留在工作区，未另行提交/推送，未修改或纳入用户的 `docs/proposals/`。
