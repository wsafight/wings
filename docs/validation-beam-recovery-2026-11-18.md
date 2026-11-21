# 2026-11-18 BEAM 取消与监督恢复对照

本记录对应 [ADR-0131](adr/0131-equivalent-beam-cancellation-recovery.md)，推进 MVP 的请求生命周期和最小恢复。环境为本机 macOS AArch64，Rust 1.99.1、OTP 29；ABI v22、4352 B 控制预留与原业务/资源/时限不变。不包含原生 Linux、CPU 硬配额或吞吐/整机内存验收。

## 业务与判定

两真实 sender 排满 4 KiB Bytes 目标，mailbox 16/共享结果 17。立即超时和已消费取消仍保留队列消息及 publisher 票据，满队列拒绝不改变容量，健康 sibling 提交 +7。其余取消与 stop 竞争只允许单一 Cancelled/Failed；停止不重启。两次取消 Fail 必须实际除零、保留旧零状态，第一次全新零状态重启、第二次强度耗尽；健康 sibling 仍为 +7。每轮和下一轮均拒绝旧身份，末尾实际票据为空。

BEAM 使用真实 CAS mailbox、有限票据适配器和 OTP supervisor；独立目标监督分支保留 Wings 按 child 的强度范围。额外控制进程/monitor 均留在 BEAM 作业内，BEAM 的保留字节/栈字段为 null，队列/票据计数不冒充 Wings 逻辑账本。

## 当前验证

- 独立 Wings/BEAM 重放模型通过，分别拒绝 57/18 组伪造历史；包含请求提前释放、重复消费、候选状态提交、错误恢复、旧身份漏检和 BEAM 账目归因。
- 最终 Debug/Release 真实短回归均在一/双 worker 与 BEAM 一/双 scheduler 下各三轮通过。已有 trace 不覆盖，未知参数、非法 worker 数和 scheduler 不一致均在创建 trace 前拒绝；默认入口仍无 OTP 依赖。各次短测使用原 CPU 5s/墙钟 10s/core 0 和三秒轮期限。
- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check` 通过；183 份 Markdown 的相对链接/围栏和全部 Python 脚本语法通过。
- Release 完整 `verify_scalar_aot.py` 一次通过，包含本切片默认 Wings/两端模型。Debug 完整套件在既有 `verify_multi_counter.py` 的 `beam_multi_counter` 原十秒子进程期限超时，尚未执行到本切片区块；没有放宽期限或重试来替代失败。本次没有捕获该进程原生栈，不能认定为历史 TCP 动态加载停顿的相同根因。
- 显式冻结双端一/双配置各 900 轮通过；每配置完整 300 轮预热/600 轮采样，共 3600 轮，没有缺轮、重试或增加任何原限制。

最终短测证据为 `target/beam-recovery-short-debug-final-20261118/`、`target/beam-recovery-short-release-20261118/`，各有两端一/双配置的独立 `run.json`、控制输出、stderr、RSS 安全观测及三轮 trace。全量日志为 `/tmp/wings-beam-recovery-full-aot-{debug,release}-20261118.log`。初次开发证据为 `target/beam-recovery-development-20261118/` 和 `target/beam-recovery-short-debug-20261118/`；它们保留自己的源码阶段，不与最终冻结结果混用。

## 冻结长测结果

基于提交 `ae5706d77a517f9dde6ba2fbd4c7fd1d7f8a3862`，冻结 487 个源码/运行端/产物文件，分别在每轮前后与最终独立重放时核对全部 SHA。证据目录为 `target/beam-recovery-paired-macos-20261118/`，包含 `environment.json`、四份 `run.json`/原始 trace/控制输出/错误与 RSS 安全日志，汇总为 `report.json`；控制台日志为 `/tmp/wings-beam-recovery-soak-20261118.log`。

| 运行端 | worker/scheduler | 预热/采样轮数 | 实际业务区间 | 最长整轮 | trace 字节 |
|---|---:|---:|---:|---:|---:|
| Wings | 1 | 300/600 | 90.002s | 32.164ms | 1671739 |
| BEAM | 1 | 300/600 | 90.001s | 2.676ms | 1610548 |
| Wings | 2 | 300/600 | 90.001s | 16.854ms | 1671310 |
| BEAM | 2 | 300/600 | 90.000s | 3.573ms | 1611550 |

整轮包含排队、结果消费、停止、两次失败/监督与清理；本表只核对原三秒轮期限，不能当作请求延迟或吞吐比较。四份 trace 各核对 7196 次旧身份失效、900 次自动重启与 900 次强度耗尽。共核对 57600 条 Add 的唯一结果和 7200 次真实 Fail：已取消/超时不撤回消息，候选 99 不提交，健康 sibling 保持 +7。stop 竞争的 Cancelled/Failed 分布允许两端不同；每个请求仍只消费一次。

两份 Wings 最终资源回到原启动基线，BEAM 实际协议槽全部释放且 actor/发送者/监督与适配器结束。四轮均保留 `persistent_recovery_lifecycle_pass=true`，同时 `complete_concurrent_fault_gate_proven=false`、`performance_or_memory_gate_proven=false`。只有每配置一轮正确性长测；没有五对吞吐验收、CPU 带宽配额或完整平台内存。长测期间另外开展的 v2 取证宿主修改不在此冻结副本中，也没有替换运行端或重放本轮失败。

## 复现与边界

```sh
cargo build --workspace --bins --examples --locked
python3 scripts/verify_recovery_soak.py --compare-beam
cargo build --release --workspace --bins --examples --locked
WINGS_PROFILE=release python3 scripts/verify_recovery_soak.py --compare-beam
target/release/wings-compile build examples/concurrent-recovery.wings -o target/recovery-aot-new
WINGS_PROFILE=release python3 scripts/measure_recovery_soak.py --artifact target/recovery-aot-new --output target/recovery-paired-new --compare-beam
```

长测每配置每端固定 30s 预热/60s 采样和 900 轮，不重试或跳轮；每轮三秒，CPU 240s/墙钟 130s/阶段 120s/core 0、64 KiB 缓冲/64 MiB trace 和 1 GiB RSS 安全停止线不变。RSS 普查是安全观测，不是完整平台内存。冻结执行、前后 SHA 核对和独立重放均须成功；失败保留部分 trace、错误与 invalid manifest。

Rust 实现未改变，不把既有测试数量重复当作本切片新增覆盖。完整并发故障、长时过载/真实服务 I/O、公平性、完整账本与监测偏差、最新原生双平台和 BEAM 性能/整机内存双门禁仍未完成，MVP 未完成。
