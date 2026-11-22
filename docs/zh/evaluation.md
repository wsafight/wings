# 评测与验收

[English](../evaluation.md) | **简体中文** · [返回概览](../../README.zh-CN.md) · [产品定位](product.md) · [运行时](runtime.md) · [风险与里程碑](roadmap.md)

Wings 的核心目标是**在相同业务负载下同时取得高于 BEAM 的持续吞吐与低于 BEAM 的整机内存占用**。功能正确不等于性能达标；所有基准结论必须附负载条件、平台环境与尾延迟边界。

## 对照负载与正确性基线

仓库在 `scripts/` 与 `crates/wings-vm/examples/` 中维护与 Erlang/OTP 同语义的可执行对照负载：

1. **状态与消息等价验证**：覆盖单 Actor / 多 Actor 计数器、双发送者合法交错、64 KiB `bytes` TCP 分片/流水线服务（`verify_tcp_bytes.py`），以及并发请求取消、除零故障与独立子进程监督重启等价性（`verify_recovery_soak.py`、`verify_failure_soak.py`）。
2. **计划开环吞吐与服务延迟（`measure_open_loop.py`）**：
   - 采用固定计划时刻发生器（避免闭环客户端掩盖排队延迟），同时报告端到端延迟（`observed - planned`）、真实服务延迟（`observed - sent`）与发生器迟发分布，记录 `mailbox_full` 拒绝、超时及进程树内存采样。
   - 代表场景包含 `mixed`（稀疏活跃会话）与 `hot-queue`（热点收件人高争用队列），每组执行 30s 预热 + 60s 采样 × 5 对独立交替重复。
3. **抢占公平性与心跳尾延迟（`measure_fairness.py`）**：在重度计算 Actor 持续施压的同时注入突发轻量心跳请求，验证受管抢占下的心跳 p99 / p999 尾延迟、零超时与资源完整回收。
4. **受限故障注入**：在 `RLIMIT_CPU` / 墙钟 / 禁 core dump 约束的独立子进程中执行真实系统分配器 `null` 注入（`verify_allocation_failures.py`）与无限循环原子让出进展回归（`verify_atomic_progress.py`）。

## 当前实测状态（macOS AArch64）

基于近期五对配对长测（30s 预热 / 60s 采样，Wings vs OTP 29）：

- **Fairness 抢占与混合负载**：Wings 与 BEAM 各 5/5 轮通过尾延迟候选且零拒绝、零超时；Wings 热计算完成率与同轮进程树 RSS / 物理占用采样呈探索性优势，心跳 p99 中位数约 `645 µs`（BEAM 约 `441 µs`），p999 中位数低于 BEAM。
- **Hot-queue 开环服务延迟**：Wings 5/5 轮零拒绝通过尾候选，但热点队列单点争用下的 `observed - sent` 服务 p99（约 `35.9–46.2 µs`）仍高于 BEAM（约 `5.3–10.0 µs`）。
- **已验证并回退的候选**：曾实验“无等待者省略结果完成通知”与“Scheduler finish 仅新增 ready 时广播”两项通知裁剪候选，配对长测中出现了 `mailbox_full` 增加或尾候选不稳，均已按实验纪律回退，保留确定性唤醒路径。
- **门禁判定**：**正式吞吐与整机内存双门禁尚未通过**（仍需优化热点队列尾延迟，并在原生 Linux cgroup v2 硬 CPU 配额与双平台同机环境下完成最终验收）。

## 优化实验纪律

任何面向吞吐或内存的激进优化（如超块检查合并、协程帧复用、无锁邮箱批处理、对象池化、紧凑布局或未来的分代/移动式 GC）必须遵循：

1. **不牺牲安全边界换取跑分**：不得移除强制 safepoint、栈界检查或三级资源准入来刷高吞吐数字。
2. **先归因再改动**：利用内置可选锁统计（`--observe-locks`）与线程 CPU 区间观测定位真实瓶颈，区分发生器 OS 调度迟发与 VM 内部背压。
3. **配对 A/B 验收与果断回退**：单项优化在同业务五对交替长测下对比吞吐、p99/p999、拒绝数与分口径内存（Linux RSS/PSS、macOS RSS/physical footprint）；无稳定收益或引发尾延迟退化时直接回退，不保留无效复杂度。

## 常规验证命令

```sh
# 1. 静态检查与全工作区单元测试
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# 2. OS 限额下的全测试目标运行器
python3 scripts/verify_workspace.py --output target/workspace-validation/debug

# 3. 完整端到端原生 AOT 回归（覆盖 Debug / Release）
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_scalar_aot.py

cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py
```
