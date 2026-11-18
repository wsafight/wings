# 2026-11-18 worker CPU 区间验证

对应 [ADR-0134](adr/0134-opt-in-worker-cpu-observation.md)，延续[同业务锁诊断](validation-open-loop-locks-2026-11-18.md)。只在已有可选锁观测启用时，worker 函数入口/退出读取两次本线程 CPU 时钟；新固定原子存储自动纳入原统计区先准入，默认不分配、不读 CPU 时钟。ABI v22、4352 B、业务、期限及资源上限保持。

## 验证范围

原生读取、非法/溢出/倒退/失败与合法零区间分别验证；Pending 不作为完成值，跨线程查询只在单原子终态发布后获得结果。guard 保持原线程，观察区随最后 handle 退账。一/双 worker 真实 AOT 保留正常状态、失败、取消及清理核对。工程、原限完整测试、MSRV 和交叉检查的实际结果另记，交叉检查不算原生 Linux。

## 已执行检查

本机 macOS 26.6.2 AArch64、Rust 1.99.1，顺序执行：

- CPU 三项、原锁六项 Rust 定向测试及九项 Python 模型通过；覆盖时钟状态/边界、默认关闭、退出发布、跨线程读取及原统计区三级准入与末 owner 退账。
- Debug/Release 各 104 个 harness、702 项测试在原 CPU 20s/墙钟 30s/core 0 的 OS 子进程通过；各包含原示例七项测试，不重复计算为新增测试数。
- 预构建入口的 Debug/Release 完整 AOT 通过，真实一/双 worker 的正常/失败状态、原锁次数与 join 后可用 CPU 值均核对；既有取消、超时、监督、GC 和消息/回复回归通过。
- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets 与 Linux x86-64 all-targets 交叉检查通过。没有原生 Linux 执行。
- 189 份 Markdown 相对链接/围栏、32 份 Python AST 和 `git diff --check` 通过；未读取或修改 `docs/proposals/`。

Rust 清单和逐 harness 日志在 `target/worker-cpu-workspace-{debug,release}-20261118/`，定向/构建/工程/AOT 日志为 `/tmp/wings-worker-cpu-*-20261118.log`，指纹摘要在 `target/worker-cpu-preflight-20261118/report.json`。本轮成功不覆盖先前原限启动超时；本切片没有修复其根因。

## 长测方式

复用 `measure_lock_statistics.py` 的显式独立入口；原 hot-queue 100×4096 B、mailbox 100、pending 256、两个 worker、80000/s，每轮 30s/60s、两对关闭/开启与反序，固定原 CPU 240s/墙钟 130s/阶段 120s。冻结驱动核对原始 40 B 事件、handler 数及全部两个 worker 的 CPU 终态，Unavailable/缺失使实验失效并保留原始输出，不重试或回填旧数据。

```sh
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_lock_statistics.py
WINGS_PROFILE=release python3 scripts/measure_lock_statistics.py --artifact target/worker-cpu-aot-new --output target/worker-cpu-new
```

产物须事先以 `wings-compile build examples/beam-multi-counter.wings` 构建。CPU 区间包含 worker 内初始化和清理，不包含 timer、发生器、其他线程及 OS 启动/退出；它不是六十秒采样 CPU、整机 CPU 或锁内 CPU，不能将持锁墙钟除以此值当 CPU 比例。两对诊断不替代五对正式性能门禁。

本机原生 macOS AArch64 的实际结果另记。完整资源账本、启动超时根因、公平/过载/服务 I/O、CPU 硬配额、原生双平台与 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
