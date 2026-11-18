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

本机没有 CPU 硬配额/主机隔离，不能把两对诊断称为完整归因、吞吐提升或内存门禁。原启动超时、公平/过载/服务 I/O、完整账本与平台内存、原生双平台和 BEAM 同负载双门禁仍未完成，MVP 未完成。
