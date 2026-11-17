# ADR-0110：仓库内的有界全量测试入口

- 日期：2026-11-17
- 状态：已采纳（验证基础设施，不是运行时或性能门禁）
- 延续：[ADR-0005](0005-platforms-and-benchmarks.md) 的双平台验证要求与 [ADR-0109](0109-accounted-wakeup-endpoint-leases.md) 的受限子进程回归。

## 问题与决定

近期本机全量测试依赖 `/tmp` 临时脚本，正式 CI 则运行 workspace 默认目标及手工列举的少数 example 测试。新增 example 的测试可能只构建、不执行；外部临时脚本也无法由新环境直接复现。不能把 `--no-run` 成功当测试通过。

增加标准库实现的 `scripts/verify_workspace.py`。它先以 `cargo test --workspace --all-targets --locked --no-run --message-format=json` 生成当前 profile 清单，要求 Cargo 明确成功结束且至少有一个 test executable；去重、规范化路径后为每个可执行文件记录 SHA-256，随后逐个实际执行。保留 debug/release 两种模式，不筛选包、example 或测试名，不传 ignore/filter，不自动重试。零测试 harness 仍记录，但不虚增测试数；缺失/重复总结、失败、忽略、基准模式或过滤项均不能通过。

实际 harness 独立 OS 进程组，stdin 为 EOF，CPU 上限 20 秒、墙钟 30 秒、core 0；超时、退出和异常清理整个进程组，输出直接保留到文件。进程启动前及结束后核对可执行文件指纹，运行期间仍不得并发重建同 profile。CPU 总时长限制不是 CPU 核数/带宽配额，不代表性能隔离或内存硬限制。

必须传入全新输出目录，已有目录直接拒绝，失败不能覆盖旧证据。目录包含 Cargo JSON 清单、stderr、执行前完整指纹清单、逐 harness 合并输出与最终 JSON 报告；失败保留状态、已执行项、退出码和时间，不隐式把后续未运行项算通过。中断时报告仍保持失败。编译由 Cargo 自己管理，不把 20s/30s 运行上限加到完整构建；doctest 独立执行，不声称 all-targets 包含 doctest。

原生 macOS AArch64/Linux x86-64 CI 都改用此入口执行 debug/release 全目标测试，另跑 doctest、既有真实 AOT/依赖检查和 MSRV，不跳过或缩减业务验证；失败也上传已有原始日志。入口自身的单元测试覆盖清单/总结伪造拒绝、OS 限制、指纹变化、超时清理和失败报告。

## 验证与边界

实际结果与首次测试修正见[验证记录](../validation-workspace-runner-2026-11-17.md)。

本机分别实际运行两个 profile，核对 harness/test 数与之前临时运行器一致。注入编译失败、非零退出、没有总结、跳过测试、墙钟超时和指纹变化，均必须失败并保留证据。目录重复运行不得改写旧报告。

这只提高验证可复现性和 CI 覆盖，不能补齐原生 Linux 结果，也不能修复此前 TCP/AOT 启动超时。MVP 的业务值/账本、公平/故障、CPU 硬配额和同负载 BEAM 吞吐/整机内存双门禁仍单独验收。
