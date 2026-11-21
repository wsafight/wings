# ADR-0142：可选的有界超时现场采样入口

- 日期：2026-11-19
- 状态：已采纳（诊断入口；原超时根因未定）
- 延续：[ADR-0112](0112-bounded-tcp-startup-evidence.md)、[ADR-0132](0132-bounded-multi-counter-failure-evidence.md)

## 背景

Debug 完整 AOT 的 TCP normal 场景反复在原五秒握手期限失败（elapsed≈5.0s、exit=-9、双流为空），Release 与定向复验通过。此前两次显式完整 Debug 启动采样均只覆盖到尚未超时的进程，真正超时的原进程已按原契约清理，没有现场栈；首次包装在握手后 join 采样还造成 accept 期限耗尽的观察干扰。需要对**真正超时**的进程取证，而不是继续对健康进程采样。

## 决定

`scripts/verify_scalar_aot.py` 增加默认关闭的 `WINGS_TIMEOUT_SAMPLE=1` 诊断入口：

- 只在 `tcp_ready` 已判定失败（原期限、原判定逻辑不变）且子进程仍存活时，对原超时进程执行 `/usr/bin/sample <pid> 2 -mayDie -file <报告>`。采样工具自身有独立资源上限：CPU 5s、墙钟 3.5s、core 0、文件 8 MiB，超限即终止并保留已有证据。
- 采样在清理前完成（最多增加约 3.5s 的失败清理延迟），随后原样执行 killpg、wait 和双流有界读取；成功路径完全不启动采样，默认行为不变。
- 失败信息在原有字段（elapsed/exit/双前缀等）之后追加采样退出值、报告路径与字节数、采样工具 stderr 前缀；若采样窗口内 stdout 出现迟到的就绪行，追加 `readiness_completed_during_sampling=true`，区分"最终只是慢"与"持续卡住"。
- 报告落在 `target/startup-samples/`，不在默认回归中启用；不改变 ABI、资源上限、任何业务期限或清理契约，不修改 macOS 安全设置或文件扩展属性。

## 替代方案

- 提高握手期限或对失败重试到全绿：拒绝，会掩盖原失败。
- 继续只对健康进程做"启动一秒后"采样：拿不到真正超时进程的现场，且曾干扰 accept 期限。
- 默认启用采样：拒绝，默认回归保持无扰动。
- 调整安全软件或文件属性绕过慢路径：拒绝，改变环境前提且破坏证据链。

## 范围与限制

这是取证能力，不是根因修复。采样在失败判定之后执行，不能回溯超时前的完整历史；报告是被采样时刻的原生栈快照，可能同时包含工具扰动。共享 `invoke` 入口的覆盖由 [ADR-0143](0143-shared-invoke-timeout-sampling.md) 扩展。实际捕捉结果见[验证记录](../validation-startup-timeout-sampling-2026-11-19.md)；完整资源账本、公平/过载/服务长测、原生双平台与 BEAM/MVP 门禁均不因本切片完成。
