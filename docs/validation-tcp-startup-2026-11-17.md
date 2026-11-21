# 2026-11-17 TCP 启动取证验证

按 [ADR-0112](adr/0112-bounded-tcp-startup-evidence.md) 修正验证脚本的半行无界等待和失败日志丢失，基于 `7775b08`。未修改 VM 代码或二进制，沿用该提交的 94 harness/653 项、check/clippy/fmt/MSRV 结果；不把本轮取证称为启动根因修复。

## 实际运行

本机 macOS AArch64、Rust 1.99.1，执行 `python3 scripts/verify_scalar_aot_tests.py`，6 项通过；原运行器 `python3 scripts/verify_workspace_tests.py` 的 8 项也通过。新增测试覆盖正常/半行/EOF/非零退出、编码/JSON/schema、4096 字节边界、大 stderr 及持管道后代清理。新入口接入双平台 CI，但本轮未运行远端 CI。

新脚本首次 TCP 专项在 debug 首场景失败：原 5 秒 deadline 到达，清理前进程仍存活、stdout/stderr 均为空、清理退出 -9；见 `/tmp/wings-tcp-startup-focused.log`。同时完整 debug AOT 在 control_tokens 的原 3 秒阶段失败，见 `/tmp/wings-tcp-startup-aot-debug.log`。没有隐藏失败或放宽限额；之后改为顺序验证，TCP 全部场景 debug/release 各 5 轮通过，日志 `/tmp/wings-tcp-startup-focused-repeat.log`。完整 debug 原限复验通过，release 完整首轮通过，日志 `/tmp/wings-tcp-startup-aot-{debug-repeat,release}.log`。

独立原限 TCP 采样在启动约 1 秒仍无输出、累计 CPU 0 秒时执行 macOS `sample`。879 个采样仅显示主线程 `_dyld_start`，当时 physical footprint 96 KiB；随后同一进程输出握手并以零连接正常结束，资源退回 bootstrap。日志 `/tmp/wings-tcp-startup-sample.log` 与 `/tmp/wings-tcp-startup-native-sample.txt`。这只定位该次停顿在可见的动态加载启动阶段，不足以断言原 5 秒失败的同一根因，更不是 VM 内存/性能数字；没有停止用户应用或系统服务。

## 边界

新代码只保证握手和启动失败证据有界，不自动采样、重试或归因；4096 字节为证据前缀而非完整错误输出。既有后续业务验证仍逐项运行。尚缺最新原生 Linux、CPU 硬配额及同业务 BEAM 吞吐/整机内存双门禁；业务值传输、完整账本、公平/过载/并发故障仍待验收，MVP 未完成。
