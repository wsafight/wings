# 2026-11-17 控制 token 计费验证

## 范围

基于 `e43193b`，分支 `feat/mvp`，按 [ADR-0107](adr/0107-accounted-control-token-leases.md) 补共享取消/让出原子独立租约及 ScalarWorker/Scheduler actor 接入。ABI v19、4352 B 执行预留、账户/堆/栈/mailbox/request 容量、CPU 20s/墙钟 30s/core 0、业务阶段 3s/TCP 启动 5s 均不放宽。用户 `docs/proposals/` 不读取、修改或提交。

本机 macOS 26.6.2/Darwin 25.6.0、AArch64 Apple M3 Pro、36 GiB，Rust/Cargo 1.99.1，MSRV 1.85.0；无最新原生 Linux x86-64 或 CPU 硬配额，worker 数不当作 CPU 配额。

## 复现与开发失败

- 先加回归再改实现：克隆 worker 私有 preemption 后销毁 worker，tenant 的 used 为零，而标志仍活着；`worker_control_flag_remains_charged_until_its_last_owner_exits` 原代码确定失败，日志 `/tmp/wings-control-lease-repro.log` 保留。
- 第一版统一在所有共享标志里内嵌可选票据，尺寸回归需要 4384 B、超过原 4352 B；293 项通过、1 项失败。未加额度，改为计费/预付两种私有 Arc 表示，让默认调用保持小分配，并显式把两份默认标志存储及引用计数算入原上界；随后 294 项通过。
- 扩展边界 fixture 首轮 298 项通过、1 项失败：VM/tenant/process 上限相同，向 process 填满前 tenant 已被子账户元数据占用，填充本身拒绝。改为分层递减的测试额度，正确在各层建立不足/精确余量；未改 runtime 准入规则，随后 299 项通过。
- 另有 account 移动后借用、测试借用期间移动 process 及删除旧导入后的 warning，编译器发现后按所有权顺序修正；不归因为平台。

## 验证内容

三级不足/精确边界和零变化、clone 不重收、原账户句柄退出及跨线程末释放、执行独占克隆期间的保留、worker 销毁及 actor stop 后外部克隆存活。真实 AOT fixture 覆盖正常完成、原子让出、跨线程取消和挂起 Drop，最后回到实际启动基线；保留 SeqCst、取消不能重置和请求不撤回语义。

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

`--no-run` 只生成当前 executable 清单，随后各 harness 在独立受限 OS 子进程实际执行。完整 AOT 启动后不再修改参与构建的代码、脚本或二进制。验证结果和失败逐次记录，不重复覆盖。

## 已完成结果

- workspace all-targets check、clippy `-D warnings`、fmt、MSRV 1.85.0 all-targets check，以及 debug/release bins/examples 构建通过。
- 当前 executable 清单实际运行：debug/release 各 92 个 harness、631 项、失败 0，VM 各 299 项；doc-test 实际 0 项。测试 harness 最多 6 个受限子进程并行，不把其速度当 VM 吞吐。
- 新 `control_tokens` 真实 AOT 专项 debug/release 各连续 5 轮通过；完整 AOT 两种模式均首轮通过，覆盖 TCP、OS 唤醒、timer、消息/回复、GC、预算/取消、监督与清理，原限不变。
- LSP/VM 依赖边界、corosensei 禁用 unwind、Python 语法、Markdown 相对链接/围栏与 `git diff --check` 通过。源码/日志 SHA-256 与环境见 [control-tokens-macos-2026-11-17.json](control-tokens-macos-2026-11-17.json)。

## MVP 边界

本轮只修正独立控制原子的生命周期归属，不将逻辑预留当 allocator 实际内存。通用记录、其他增长/启动/装载资源账本、长时公平/过载/并发故障恢复、最新原生双平台和同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，不宣布 MVP 完成，不推送或发布。
