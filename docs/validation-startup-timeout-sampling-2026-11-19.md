# 有界超时现场采样验证（2026-11-19）

按 [ADR-0142](adr/0142-opt-in-timeout-stack-sampling.md) 接入默认关闭的 `WINGS_TIMEOUT_SAMPLE=1` 诊断入口，基于 `6fa35f4`。没有修改 ABI v22、4352 B 执行控制、32 MiB VM 上限、TCP 五秒握手/其他业务期限、CPU 20s/core 0 限制或清理契约，没有修改 macOS 安全设置或文件扩展属性。

## 实现与自检

`scripts/verify_scalar_aot.py` 的 `tcp_ready` 在失败判定之后、killpg 清理之前，对仍存活的子进程执行 `/usr/bin/sample <pid> 2 -mayDie -file <报告>`；采样工具独立限额 CPU 5s、墙钟 3.5s、core 0、文件 8 MiB，超限终止并保留已有证据。报告写入 `target/startup-samples/`；失败信息在原有字段后追加采样退出值/路径/字节数/stderr 前缀，采样窗口内出现完整就绪行时追加 `readiness_completed_during_sampling=true`。成功路径与默认（未开启）行为不变。

`scripts/verify_scalar_aot_tests.py` 从 6 项扩至 10 项并全部通过：新增默认关闭无采样字段、真实卡死进程现场栈捕获、采样窗口内迟到就绪标记、已退出进程跳过采样。真实捕获证据为 `target/startup-samples/tcp-ready-92834-1795022654188755000.txt`（卡死进程，Call graph 含 sleep）与 `...656706124000.txt`（迟到就绪场景），SHA 分别为 `cf637e68…35dca`、`bb9cb981…37584`。

CI 同名自检 `verify_workspace_tests`、`verify_scalar_aot_tests`、`verify_tcp_bytes_tests`、`verify_allocation_failures_tests`、`verify_atomic_progress_tests`、`verify_multi_counter_tests`、`verify_lock_statistics`、`verify_cpu_quota` 全部通过。本轮只改 Python 验证脚本与文档，未改 Rust 源码。

## 完整 Debug AOT 六轮捕捉

`WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" WINGS_TIMEOUT_SAMPLE=1 python3 scripts/verify_scalar_aot.py` 顺序执行六轮，全部 exit=0：

| 轮次 | 日志 | 结果 |
|---|---|---|
| 1 | `/tmp/wings-startup-sample-full-aot-debug-20261119.log` | exit=0，SHA `2b0ed08d…d36e49e` |
| 2 | `/tmp/wings-startup-sample-full-aot-debug-2-20261119.log` | exit=0，SHA 同轮 1 |
| 3–6 | `/tmp/wings-startup-sample-full-aot-debug-{3,4,5,6}-20261119.log` | 均 exit=0，SHA 均 `9e83a54d…f04739` |

六轮均未触发五秒握手失败，因此没有产生真实超时进程的采样报告（`target/startup-samples/` 中仍只有上述两份自检文件）。同日 00:19 的 TCP normal 原五秒失败（`d49cbe2` 后 Debug 完整 AOT，elapsed=5.008s、exit=-9、双流为空）及更早各次失败记录保持有效；本轮通过不构成根因修复，也不将采样器称为已捕获真实超时现场。间歇失败本轮未复现，停止追加盲重试。

## 范围与下一步

采样器只在失败判定后取证，无法回溯超时前历史；其他期限入口（多 actor 十秒入口、`observed_yield` 等）本轮未接入。后续任何一轮原失败复现时，保留对应日志与 `target/startup-samples/` 报告即可用于根因分析；不把持续重跑至全绿当作验证。完整资源账本、启动根因、公平/过载/服务长测、原生双平台与同机同业务 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。

## 复现入口

```sh
python3 scripts/verify_scalar_aot_tests.py
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" WINGS_TIMEOUT_SAMPLE=1 python3 scripts/verify_scalar_aot.py
```
