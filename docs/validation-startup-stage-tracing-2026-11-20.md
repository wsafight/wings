# 有界启动阶段轨迹验证（2026-11-20）

按 [ADR-0148](adr/0148-bounded-startup-stage-tracing.md) 接入默认关闭的 `WINGS_STARTUP_TRACE_DIR`，基于 `8789337`。本机 macOS 26.6.2 AArch64、Rust 1.99.1。没有修改 ABI v22、4352 B 控制预留、业务预算、TCP 五秒握手、共享入口十秒、通用命令三十秒或清理契约。

## 实现与有界自测

`wings-vm` 的模块加载记录路径、元数据、清单、映像预检、`dlopen` 和符号解析。每进程独立 `create_new` 文件，普通记录至多 64 条、文件至多 8192 B，超限写 `startup_trace_truncated` 后关闭；每条使用固定 128 B 栈缓冲。目录/文件初始化或写错误只关闭诊断，不写业务双流。

`cargo test --locked -p wings-vm startup_trace --lib` 的 5 项测试通过：顺序/PID/load ID/单调相对时钟与非法阶段拒绝、两个并发加载的身份保持、65 次标记后的显式截断、缺失目录不创建，以及只读句柄的真实写错误后不再记录。首次写错误测试误用 `/dev/full`，在本机以 ENOENT 失败；随后改为打开只读文件句柄制造写错误并重跑通过，首次失败不记为测试通过。

默认路径仍只在首次标记时检查环境并保留 OnceLock 空分支，不声称零开销。诊断文件、FD 和初始化分配属于外部测试成本，启用轨迹的结果不作性能对照。

## 真实 AOT 成功与默认关闭对照

构建 `observed_yield` 后，复用 ABI v22 的冻结 `target/observed-yield-live-20261118/artifact`，以同一可执行文件分别运行开启和默认关闭两次，均 exit=0、stderr 为空，成功 JSON 的键集合一致，业务核对 `status=ok`、完整 state 与资源清理。默认关闭没有新增轨迹文件。

开启轮产生 `target/startup-stage-validation-20261120/final-load-id/startup-39666-1795152260964828000.log`，319 B、9 条记录，全部使用 load ID 1，阶段严格为：

```text
module_load_begin
module_path_ready
module_metadata_ready
module_manifest_ready
module_image_begin
module_image_ready
module_dlopen_begin
module_dlopen_ready
module_symbols_ready
```

该轮 `module_dlopen_begin` 到 `module_dlopen_ready` 为 814459 ns。它只证明诊断时间线和本次短装载，不是历史五秒/十秒/三十秒超时现场、OS 根因或装载性能分布。默认与开启输出分别保存在同目录 `default.{stdout,stderr}` 和 `traced.{stdout,stderr}`。

另以不存在的产物目录执行真实进程，进程非零退出、stdout 为空；独立 `failure/` 中只保留一条 `module_load_begin`，没有把路径解析失败写成 ready。此用例验证失败时的最后完成阶段，不模拟间歇原生阻塞。

## 工程与完整 AOT 回归

- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check` 和 `git diff --check` 通过。
- 最终 load ID 代码的 Debug/Release 全目标有界测试分别通过 **108 个 harness、724 项测试**；清单、SHA-256 和逐项原始日志保留在 `target/startup-stage-workspace-{debug,release}-final-20261120/`。每个 harness 沿用 CPU 20s、墙钟 30s、core 0。
- 原限完整 Debug AOT 在 `stack_boundary --trusted <counter> <pressure>` 的 **30 秒**通用命令期限失败，日志为 `target/startup-stage-validation-20261120/full-aot-debug/run.log`（6724 B，SHA-256 `8f76c196f90b40516a4a6036975823c13f20e823277a4ba4771e43106b37ad2a`）。此前 31 个阶段通过不覆盖此失败，也不据此推定其他历史超时同因。
- 超时后的真实报告为 `target/startup-samples/stack_boundary-48459-1795160499599799000.txt`（23814 B，SHA-256 `c3d5b699ff87bdb49299166b1f1e7c3103cc0690f5f7709cea98a0bc8caeadd6`）。文件名中的 48459 是采样器所属 Python PID；报告明确记录业务 `stack_boundary` PID **49535**、父进程 48459。主线程 599 个样本中 588 个在 `_dyld_start`；启动阶段和进入 `main` 后的直接 `dlopen` 均出现 `RemoteNotificationResponder::blockOnSynchronousEvent -> mach_msg2_trap`。这支持本次 dyld 同步通知等待候选，但没有定位通知对端、OS 延迟来源或 Wings 可修复根因。
- 该 example 直接调用 `libloading::Library::new`，不经过 `ScalarModule`，所以 PID 49535 没有阶段文件是预期覆盖边界，**不能**用无文件单独证明尚未进入 `main`；上述启动结论来自原生栈。通用 `sampled_command` 随后在 `killpg` 遇到 `PermissionError: [Errno 1]`，覆盖了预期的超时 AssertionError；原 `TimeoutExpired`、采样报告与清理异常都留在日志，查询时 PID 49535 已不存在。不重跑 Debug 掩盖此现场。
- 原限完整 Release AOT 一轮通过，最终摘要包含执行/栈边界、持久根/GC、消息/回复、监督、OS 唤醒与产物拒绝；日志 `target/startup-stage-validation-20261120/full-aot-release/run.log`（6841 B，SHA-256 `00bb2a652119e1e7902d15da09e5cad0604ce7d8fc6afa6a759db8ff12a6b5b5`）。这不覆盖上述 Debug 失败。
- 对完整套件已有轨迹逐文件解析：Debug 86 文件/698 条、Release 210 文件/1681 条，四字段、文件 PID、64+1 行与 8192 B 上限均通过；最大文件分别 1279/319 B。该核对不包括没有进入 `ScalarModule` 的进程。
- `CARGO_TARGET_DIR=target/startup-stage-msrv-20261120 cargo +1.85.0 check --workspace --all-targets --locked` 与 `CARGO_TARGET_DIR=target/startup-stage-cross-20261120 cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked` 通过。后者只是交叉检查，不是原生 Linux 执行。历史间歇超时不会因其他轮次通过而关闭。

另有一次误把 `--help` 传给无参数解析的完整脚本，实际启动默认 Debug AOT 后被外层 120 秒工具期限中止；这不是脚本原限业务判定。随后确认没有相关遗留进程，并使用上述显式诊断入口完成正式 Debug 轮。

## 复现入口

```sh
cargo test --locked -p wings-vm startup_trace --lib
cargo build --locked -p wings-vm --example observed_yield
mkdir -p target/startup-stages
WINGS_STARTUP_TRACE_DIR="$PWD/target/startup-stages" \
  target/debug/examples/observed_yield --trusted <artifact>
```

需要真实超时关联时，在同一原始运行中同时设置 `WINGS_TIMEOUT_SAMPLE=1` 和上述目录，并按报告中的 PID 关联 `startup-<pid>-*.log` 与采样文件。采样仍在原失败判定后执行，不延长期限。没有复现时只称诊断链路通过，不持续盲跑至全绿。
