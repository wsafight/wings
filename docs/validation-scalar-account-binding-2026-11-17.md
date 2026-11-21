# 2026-11-17 标量进程账户绑定验证

按 [ADR-0114](adr/0114-scalar-process-account-binding.md) 修复从 process 计费池初始化的纯标量实例可直接换池或 detach 后改借其他账户的问题，基于 `de12eb3`。原 ABI v21、4352 B 调用控制预留、CPU 20s/墙钟 30s/core 0、业务阶段 3s 和全部容量限制不变。

## 命令与结果

本机 macOS AArch64、M3 Pro、36 GiB、Rust 1.99.1，顺序执行：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_workspace.py --output target/workspace-validation/scalar-binding-debug-final-20261117
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/scalar-binding-release-final-20261117
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
for suite in counter_equivalence multi_counter senders_counter admission_counter open_loop fairness concurrent_failure; do
  WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_${suite}.py --compare-beam
done
python3 scripts/verify_memory.py
python3 scripts/verify_measurement.py
python3 scripts/verify_fairness_measurement.py
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_workspace_tests.py
```

- check/clippy/fmt/MSRV 和 Linux VM 交叉检查通过；交叉检查不是原生 Linux 执行。依赖隔离及 corosensei 无 unwind 特性检查通过。
- debug/release 各实际执行 95 个 harness、661 项，失败/忽略/过滤 0，其中 VM 库 314 项；doctest 实际各 0 项。两种完整 AOT 均首轮通过；计费模块真实 AOT 专项另外各连续 5 轮通过。
- 新增 5 项回归覆盖同 VM 兄弟 process、其他 tenant/VM、工具池拒绝，拒绝前不借栈或调用 token 工厂，不毒化实例、不改 state/账目；原账户满额不能借其他账户，正确池释放额度后仍提交 7。detach 错误池拒绝后在正确池恢复提交 7 并递增为 9；同账户跨线程换池不重跑 initializer。原账户句柄退出后，末镜像仍保留账户节点票据，释放后回到原基线。
- 原生 Darwin 内存 probe、后代 census/身份/错误模型、开环和 fairness 离线模型通过；运行器自身分别 6 项和 8 项通过。没有执行新的长测。

日志前缀 `/tmp/wings-scalar-binding-`，后缀包括 `check-final`、`clippy-final`、`fmt-final`、`msrv-final`、`linux-check-final`、`build-{debug,release}-final`、`tests-{debug,release}-final`、`doctests-{debug,release}`、`aot-{debug,release}`、`module-repeat`、`beam-<suite>`、`memory`、`measurement`、`fairness_measurement`、`scalar_aot_tests` 和 `workspace_tests`，均为 `.log`。全量目录保留指纹、原始输出和报告。

## 真实 BEAM 回归与限制

七组显式 Wings/BEAM/独立 Python 对照通过业务核验：v1 为 45 业务及 28 JSON/14 bridge 拒绝；v2 为 18 业务及 17 bridge/49 JSON 拒绝；v3 为 21 FIFO/交错、8 伪造 history 及 17 bridge/47 JSON 拒绝；v4 为 1/100/256 满队列准入、健康 actor、停止与清理；v5 为 6 个短时计划及 10 配置拒绝；v7 为两个 CPU/突发心跳计划；v9 为 27 业务、96 枚举/24 伪造 history 及 18 bridge/51 JSON 拒绝。

短时过载结果不得写成性能门禁通过：

| 负载与区间 | 端 | 回复 | 队满拒绝 | 超时 |
|---|---|---:|---:|---:|
| hot-queue，1ms，1280000/s | Wings | 528 | 752 | 0 |
| 同上 | BEAM | 1280 | 0 | 0 |
| dense-64k，10ms，1000000/s | Wings | 1637 | 8363 | 0 |
| 同上 | BEAM | 6862 | 3138 | 0 |

原 CPU/墙钟和阶段限制不变，未设 CPU 硬配额，不能由极短采样推出持续吞吐比值。四组较低速 smoke 两端均收齐，无拒绝/超时。v7 Scheduler yield 样本仍为 0，不报告零延迟或公平性通过。较早针对 `de12eb3` 的七组回归、probe/离线模型另保留 `/tmp/wings-mvp-*-20261117.log`，不混作当前二进制长测。

## 保留的开发失败

新增两个复现测试在旧实现都确定失败，见 `repro.log`；加入账户身份后通过，见 `fixed.log`。初版专项直接调用私有 Invocation 构造器编译失败，见 `focused.log`，后改为 crate 内可见的 ScalarTask 入口，断言仍检查拒绝前不能触发 token 工厂。补充末镜像租约测试时一度访问私有镜像字段，在编译前删除无关内部访问，保留账户生命周期断言。最终专项 9 项通过，见 `focused-complete.log`；本次冻结版本没有全量测试或 AOT 失败，不覆盖历史 TCP 间歇失败证据。

## 尚未完成

完整分配/装载账本、具名记录回复、公平/过载/长时并发故障、最新原生 Linux x86-64 和各平台同机同负载 BEAM 吞吐/整机内存双门禁仍未完成。当前 Docker daemon 不在线；本机已有 Colima 配置均为 AArch64，即使启动也不能代替原生 Linux x86-64 证据。MVP 未完成，不推送、不发布，不调整用户待审语法草案。
