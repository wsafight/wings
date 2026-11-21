# 原生同步后备存储计费验证（2026-11-18）

## 条件与缺口

本机 macOS 26.6.2 AArch64、Apple M3 Pro、36 GiB，Rust 1.99.1；基于 `e164992`，契约见 [ADR-0118](adr/0118-native-sync-storage-admission.md)。独立 `sync_accounting` 示例用 System allocator 包装器统计请求存储与分配次数，不统计 allocator 元数据、RSS 或 OS 内核成本。受原 AOT 子进程 CPU 20 秒、墙钟 30 秒和禁 core 限制。

修复前实际失败：标准 Mutex 首次使用新增 64 B，Condvar 新增 48 B；计费根创建存活 168 B，首次 `used` 后存活 232 B，账面仍 168 B，新增一次分配。示例以 `VM bootstrap misses native storage` 退出 101。保留 `/tmp/wings-sync-accounting-baseline-20261118.log`，不以修复后成功覆盖。

## 修正与检查

根成本增加平台后备字节，在准入之后、共享发布之前初始化。退出票据原 64 B 控制余量本机增至 80 B，以同时覆盖 Arc 头和 mutex；其他直接同步对象沿足够的原固定预留，timer/observer 显式核对其控制成本。原额度、4352 B、ABI v21、manifest v2、队列容量和各时限不变。

最初修复探针实测：根创建存活、账面和首次查询后存活均为 232 B，首次查询新增分配为零，释放回到原基线。日志 `/tmp/wings-sync-accounting-root-20261118.log`。继续验证精确满额根的 1024 次零资源准入/查询/释放不分配、少 1 B 启动拒绝与多 1 B 预留拒绝，以及退出票据的 tenant/VM 精确边界、公布终态和末持有者清理。完整最终验证结果另记于下。

第一轮 debug 全量 98 个 harness、678 项通过。随后 AOT 在旧锁统计检查写死 `bootstrap_bytes == remaining_bytes == 168` 处失败，日志保留 `/tmp/wings-sync-aot-debug-20261118.log`。将该处接到已有的严格正整数/精确清理回执校验，根存储精确大小由独立分配探针验证；不改历史模型中的合法 168 B 合成回执。脚本修正后的独立运行另存新日志。

## 最终本机结果

以下命令均通过；执行代码和运行器修正后未改变 Rust 源码：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo build --workspace --bins --examples --locked
cargo build --workspace --bins --examples --release --locked
python3 scripts/verify_workspace.py --output target/workspace-validation/native-sync-debug-20261118
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/native-sync-release-20261118
cargo test --workspace --doc --locked
cargo test --workspace --doc --release --locked
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
CARGO_TARGET_DIR=target/native-sync-msrv-20261118 cargo +1.85.0 build --locked -p wings-vm --example sync_accounting
python3 scripts/verify_workspace_tests.py
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_resource_receipt.py
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam
```

debug/release 全目标各 98 个 harness、678 项通过，其中 VM 库 325 项、CLI 二进制 27 项；doctest 通过。完整 debug/release AOT 均通过，分别保留 `/tmp/wings-sync-aot-debug-r2-20261118.log` 和 `/tmp/wings-sync-aot-release-20261118.log`。workspace/AOT 运行器自测分别 8/6 项通过，回执校验拒绝 37 组伪造数据。受限 `sync_accounting` main 在 Rust 1.99.1 的 debug/release 及独立 Rust 1.85 构建中实际执行，三者均测得 mutex 64 B、condvar 48 B、根 232 B、首次查询零分配和满额根 1024 次操作零分配，最终均释放；example 的零项 test harness 不替代这些 main 实跑。

真实 release Wings/OTP 29/Python 开环六组与十组配置拒绝通过；并发失败验证 27 组、96 个枚举历史、24 个伪造历史及 18/51 个协议拒绝通过。保留过载差异：hot-queue 的 1 ms、1280 次计划到达中，Wings 回复 337/队满 943，BEAM 回复 1280/队满 0；dense-64k 的 10 ms、10000 次中，Wings 回复 1727/队满 8273，BEAM 回复 7201/队满 2799。它们是短时计划开环，含发生器迟发，不能当作持续吞吐排名或本次计费修正的性能变化。

两组百万加法/同刻心跳短测均完整返回，无超时或拒绝；Scheduler 仍为零条原子让出样本，不能报告零延迟或称公平性门禁通过。日志分别为 `/tmp/wings-sync-open-loop-beam-20261118.log`、`/tmp/wings-sync-concurrent-failure-beam-20261118.log` 和 `/tmp/wings-sync-fairness-beam-20261118.log`。文档相对链接、围栏与 `git diff --check` 通过。

## 边界

Linux 的内联 futex 无额外后备计费，但交叉检查不能代替最新原生 Linux。标准库 channel/thread 的内部实现仍由原控制余量约束，本切片不宣称完整分配/装载账本闭环。前轮 release 全 AOT 的 native probe 首次启动 2 秒超时仍保留，见 [CLI 记录输出验证](validation-cli-record-replies-2026-11-18.md)；后续新源码验证不能消除该失败。长时公平/过载/故障、原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双门禁仍待完成，MVP 未完成。
