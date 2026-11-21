# 2026-11-18 开环 worker 锁诊断

对应 [ADR-0133](adr/0133-open-loop-worker-lock-diagnostics.md)。本机 macOS AArch64/Rust 1.99.1；复用已有计费锁统计 API，只为 `beam_open_loop` 示例添加显式长测后缀及冻结诊断驱动，不改变 VM、ABI v22、业务/原限或取消/让出检查。

## 测量范围

hot-queue 原业务为 100 actor×4096 B Bytes，mailbox 100、pending 256、两个 worker、90% 请求到 actor 1；本诊断固定协议已有 80000/s。每轮仍 30s 预热/60s 采样，保留 40 B 事件逐条重放及所有迟发/拒绝/超时。两对反序为关闭/开启、开启/关闭，用于观察统计扰动，不充作五对性能门禁。

统计在 actor 停止、worker join 后读取，累计范围包含 Configure、预热、采样和清理。claim/start/snapshot/finish 的等待与锁内时间是墙钟，不能当 CPU；没有计时器 mutex、宿主结果表锁或 Condvar 睡眠。独立校验全部 worker、无饱和、最大值和总和，以及 start/finish 恰好等于 actor Configure 数加最终实际提交增量；仍须原逐条业务重放通过。

## 验证状态

- 新六项 Python 模型测试通过，覆盖开关、统计范围/worker join、完整身份、u64/溢出/最大值、零次获取、真实 handler 总数及冻结输入变化拒绝。
- Debug/Release 示例各七项 Rust 测试在受限 OS 子进程通过，含新增 CLI 后缀/普通 smoke 拒绝；真实 CLI 均验证非法参数不创建 trace、已有 trace 不覆盖。
- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets 以及 Linux x86-64 all-targets 交叉检查通过；交叉检查不算原生 Linux 执行。
- 初次 Debug 全量 AOT 与 Cargo 交叉构建同时运行，既有 `cargo run ... worker_ingress` 在原 30s 期限超时，保留失败，根因未定。构建结束后，预构建入口顺序执行的 Debug、Release 全量 AOT 均通过；没有提高期限，也不以成功覆盖原失败。
- 187 份 Markdown 的相对链接/围栏、全部 Python AST 及 `git diff --check` 通过。实际冻结长测结果另记，不用模型或定向检查代替。

日志为 `/tmp/wings-open-loop-locks-{unit-final,tests-debug,tests-release,cli-debug,cli-release,check-final,clippy,fmt,msrv,linux-cross,full-aot-debug,full-aot-debug-sequential,full-aot-release}-20261118.log`，检查摘要在 `target/open-loop-locks-preflight-20261118/report.json`。本切片不重复执行未更改 VM 的所有 Rust harness；既有 698 项证据不增加本次执行次数，当前示例新增一项测试。

## 复现

```sh
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_lock_statistics.py
WINGS_PROFILE=release python3 scripts/verify_lock_statistics.py --runtime
target/release/wings-compile build examples/beam-multi-counter.wings -o target/lock-diagnostic-aot-new
WINGS_PROFILE=release python3 scripts/measure_lock_statistics.py --artifact target/lock-diagnostic-aot-new --output target/lock-diagnostic-new
```

驱动冻结运行端/产物/全部源码及原生内存 probe 后，用 `exec` 切换至冻结 Python 驱动。运行前后核对 SHA，原限 CPU 240s/墙钟 130s/阶段 120s、10 GiB trace 和 1 GiB RSS 安全停止线保持；失败保留部分事件/统计/错误和 invalid manifest，不自动重试。`--observe-locks` 只允许手动长测，默认完整 AOT 不启动 90s 诊断。

## 实际两对长测

冻结提交 `e2fe7def43be8b2dbafd83212c3ed914a3436f8b` 的 496 项输入，原生 Apple M3 Pro/macOS 26.6.2 AArch64 顺序执行关闭/开启、开启/关闭。四轮各 30s 预热/60s 采样，均完整收齐请求、核对状态并回到 232 B 根启动账本；没有并行运行本项目构建或测试。主机其他活动和 CPU 硬配额仍未隔离。

下表为各轮 60s 采样区间，延迟从计划到达到结果观察，包含发生器迟发；单位 ms。pending 满和超时均为 0，队满均不能省略。

| 对/统计 | 计划数 | 按时回复 | 队满 | p99 | p999 |
|---|---:|---:|---:|---:|---:|
| 1/关闭 | 4800000 | 4785820 | 14180 | 0.093167 | 5.453625 |
| 1/开启 | 4800000 | 4781266 | 18734 | 0.101833 | 6.693500 |
| 2/开启 | 4800000 | 4785155 | 14845 | 0.099042 | 5.495000 |
| 2/关闭 | 4800000 | 4756511 | 43489 | 1.470458 | 8.339833 |

每轮 2400000 次预热中的队满依次为 16914、5489、7983、63471，pending 满/超时均为 0。开启统计在第一对更差、第二对更好，不能确定其端到端开销，也不能把任一低分位当作优化收益。四轮零拒绝候选均不通过。

以下是两名 worker 的累计之和，涵盖完整 Scheduler 生命周期，单位 s；次数包括 Configure，不能除以 60s 当作采样期速率。

| 开启轮/阶段 | 获取次数 | 等待墙钟 | 持锁墙钟 |
|---|---:|---:|---:|
| 1/claim | 23083593 | 2.094862 | 8.006652 |
| 1/start | 7175877 | 1.484308 | 1.036488 |
| 1/finish | 7175877 | 1.728444 | 2.720096 |
| 2/claim | 23266801 | 2.280125 | 8.088941 |
| 2/start | 7177272 | 1.527033 | 1.042766 |
| 2/finish | 7177272 | 1.703689 | 2.724036 |

两轮 snapshot 都是 0 次，因此没有样本，不能报其延迟为 0。两轮 start/finish 分别恰好等于 Configure 加全部实际提交的 7175877/7177272 个 handler，claim 还包含没有认领到业务的循环。两轮四阶段累计等待为 5.307614/5.510847s，累计持锁为 11.763236/11.855743s；它们仍是可能含 OS 停顿的墙钟，不能解释为 CPU 消耗或全部 worker 时间。下一步先补 worker CPU 范围，不能仅凭 claim 次数改唤醒/迁移或恢复已回退的候选。

进程树加 driver 的观测 RSS 峰值依次为 54722560、65912832、73056256、77119488 B，原生完整样本数为 90/90、88/88、88/88、89/89。driver 跨轮复用，采样时刻与进程集合的限制保留，不能把这些峰值当作 Wings 独占内存或两对内存验收。

## 原始证据

目录 `target/open-loop-locks-macos-20261118/` 保存冻结副本、逐轮 stdout/stderr、原始事件、内存样本和 manifest；总报告 `report.json`，再次独立重放与全部文件指纹见 `independent-replay.json`。日志为 `/tmp/wings-open-loop-locks-measure-20261118.log` 和 `/tmp/wings-open-loop-locks-replay-20261118.log`。

| 轮 | 事件字节 | SHA-256 |
|---|---:|---|
| 1/关闭 | 574756240 | `50f2c2c36d7db1c8822217c9270a9652eca11c27041e2de725590611bad153fd` |
| 1/开启 | 575031080 | `60c3e6ce7c83682713389f2fa6943ef725db1e1223fc8540e28db559c7e20f4e` |
| 2/开启 | 575086880 | `1a29d4c2650648b516292ae761f724b4f97c098d60663139a0bbc669e166414e` |
| 2/关闭 | 571721600 | `7a044b021e77995a06fad7882dfd0fe0a233fd7f5896441a6960e1174aef4603` |

本机没有 CPU 硬配额/主机隔离，不能把两对诊断称为完整归因、吞吐提升或内存门禁。原启动超时、公平/过载/服务 I/O、完整账本与平台内存、原生双平台和 BEAM 同负载双门禁仍未完成，MVP 未完成。
