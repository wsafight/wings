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

## 当前实测状态（双平台配对基准与 `cgroup v2` 硬配额）

`.github/workflows/benchmark.yml` 在 **Linux x86-64 `cgroup v2` 硬 CPU 配额（`cpu.max = 20000 10000`、`cpu.max.burst = 0`，3s 预热 / 5s 采样 × 3 对交替重复）** 与 **macOS AArch64（`macos-15`，5s 预热 / 5s 采样 × 3 对交替重复）** 上自动化对比 Wings 与 Erlang/OTP 28 的四组工作负载：

- **Linux x86-64（`cgroup v2` 2.0-vCPU 硬配额，OTP 28）**：
  - **开环吞吐与服务延迟**：`hot-queue`（`10,000` 准时 RPS，服务 p99 中位数约 `16.2 µs` vs BEAM `5.1 µs`，峰值 PSS 约 `41.0 MB` vs `70.2 MB`）、`sparse-256`（10,000 Actor，`1,000` RPS，服务 p99 约 `22.8 µs`，PSS 约 `61.7 MB` vs `98.5 MB`）与 `mixed-4k`（1,000 Actor，`1,000` RPS，服务 p99 约 `24.6 µs`，PSS 约 `33.4 MB` vs `66.9 MB`）均以 3/3 零拒绝、零超时通过。
  - **Fairness 抢占公平性**：3/3 轮零拒绝、零超时通过，百万次加法计算完成率高于 BEAM（约 `102.2/s` vs `88.2/s`），心跳 p99 持平（约 `445 µs` vs `444 µs`），峰值 PSS 降低约 `2.8×`（约 `22.3 MB` vs `63.1 MB`）。
- **macOS AArch64（`macos-15` 虚拟 M1 与本地 Apple Silicon）**：
  - 四组配对套件全部通过，物理内存占用均显著低于同机 BEAM：`hot-queue`（约 `45.0 MB` vs `70.7 MB`，服务 p99 约 `130 µs` vs `226 µs`）、`sparse-256`（约 `64.0–64.7 MB` vs `96.5–98.0 MB`）、`mixed-4k`（约 `36.1–37.3 MB` vs `65.3–65.8 MB`）与 `fairness`（约 `22.9–23.1 MB` vs `59.1–59.6 MB`）；本机 Apple Silicon 30s/60s 五对长测同样 5/5 通过（心跳 p99 中位数约 `645 µs`）。
- **保留边界说明**：
  - 尽管 Linux `hot-queue` 已稳定通过 `< 100 µs` 服务延迟门禁，单收件人极限争用下的微秒级服务 p99（约 `16 µs`）绝对值仍约为 BEAM 进程内归约循环（约 `5 µs`）的 3 倍。
  - 手动基准 CI 采用有界 3–5s 窗口（`WINGS_BENCH_WINDOW`）适配云端共享 runner；日常 `ci.yml` 仅执行增量正确性检查。`.github/workflows/benchmark.yml` 仅手动触发，独占物理机长稳验收仍使用默认 30s 预热 + 60s 采样 × 5 对脚本。
  - `WingsAllocator` 与 `native_memory_ledger` 已覆盖全局 Rust 堆请求/可用/碎片字节及 OS PSS/physical footprint 对账，Rust 分配器之外的系统动态装载器（`dyld`/`ld.so`）内部元数据按整进程物理内存口径观测。

## 优化实验纪律

任何面向吞吐或内存的激进优化（如超块检查合并、协程帧复用、无锁邮箱批处理、对象池化、紧凑布局或未来的分代/移动式 GC）必须遵循：

1. **不牺牲安全边界换取跑分**：不得移除强制 safepoint、栈界检查或三级资源准入来刷高吞吐数字。
2. **先归因再改动**：可选 `--observe-locks` 会报告调度器互斥锁各阶段的等待/持有时间、worker 线程 CPU，以及 FastSlot/邮箱自旋竞争（竞争获取次数、失败尝试、让出次数和等待时间）；观测时仍保留直接执行与批处理快路径。观测结果仅用于归因，不作为性能验收结论。
3. **配对 A/B 验收与果断回退**：单项优化在同业务五对交替长测下对比吞吐、p99/p999、拒绝数与分口径内存（Linux RSS/PSS、macOS RSS/physical footprint）；无稳定收益或引发尾延迟退化时直接回退，不保留无效复杂度。

## 日常增量验证

`python3 scripts/verify_incremental.py` 根据暂存、未暂存、删除、重命名和未跟踪文件选择检查；`--base <ref>` 同时包含相对 merge-base 已提交的改动，基准引用无效时直接失败，不静默漏检。`--plan` 输出任务图，`--all` 选择全部增量检查，`--no-cache` 强制重新执行；这些选项均不会运行发布或性能验收。

- Rust 改动通过 Cargo metadata 选择所属 crate 及其 workspace 传递依赖方，执行格式、依赖隔离、全 target 定向 Clippy，以及沿用 CPU/墙钟/core 限制的库/二进制/集成测试 harness。改动的示例 target 也执行其 harness，其余示例仅编译检查，全 target 执行保留在发布验证；选中 harness 内不再过滤单个测试。
- 影响编译或执行的改动增加有界计数器/会话 AOT 冒烟，覆盖 TCP/HTTP 服务与故障恢复。共享构建步骤只执行一次，复用 Cargo 增量产物；完整 AOT 套件保留在发布验证中。
- 脚本与 workflow 改动运行 Python 回归，文档与网站改动构建网站；未知路径保守扩大到全部增量检查。任务串行执行，本地文件锁防止调度器重叠运行。
- 本地成功证据按相关文件内容、删除、权限、依赖、执行脚本、锁文件、工具版本、平台、Cargo 配置和环境摘要生成指纹；失败/中断不缓存，执行中输入变化则本轮不可采信。缓存与报告放在已忽略的 `target/incremental-validation/`，仅供本地提速，不是可信发布凭证。
- 日常 PR 和 `main` 推送在 Linux 上以 `--no-cache` 运行同一调度器，`incremental` 检查替代日常全量 native/MSRV/site 任务，Pages 部署保持独立。发布前手动运行 `.github/workflows/release.yml` 的双平台、MSRV、doctest 与完整 AOT 检查，BEAM 配对另由 `benchmark.yml` 手动触发；增量通过不代表发布或性能验收通过。

## 发布前验证命令

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
