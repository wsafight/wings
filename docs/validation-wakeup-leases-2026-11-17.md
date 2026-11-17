# 2026-11-17 OS 唤醒租约验证

## 范围与环境

基于 `b1352b4`，在 `feat/mvp` 按 [ADR-0109](adr/0109-accounted-wakeup-endpoint-leases.md) 修复原始 notifier 延长 FD 生命周期却不能延长票据的问题。保持 ABI v20、4352 B 执行控制预留、原 4096 B 等待额度、CPU 20s/墙钟 30s/core 0、业务阶段 3s/TCP 启动等待 5s，不修改用户待审语法草案。

本机 macOS 26.6.2/Darwin 25.6.0、AArch64 Apple M3 Pro、36 GiB，Rust 1.99.1、MSRV 1.85.0。无 CPU 硬配额或最新原生 Linux x86-64 执行结果。

## 缺陷与专项验证

先增加 `worker_notifier_retains_wait_charge_until_its_last_owner_exits`，旧实现确定失败：保留 notifier 后销毁 worker，tenant bytes 实际为 0，预期 4096；日志 `/tmp/wings-wakeup-lease-repro.log`。修复后通过，日志 `/tmp/wings-wakeup-lease-fixed.log`。

票据改由共享端点持有，关闭 FD 后才归还；worker/ingress 不重复计费。验证 process/tenant/VM 的零/少一/精确余量、先准入再调用 OS 工厂、注入创建失败回滚、clone 不重收、账户句柄退出及跨线程末 owner 清理。结构尺寸断言只覆盖逻辑存储，不计内核或 allocator 元数据。实际 OS 唤醒示例包含 16 轮计费生命周期，debug/release 各连续五次通过，日志 `/tmp/wings-wakeup-os-repeat.log`。

## 命令与结果

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

check/clippy/fmt/MSRV 及两种构建通过。`--no-run` 仅生成清单，随后实际逐个在受限子进程运行，debug/release 各 93 harness、647 项通过、失败 0，其中 VM 各 307 项；doc-test 实际各 0 项。清单和执行日志为 `/tmp/wings-wakeup-harness-{debug,release}.jsonl`、`/tmp/wings-wakeup-tests-{debug,release}.log`。依赖隔离和协程禁 unwind 检查通过。

Linux 交叉检查使用 MSRV 1.85.0 通过，见 `/tmp/wings-wakeup-linux-check.log`，只验证编译，不是原生 Linux 唤醒/执行门禁。首次为 1.99.1 安装该 target 时因本机已有未登记文件冲突而失败，见 `/tmp/wings-wakeup-linux-target.log`；未删除工具链文件，改在已有 MSRV 工具链安装 target。

release 完整 AOT 首轮通过，日志 `/tmp/wings-wakeup-aot-release.log`；debug 完整原限复验通过，日志 `/tmp/wings-wakeup-aot-debug-repeat.log`。debug 首轮在 TCP 第一个正常场景的原五秒启动等待失败，日志 `/tmp/wings-wakeup-aot-debug.log`。独立五轮启动诊断均在五秒内收到监听地址，随后主动停止，不作为 TCP 业务通过；没有获得失败现场堆栈，不能据复跑通过将原因归为 OS 或认定已修复。诊断日志 `/tmp/wings-wakeup-tcp-diagnostic.log` 保留。

## 剩余门禁

这是外部事件资源寿命修正，不是完整账本、RSS 或 MVP 验收。具名记录传输、全部增长/启动/装载资源账本、公平/过载/并发故障恢复、最新原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双目标仍未完成。不推送、发布或绕过 main 保护。
