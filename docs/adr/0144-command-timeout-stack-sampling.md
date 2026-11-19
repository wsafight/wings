# ADR-0144：通用命令超时的原进程现场采样

- 日期：2026-11-19
- 状态：已采纳（诊断入口扩展；原超时根因未定）
- 延续：[ADR-0142](0142-opt-in-timeout-stack-sampling.md)、[ADR-0143](0143-shared-invoke-timeout-sampling.md)

## 背景

Debug 完整 AOT 在 `scalar_requests --trusted` 的原 30 秒期限发生一次无输出超时，定向预构建复现随后通过。通用 `command` 使用 `subprocess.run`；超时异常返回给调用者时原直接子进程已进入清理，ADR-0142 的 TCP 握手和 ADR-0143 的共享 `invoke` 采样都覆盖不到这个现场。继续无观测重跑不能区分 cargo/动态装载、宿主等待和受管执行停顿。

## 决定

沿用默认关闭的 `WINGS_TIMEOUT_SAMPLE=1`：

- 开关未设置时，`command` 仍走原 `subprocess.run`，30 秒默认期限、退出核对和成功返回形状不变。
- 开关启用时，通用命令在原期限到达并抛出 `TimeoutExpired` 后、清理前，对仍存活的原直接子进程调用同一 `sample_timeout_stacks`。采样器继续使用独立 CPU 5s、墙钟 3.5s、core 0、文件 8 MiB 限额；采样失败不得替换原超时或跳过清理。
- 诊断路径从启动起使用独立进程组，采样后 killpg、wait 并保留完整 stdout/stderr；随后仍抛出带原期限、清理前后退出值和采样证据的失败。采样可以延长失败后的清理时间，但不延长业务判定，也不把采样窗口内的迟出改成成功。
- 正常完成和预期非零退出不采样，继续返回 `CompletedProcess` 并按原 expected 核对。

只保证采样命令的**直接子进程**。`cargo run` 下该进程可能是 cargo 包装进程，不声称自动取得其 example 后代的业务栈；需要业务进程现场时使用既有 `WINGS_PREBUILT_EXAMPLES=1` 直接执行预构建 example。诊断路径不自动扫描或附加整棵进程树。

## 范围与限制

这是失败后的原生栈快照，不是超时根因修复，也不能恢复超时前历史。通用命令原有输出捕获规模不变；本切片只复用有界采样器和报告文件，不新增输出上限或提高资源额度。默认完整 AOT 不产生采样扰动，原 ABI v22、4352 B 控制预留、业务预算和期限不变。实际验证见[记录](../validation-command-timeout-sampling-2026-11-19.md)；完整资源账本、公平/过载/服务长测、原生双平台与 BEAM/MVP 门禁保持开放。
