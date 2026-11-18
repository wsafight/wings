# CPU 配额条件验证（2026-11-18）

按 [ADR-0130](adr/0130-readonly-cgroup-cpu-quota-observation.md) 为既有计划开环与百万 checked 加法/100 突发心跳测量补 CPU 条件入口，基于 `1ab93a6`。没有修改 VM、编译器、ABI v22、运行时/actor 容量或任何阶段/CPU 秒数/墙钟/文件上限。

## 行为

`measure_open_loop.py` 与 `measure_fairness.py` 接受成对的 `--cpu-quota-us`/`--cpu-period-us`。调用者先在原生 Linux 预配置独占 leaf domain cgroup v2 并将整个测量 job 放入其中；启动时组内只能有当前 driver。脚本不创建、移动、限速或写入任何 cgroup，只核对实际内核文件与显式预期。未指定参数保持旧探索模式，不访问 cgroup 控制文件。

范围含 driver、Wings/BEAM 和本作业的普查/probe helper。记录 unified `/proc/self/cgroup`、实际 cgroup2 mount、目录身份、所有可见祖先 cpu.max/burst、进程/线程成员与 SCHED_OTHER、cpu.stat 和每次单调观测区间；不把 aggregate quota/period 当实际可用 CPU、绑核或单 VM 配额。原五对顺序、30s/60s、业务协议和尾延迟要求不变。

每轮 manifest 保留首尾 CPU 观测及样本数，逐秒 `.memory.jsonl` 在开启时附 CPU 数据。控制改变、身份/位置改变、共享外来进程、子组、冻结、非 fair 线程、缺失/回退统计、越界/退出竞争均使实验无效并保存原因，不自动重试或生成零值。宿主和祖先策略仍须可信稳定；采样空隙不是抗管理员修改的连续证明。`performance_gate_proven` 保持 false，原 missing_gates 不自动删除。

## 实际验证

本机 macOS AArch64、Python 3.12，**10 项 unittest 通过**：

- 用临时普通文件和 mock 系统查询验证正常固定控制、祖先、无写入，以及 quota/period/burst/冻结/type/成员/累计值变化拒绝。
- 验证挂载解析/转义、重复或越界成员、错误整数/统计、进程父链、陈旧目录身份、外来成员/子组及线程策略/退出竞争。
- 真实启动有原 OS 限制的短 Python 子进程，通过注入模型 CPU/内存样本核对 collector 可选字段、原控制事件和无性能通过声明；观测中失败会终结该子进程并保留 invalid manifest。这里的 CPU 文件/统计是模型，不能当内核节流证据。
- 两个真实 CLI 的不完整 quota 参数均以退出 3 在实验目录创建之前失败；默认无参不读取 CPU 文件。

两入口在 macOS 传入完整 quota=100000 us、period=100000 us，也均在创建目录/启动业务之前退出 3，stdout 空、错误明确要求原生 Linux cgroup v2。实际命令、退出、目录状态和四个脚本 SHA-256 保留于 `target/cpu-quota-preflight-macos-20261118/report.json`。没有把该早拒绝解释为 macOS 硬配额实现。

原 `verify_measurement.py --compare-beam` 与 `verify_fairness_measurement.py --compare-beam` 通过，包含真实 Wings/OTP 非法参数/已有 trace 拒绝、原 collector、90000 到达重放、内存 probe 和公平性伪造历史。它们不执行 90 秒新长测。日志：

- `/tmp/wings-cpu-quota-unit-final-20261118.log`
- `/tmp/wings-cpu-quota-open-loop-regression-20261118.log`
- `/tmp/wings-cpu-quota-fairness-regression-20261118.log`

workspace all-target check、clippy `-D warnings`、fmt 通过；Python 脚本语法检查通过。Rust 实现自 `1ab93a6` 未改，沿用其 Debug/Release 各 104 harness/698 tests、MSRV 和 Linux 交叉检查证据；不将这些写成本轮原生 Linux CPU 运行。新增十项模型测试已加入既有双平台 CI，但本轮没有推送或运行远端 CI。

## 使用入口与待验证项

先由 Linux 操作者准备好独占、无子组、无 burst 且实际 cpu.max 为 `100000 100000` 的 cgroup，并让启动测量的 Python 进程位于其中。随后可以给原命令加参数，例如：

```sh
WINGS_PROFILE=release python3 scripts/measure_open_loop.py --case hot-queue --artifact target/long-counter --output target/quota-hot --cpu-quota-us 100000 --cpu-period-us 100000
WINGS_PROFILE=release python3 scripts/measure_fairness.py --artifact target/long-fairness --output target/quota-fairness --cpu-quota-us 100000 --cpu-period-us 100000
```

以上是待原生 Linux 复验的命令入口，本机没有运行或配置该组。须验证真实 fork/exec 和线程继承、节流计数、组/祖先配置变化的失败保留，以及新增观测的偏差，再保留两端原五对数据。缺少这些条件时不宣称 CPU 门禁完成。

原 Debug/Release 完整 AOT 的 control_tokens/TCP 阶段失败仍保留在[模块依赖记录](validation-module-dependencies-2026-11-18.md)；本轮没有修复或重试那些失败。完整资源账本、公平/过载/故障、最新原生双平台、平台内存与同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
