# 通用命令超时采样验证（2026-11-19）

按 [ADR-0144](adr/0144-command-timeout-stack-sampling.md) 扩展第 5/6 项失败取证，基于 `e6e7501`。本机 macOS 26.6.2 AArch64；本切片只修改默认关闭的诊断路径，不修改 VM、ABI、预算或业务期限。

## 触发证据与定向复现

此前完整 Debug AOT 已在 `scalar_requests --trusted` 的原 30 秒期限超时，日志保留在 `target/module-path-allocation-full-aot-debug.log`。使用同一源码生成独立 Queue 产物后，直接预构建 example 定向执行一次通过，耗时 5.425043 秒、exit=0、stderr 为空，日志为 `target/scalar-requests-timeout-reproduction.log`。这次通过没有采样，不覆盖历史失败，也不能确定此前停在 cargo、装载或业务执行。

## 自测与真实采样

`verify_scalar_aot_tests.py` 共 14 项通过。新增四项分别核对：默认超时不启动采样；opt-in 在清理前采样、保留双流并 killpg；采样器自身失败不替换原超时且仍清理；正常与预期非零退出保持原返回契约。短测试使用 0.2 秒模型期限，不改变通用命令默认 30 秒。

另在本机用真实 `/usr/bin/sample` 执行 0.2 秒卡死模型。原判定为超时且清理前进程活跃；采样 exit=0，报告 8614 B，随后原进程 exit=-9，stdout 的 `partial` 保留，stderr 为空。日志为 `target/command-timeout-sampling-real.log`，实际报告位于 `target/startup-samples/python3-64026-1795073174648720000.txt`。这是诊断链路自测，不是 `scalar_requests` 原超时现场或根因证据。

真实 cargo 包装与业务 example 的直接子进程边界按 ADR 保留，不将 cargo 栈解释为受管 handler 栈。

## 完整 AOT 与实际超时现场

默认关闭采样时，完整 Debug AOT 再次在原 30 秒期限失败，这次位置是 `record_loops`，双流没有业务输出，日志为 `target/command-timeout-full-aot-debug.log`。它与此前 `scalar_requests` 失败分别保留，不能归为同一根因或用位置变化掩盖失败。

随后以 `WINGS_TIMEOUT_SAMPLE=1` 独立执行完整 Debug，仍在原 30 秒期限失败，这次位置是 `named_record_replies`，日志为 `target/command-timeout-sampled-full-aot-debug.log`。采样器 exit=0，报告为 `target/startup-samples/cargo-68147-1795074610734572000.txt`、24199 B。虽然命令以 `cargo run` 发起，报告确认被采样直接子进程已经执行 `named_record_replies`，所以这是本次业务 example 的真实现场，不是 cargo 等待栈。

报告的一段 1282 样本启动调用图中有 1269 个位于 `_dyld_start`；另一段 245 样本的主线程调用图中，243 个位于 `ScalarModule::load_accounted` → `libloading::Library::new` → `dlopen` → `mapSegments` → `fcntl`，1 个位于 Manifest 解析，1 个位于 `main` 其他位置。由此只能断定**这次**原限超时主要处于原生启动及模块装载，而不是受管 handler 执行。`fcntl` 缓慢的系统原因、采样扰动和其他超时是否同类仍未确定；不修改 socket、安全设置、文件属性或业务期限来规避，也不宣称启动根因已修复。带采样 Debug 不是完整通过。

默认 Release 完整 AOT 通过，日志为 `target/command-timeout-full-aot-release.log`。workspace all-target check、clippy `-D warnings`、fmt、MSRV 1.85 all-target check 与 `git diff --check` 通过；双平台 CI 结果另补。完整资源账本、公平/过载/服务长测、CPU 硬配额和 BEAM/MVP 门禁均未因此完成。
