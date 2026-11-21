# 无限 handler 原子让出进展验证（2026-11-18）

按 [ADR-0137](adr/0137-infinite-handler-atomic-progress.md) 补 MVP 公平调度的前置缺口，基于 `9174455`。本机为 macOS 26.6.2 AArch64、M3 Pro、36 GiB，Rust 1.99.1；没有原生 Linux。没有改 VM 实现、ABI v22、4352 B 控制预留、原百万 checked 加法/100 心跳业务或任何已有期限与资源容量。

## 业务与证据边界

`scheduler_atomic_progress` 复用真实 AOT 的 SchedulerSlices：每 worker 启动一个先写候选 99、随后永久循环的 Hold，另外八个健康 actor。步骤片和忙 handler 总预算均为 `u64::MAX`，量子 1 ms；先等每 worker 实际确认原子让出，再顺序运行 16 轮、每轮八次 Work，核对每次精确结果和最后 Read=16。忙 actor 保持原 worker、Suspended、预算未耗尽且旧提交仍为 0。分别验证一/双 worker；单 worker 配置确保双方确实竞争同一 worker。

结果逐条记录 128 个心跳的发送/观察时钟，以及每 worker 固定 256 槽 FIFO 中保留的原子请求/确认、序号和按完整 Pid 匹配的 actor 序号：忙 actor 为 0..workers-1，健康为 workers..workers+7，替代实例为 workers+8。FIFO 在停止前不消费，尾部满额明确计 dropped；合法请求区间可以重叠，但请求与确认各自必须单调。停止正常取消无限 handler、在 Scheduler 仍开放时检查旧身份与槽复用、join worker、保留零失败终态，最后账户精确回到非零根基线。每配置从账户/模块创建到 join、观测排空和退账均受原 3 秒内部期限约束；外层仍 CPU 20s/墙钟 30s/core 0。

独立 `verify_atomic_progress.py` 核对精确参数、两配置、完整串行心跳顺序、状态、每 worker 初始忙 actor 让出早于首心跳、FIFO 固定保留/尾丢样、时钟和停止计数守恒、资源回执。八项模型覆盖缺字段、bool 冒充整数、假 actor/worker、缺失/重复/乱序心跳、错序号、倒退时钟、期限、容量、溢出、假提交和假清理。真实 PID 匹配和 Suspended 快照由可信原生例程核对，不把 JSON 重放称为独立执行引擎。

## 首次运行与失败保留

首次 Debug/Release 真实例程各一次、一/双配置均通过，输入产物、可执行副本、双流和 SHA 保留在 `target/atomic-progress-initial-20261118/`。两构建的单 worker 均 published=273/taken=256/dropped=17，双 worker 各 published=taken=252/dropped=0；每配置 128 个健康请求精确完成，根基线均为 232 B。原 JSON 只记录到停止，后续补 `finished_ns` 和排空/退账后的期限检查；首次证据不改写为最终 schema 或全程耗时。

首次 Python 自测有一项失败：伪造 dropped=0 与合法基线相同，却错误期望拒绝。保留 `/tmp/wings-atomic-progress-model-initial-20261118.log`，仅修测试为修改原值，不收紧合法零丢样或放宽验证器。修正后八项通过；新增结束时间后八项也通过，日志分别为 `/tmp/wings-atomic-progress-model-final-20261118.log` 和 `/tmp/wings-atomic-progress-model-deadline-20261118.log`。

首次全量预构建之后补结束时间和开放 Scheduler 内的旧身份/槽复用验证：先等旧 actor 退场，再创建替代实例，核对旧身份拒绝和新实例 Read=0，不把关闭后的统一拒绝冒充身份检查。已开始的 Debug workspace 继续执行原二进制，后续 shell 阶段已停止，不能把该轮说成最终源码全量验证。最终源码的运行和工程验证另记。

## 最终验证

最终源码 Debug/Release 的独立受限运行与逐条重放均通过，保留原产物与新可执行副本至 `target/atomic-progress-final-20261118/`，八个文件 SHA 再核对一致。每配置 128 个健康请求及替代实例 Read=0，停止后根基线均为 232 B；以下仅为短测执行情况，不是吞吐或尾延迟结果：

| 构建 | worker | 内部完成时间 ms | 各 worker published/taken/dropped |
|---|---:|---:|---|
| Debug | 1 | 793.173042 | 273/256/17 |
| Debug | 2 | 316.463458 | 251/251/0、251/251/0 |
| Release | 1 | 343.131000 | 273/256/17 |
| Release | 2 | 301.172958 | 240/240/0、240/240/0 |

最终 Debug/Release workspace 各 106 harness/707 项测试通过，报告为 `target/atomic-progress-workspace-{debug,release}-final-20261118/report.json`。八项原子进展模型、八项 workspace 运行器与六项 AOT 运行器自检通过，日志 `/tmp/wings-atomic-progress-python-final-20261118.log`；模型内部专门产生的失败报告是断言对象，不是实际 workspace 失败。229 个 Markdown/Python 文件的相对链接/围栏/AST 检查通过，报告 `target/atomic-progress-static-20261118/report.json`。

本轮 Debug 完整 AOT 已先通过新增原子进展回归，但随后 TCP normal 场景在原 5 秒启动握手期限失败：elapsed=5.012s、清理前仍活、清理 exit=-9、stdout/stderr 前缀均空。原日志 `/tmp/wings-atomic-progress-full-aot-debug-final-20261118.log` 保留；没有同时构建，也没有将旧启动采样套用到这次失败。不能宣称本轮 Debug 完整 AOT 通过。顺序执行的 Release 完整 AOT 通过，日志 `/tmp/wings-atomic-progress-full-aot-release-final-20261118.log`，它不覆盖或关闭 Debug 启动失败。

后续 TCP 专项四轮/36 次握手通过，但 Debug 完整复验在 `observed_yield` 原十秒入口超时，双流仍空；两种构建该入口的定向冻结复验又通过。完整失败和两组模块加载采样见[启动复验](validation-startup-followup-2026-11-18.md)，不将其中任何一次通过称为启动问题修复。

最终 `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets、Linux x86-64 all-targets 交叉检查和两构建 workspace doctest 全部通过，日志 `/tmp/wings-atomic-progress-{check,clippy,fmt-check,msrv,linux-cross,doctest-debug,doctest-release}-final-20261118.log`。工程命令在 Release AOT 结束后才启动；交叉检查没有执行 Linux 代码。源码/证据的最终指纹清单见 `target/atomic-progress-preflight-20261118/report.json`。

复现入口：

```sh
python3 scripts/verify_atomic_progress_tests.py
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo run --locked -p wings-compile -- build examples/scheduler-slices.wings -o target/atomic-progress-artifact
target/debug/examples/scheduler_atomic_progress --trusted target/atomic-progress-artifact > target/atomic-progress.json
python3 scripts/verify_atomic_progress.py target/atomic-progress.json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

完整 AOT 入口为例程施加上述 OS 限制；单独执行例程不等于外层限制已生效。CI 在两个原生平台运行八项模型及 Debug/Release 完整 AOT；本机源码接入不等于远端 CI 或原生 Linux 已运行。

## 启动采样旁证

本轮另为历史启动超时采集 `sample`，保留于 `target/startup-native-samples-20261118/`：冻结空 Debug harness、Debug/Release v2 Counter 和单 actor/256 B/一次加一输入。各业务与采样工具均 exit 0，没有复现原超时。原 CPU/墙钟限制分别为 20s/30s、5s/10s；core 0，采样输出另限 8 MiB。50 ms 后启动的一秒采样中，空 harness 的 85 个样本停在 `_dyld_start`；Counter 多数也在此，少量进入原生装载或系统同步通知等待。

报告 1.808/1.543/0.620 秒包含等待采样工具，并非纯启动或业务耗时；采样本身有扰动。历史 worker_ingress 30s 超时没有原现场栈，本轮既不能确认其根因，也不能宣称已修复启动稳定性。

## 尚缺门禁

本例程只证明原子让出条件下的短时健康进展及停止清理，不给完整公平性分布、p99 <5ms、OS 单核配额、长时过载或性能结论。完整资源账本、启动超时根因、公平/故障/服务 I/O 长测、原生 Linux/双平台，以及同机同业务 BEAM 持续吞吐与整机内存双门禁仍待完成，MVP 未完成。
