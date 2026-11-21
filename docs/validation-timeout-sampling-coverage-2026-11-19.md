# 共享入口超时采样覆盖验证（2026-11-19）

按 [ADR-0143](adr/0143-shared-invoke-timeout-sampling.md) 将 [ADR-0142](adr/0142-opt-in-timeout-stack-sampling.md) 的默认关闭采样扩展到 `verify_multi_counter.invoke`（八个验证脚本共用，含多 actor 十秒入口与 `observed_yield`），基于 `ddaae33`。未修改任何期限、输出上限、清理契约或默认行为。

## 实现与自检

采样器增加标签参数（消毒后至多 40 字符进入文件名）。`invoke` 失败路径在 killpg 前采样，随后在原 1 MiB/64 KiB 上限内非阻塞补读迟到输出，证据 JSON 追加 `timeout_sampling` 与 `output_bytes_during_sampling`；已关闭流的 `ValueError`、EOF 与 `BlockingIOError` 均不掩盖原始错误。未开启开关时两个字段均不出现。

`verify_multi_counter_tests.py` 从 7 项扩至 11 项并全部通过：新增默认无采样字段、真实卡死进程栈捕获（报告含 Call graph/sleep，`sample_exit=0`，文件名带 `multi-counter-` 标签，清理后 exit=-9）、采样窗口内迟到输出保留（`early\nlate\n` 与迟到字节计数）、已退出进程跳过采样（保留 exit_before_cleanup=7）。

`verify_scalar_aot_tests.py` 10 项及 CI 其余自检（workspace、tcp_bytes、allocation_failures、atomic_progress、lock_statistics、cpu_quota）全部通过。本轮只改 Python 验证脚本与文档。

## 完整 Debug AOT 回归

默认（未开启采样）执行一轮 `WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" python3 scripts/verify_scalar_aot.py`，exit=0，日志 `/tmp/wings-sampling-coverage-full-aot-debug-20261119.log`，SHA `9e83a54d…f04739`，与 ADR-0142 轮次第 3–6 轮输出逐字节一致，确认默认路径行为未变。本轮未复现历史间歇超时，没有真实超时采样报告；既有失败记录保持，根因仍未定。

## 范围与复现

长测 `measure_open_loop.py` 的独立捕获路径未接入；采样仍是失败后取证，不是修复。完整账本、公平/过载/服务长测、原生双平台与 BEAM/MVP 门禁不变。

```sh
python3 scripts/verify_multi_counter_tests.py
python3 scripts/verify_scalar_aot_tests.py
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" python3 scripts/verify_scalar_aot.py
```
