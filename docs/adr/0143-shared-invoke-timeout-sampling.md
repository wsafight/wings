# ADR-0143：共享有界调用入口的超时采样覆盖

- 日期：2026-11-19
- 状态：已采纳（诊断入口扩展；原超时根因未定）
- 延续：[ADR-0142](0142-opt-in-timeout-stack-sampling.md)、[ADR-0132](0132-bounded-multi-counter-failure-evidence.md)

## 背景

ADR-0142 只为 `tcp_ready` 接入了超时现场采样；历史上 Debug 完整 AOT 的多 actor 十秒入口（`verify_multi_counter`）与 `observed_yield` 独立入口也发生过原限超时，且同样没有原进程现场。这两个入口与其他六个验证脚本共用 `verify_multi_counter.invoke` 的有界捕获路径。

## 决定

在同一 opt-in 开关 `WINGS_TIMEOUT_SAMPLE=1` 下扩展覆盖，默认行为不变：

- `verify_scalar_aot.sample_timeout_stacks` 增加标签参数（消毒后进入报告文件名），采样器的独立限额（CPU 5s/墙钟 3.5s/core 0/文件 8 MiB）与"仅对仍存活进程、失败判定后执行"的契约不变。
- `verify_multi_counter.invoke` 的失败路径在 killpg 之前调用同一采样器；随后在原有上限内做一次非阻塞补读，把采样窗口内迟到的输出保留进原证据（各流原 1 MiB/64 KiB 上限与前缀截断标记不变），并在证据 JSON 追加 `timeout_sampling` 与 `output_bytes_during_sampling`。
- 未开启开关时证据字段完全不出现，原证据形状、期限、清理和退出语义不变；已退出进程跳过采样；补读遇到已关闭流或 EOF 不影响原始错误。

## 范围与限制

覆盖 `invoke` 的全部八个调用方（multi-counter、fairness、`observed_yield`、admission/concurrent failure、senders、open-loop/measurement、lock statistics 等），但仍是失败后的原生栈快照与有界输出，不能回溯超时前历史；长测 `measure_open_loop.py` 的独立捕获路径本轮未接入。它不修复任何超时，实测见[验证记录](../validation-timeout-sampling-coverage-2026-11-19.md)；完整账本、公平/过载/服务长测、原生双平台与 BEAM/MVP 门禁不变。
