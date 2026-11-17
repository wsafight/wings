# 调用参数分配诊断与候选回退（2026-11-18）

## 原实现与失败探针

契约和回退理由见 [ADR-0119](adr/0119-inline-small-invocation-arguments.md)。本机 Apple M3 Pro、36 GiB、macOS 26.6.2 AArch64、Rust 1.99.1，基线 `3d4d444`。独立 System allocator 包装器只计 Rust 请求的分配次数，不计 allocator 元数据、系统内部 malloc、物理占用或 RSS；所有故障均在原 CPU 20s/墙钟 30s/core 0 子进程内运行。

原实现的首次诊断在同一显式 process 计费池、预先创建的 token 下运行 0/1/64 参数各 16 次真实 AOT 调用，每次启动后修改调用方数组，再以 `resume(1)` 运行到结束。分别观测 0/16/16 次分配；回复、累计 state 和清理通过，但原“全部零分配”假设断言失败退出 101。日志 `/tmp/wings-invocation-arguments-baseline-20261118.log` 保留，不伪称旧实现没有分配。

## 两种候选及边界

直接捕获固定 64 槽会超过 corosensei 的 1024 B 闭包传递上界，实际 panic `type is too big to transfer`，未进入正式实现。直接探针日志 `/tmp/wings-invocation-arguments-candidate-20261118.log`；误启动的长测基线完成，候选在初始化退出 101，整个实验无效，原始文件在 `target/mvp-invocation-arguments-20261118/pair-1/`、日志 `/tmp/wings-invocation-arguments-pair1-20261118.log`。不放宽依赖的保护边界、不将 panic 说成受管业务错误，也不将这轮当作业务回归通过。

小参数候选使用至多八槽的 enum，9..=64 沿原 Vec。在独立 release 主程序的 0/1/8/9/64 参数各 16 次中，分配分别为 0/0/0/16/16，全部挂起/原参数/结果/状态/账本检查通过，见 `/tmp/wings-small-invocation-arguments-candidate-r2-20261118.log`。更早的探针仍要求所有参数零分配，因此在 64 参数的原 fallback 处失败，保留 `/tmp/wings-small-invocation-arguments-candidate-20261118.log`；后续明确小参数边界，不把大参数仍分配藏掉。

候选没有改变 safepoint、预算、取消、GC、计时器、迁移或资源容量，但增大闭包固定尺寸。消除一次分配不自动等于端到端更快或内存更少；正式实现已撤回候选，保留原 Vec 与原 4352 B 控制预留。

## 冻结长测条件

冻结两份 release runner、完整源码、候选补丁、编译工具、相同 AOT 产物和驱动，每次运行前后核对全部输入 SHA-256；目录 `target/mvp-small-invocation-arguments-20261118/`。沿原 v6 hot-queue：100 actor × 4096 B、90% 请求到同一 actor、mailbox 100、pending 256、两个 worker、80000 次/秒，30s 预热/60s 采样、40 B 原始事件、CPU 240s、墙钟 130s、阶段 120s、文件 10 GiB 和 RSS 安全停止 1 GiB 不变。

第一对基线先跑，第二对候选先跑；所有到达/结果独立逐条重放、核对最终 state、pending 清空和根启动回执。运行期间无本项目构建/测试或 profiler，其他主机活动未隔离。缺少 CPU 硬配额、温度/电源控制、同轮 BEAM、完整平台内存及监测偏差、原生双平台；RSS 安全观察不是内存门禁。两对筛选不冒充五对性能验收，所有拒绝/超时和发生器迟发保留。

每轮采样 4800000 次计划到达，单位 ms：

| 对次/顺序 | 版本 | 按时回复 | 队满 | pending 满 | 超时 | p99 | p999 | 迟发 p999 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| 1/先 | 原 Vec | 4674700 | 125227 | 0 | 73 | 2.786334 | 24.306042 | 52.937417 |
| 1/后 | 八槽 | 4172186 | 566990 | 50248 | 10576 | 31.133458 | 147.823708 | 172.049125 |
| 2/先 | 八槽 | 4747337 | 51885 | 778 | 0 | 0.849333 | 10.475875 | 11.326750 |
| 2/后 | 原 Vec | 4752515 | 47315 | 0 | 170 | 0.454958 | 15.570916 | 53.411291 |

第一对候选超时、拒绝和尾延迟均更差；第二对候选超时/p999 更好，但拒绝/p99 更差。发生器迟发和轮间波动明显，不能确认所有差异由存储布局导致，也没有稳定端到端收益支持保留候选。原计划最多五对，在两对筛选后停止，不删除不利轮次或用第二对的低超时抵消第一对 10576 次超时。

每轮预热计划 2400000 次：

| 对次/顺序 | 版本 | 按时回复 | 队满 | pending 满 | 超时 |
|---|---|---:|---:|---:|---:|
| 1/先 | 原 Vec | 2169416 | 230312 | 226 | 46 |
| 1/后 | 八槽 | 952216 | 1426722 | 19170 | 1892 |
| 2/先 | 八槽 | 2386386 | 13614 | 0 | 0 |
| 2/后 | 原 Vec | 2376123 | 23495 | 382 | 0 |

原始目录中有 `environment.json`、两份源码/runner、`candidate.patch`、`compare.py`，以及各对的事件、控制输出、错误、RSS 安全观察和逐条重放结果。日志 `/tmp/wings-small-invocation-arguments-pair1-20261118.log`、`/tmp/wings-small-invocation-arguments-pair2-20261118.log`；四轮全部独立重放通过。

| 轮次 | 事件 SHA-256 |
|---|---|
| 1 原 Vec | `9a3bd2b9c51c3e26a94726eae97e4f72a1e067c515f887e4dd8efd79880831ee` |
| 1 八槽 | `4400a0069fa49d4335ac51ebab4ac3d0056ec56a5b4f7e3d6f921885d9d0d0eb` |
| 2 八槽 | `da80a641370165f78fd6833e3573f135219bfc37e7c5f985f69248cd268b6c30` |
| 2 原 Vec | `f43292d5fc0611a0909f4d460fdffdff64b59d3b2bb02bda098c4f170e2cd202` |

## 保留的诊断

`invocation_arguments` main 接入原完整 AOT：0/1/8/9/64 各 16 次，验证独立参数、真实挂起、正确累计结果、非空每次一次分配/零参数不分配、65 参数拒绝和失败后无活跃任务、逐轮相同 used 及最终启动基线。它是当前行为回归，不是已消除分配或证明吞吐收益；example 的零项 test harness 不代替 main 实跑。

## 最终回归

以下命令均通过，执行核心 `execution.rs` 与基线没有差异：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo build --workspace --bins --examples --locked
cargo build --workspace --bins --examples --release --locked
python3 scripts/verify_workspace.py --output target/workspace-validation/argument-probe-debug-20261118
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/argument-probe-release-20261118
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
python3 scripts/verify_workspace_tests.py
python3 scripts/verify_scalar_aot_tests.py
```

debug/release 全目标各 99 个 harness、678 项通过，报告为上述两个 workspace 目录的 `report.json`；VM 库仍为 325 项、CLI 二进制仍为 27 项。新增 example harness 本身零项，main 已在两种完整 AOT 中实际执行，各有 80 次成功调用、65 参数拒绝及最后清理。两种完整 AOT 均通过，日志 `/tmp/wings-argument-probe-aot-debug-20261118.log` 和 `/tmp/wings-argument-probe-aot-release-20261118.log`；workspace/AOT 运行器自测分别 8/6 项通过。相对链接、代码围栏和 `git diff --check` 通过。Linux 仅交叉检查，没有原生执行；候选的局部探针和负载不等于候选完成全部功能门禁。

完整资源账本、公平/过载/长时故障、最新原生双平台及同负载 BEAM 吞吐/整机内存门禁仍未通过，MVP 未完成。
