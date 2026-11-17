# 2026-11-17 全量测试入口验证

按 [ADR-0110](adr/0110-reproducible-bounded-workspace-validation.md) 将临时 `/tmp` 运行器替换为仓库内可复现入口，并接入既有 macOS AArch64/Linux x86-64 CI。运行时/ABI/业务与性能限制均不变；基于 `b7cac72`，不涉及待审语法草案。

## 实际结果

本机 macOS AArch64、M3 Pro、36 GiB、Rust 1.99.1，执行：

```sh
python3 scripts/verify_workspace_tests.py
python3 scripts/verify_workspace.py --output target/workspace-validation/local-debug-20261117
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/local-release-20261117
```

- 运行器自身 8 项单元测试通过，涵盖清单完整/非空/去重、非法总结、失败/跳过、stdin EOF、真实 OS 限制、指纹变化、超时进程组清理、编译失败与成功/失败报告、零测试 harness 及目录拒绝覆盖。
- 两种 profile 均实际执行 93 个 harness、647 项，失败/忽略/过滤/基准项均为 0；与此前临时运行器结果一致。每项 CPU 20s/墙钟 30s/core 0，未因完整运行耗时放宽单项限制。
- 每个目录保留 `cargo.jsonl`、`cargo.stderr`、`inventory.json`、93 份原始 harness 日志和 `report.json`，指纹在执行前后核对。没有将编译或零测试 harness 算成业务测试数。
- CI YAML 解析、Python AST、Markdown 文件链接/围栏和 `git diff --check` 通过；相同 Rust 源码的 check/clippy/fmt/MSRV、doctest、完整 debug/release AOT 结果见[唤醒租约验证](validation-wakeup-leases-2026-11-17.md)。

首次运行器单测 5 项通过、1 项失败：macOS 临时目录 `/var` 被 resolve 为 `/private/var`，测试预期未规范化。统一测试根路径后通过，保留 `/tmp/wings-workspace-runner-tests.log`，没有放宽生产清单检查。补齐两项报告路径回归后 8 项通过，日志 `/tmp/wings-workspace-runner-tests-final.log`。实际运行日志 `/tmp/wings-workspace-runner-{debug,release}.log`。

## 边界

提交 CI 配置不是已经运行远端 CI；本轮没有最新原生 Linux 执行结果。all-targets 不包括 doctest，真实 AOT 和 BEAM 业务协议仍独立验证。运行器不自动重试，既有 TCP 五秒启动失败仍保留且根因未定。具名记录传输、完整资源账本、公平/过载/故障、原生双平台和同业务 BEAM 吞吐/整机内存双门禁仍未完成，MVP 状态不变。不推送或发布。
