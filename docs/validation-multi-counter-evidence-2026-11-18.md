# 2026-11-18 多 actor 对照有界取证验证

对应 [ADR-0132](adr/0132-bounded-multi-counter-failure-evidence.md)。本机 macOS AArch64、Rust 1.99.1、Python 3.12/OTP 29。只修改 v2 Python 验证宿主和短测试/CI 入口；VM、ABI v22、业务模块、CPU 5s/墙钟 10s/core 0、三秒 actor 阶段及原输出上限不变。

## 保留的原失败

基于 [BEAM 恢复切片](validation-beam-recovery-2026-11-18.md) 的完整 Debug AOT，在 `verify_multi_counter.py` 内真实 `beam_multi_counter` 超过原十秒子进程期限；原日志 `/tmp/wings-beam-recovery-full-aot-debug-20261118.log` 仍保留。旧调用器只报告命令，清理时丢失输出和用例身份。本次没有该原进程的栈、输入指纹或阶段证据，不能确定是 10000 actor、其他合法用例或非法输入启动，也不能断言是此前 TCP 的动态加载停顿。后续专项成功不覆盖这条失败，启动稳定性仍未完成。

## 实际验证

- 七项真实受限子进程测试通过，日志 `/tmp/wings-multi-counter-evidence-unit-final-20261118.log`。包含 65537 B 输入回压、合法 0/1/2 协议退出、恰好 1 MiB 的 JSON 输出、stdout/stderr 越界提前清理、双流超时前缀及请求指纹、EOF 后不退出、坏 JSON/UTF-8/stderr、命令前缀截断和持管道后代清理。测试仅缩短墙钟期限，不放宽生产限制，不制造崩溃或 OOM。
- Debug/Release 分别重新编译当前 v2 业务产物，真实 Wings/OTP/Python 三端均通过 18 个字节状态/FIFO/退出用例、17 个非法桥接输入和 49 个非法 JSON。代表容量仍为 10000×256 B、1000×4 KiB、100×64 KiB；没有缩小原规模、放宽 actor 阶段或重试失败。
- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check` 通过；185 份 Markdown 相对链接/围栏和全部 Python 脚本语法通过。

原始专项日志为 `/tmp/wings-multi-counter-evidence-{debug,release}-20261118.log`，产物保留在 `target/multi-counter-evidence-{debug,release}-20261118/artifact/`。本轮只运行相关专项和必要工程检查，没有把前一切片的 Release 完整 AOT 成功标为本次宿主修改后的完整套件结果。Rust 实现未变，698 项既有 Rust 证据沿用，不伪造本轮新增执行次数。

## 复现

```sh
python3 scripts/verify_multi_counter_tests.py
cargo build --workspace --bins --examples --locked
python3 scripts/verify_multi_counter.py --compare-beam
cargo build --release --workspace --bins --examples --locked
WINGS_PROFILE=release python3 scripts/verify_multi_counter.py --compare-beam
```

新短入口加入既有双平台 CI，但未推送或执行远端 CI。本机检查及源/日志/产物指纹汇总在 `target/multi-counter-evidence-20261118/report.json`。所有失败信息只报告已观察字节与 EOF，前缀不是完整输出，也不把已写 stdin 当作远端消费。修正不自动采样原生栈、不重试、不解释启动阶段。完整资源账本、启动超时根因、过载/服务 I/O、最新原生双平台及 BEAM 性能/整机内存双门禁仍待完成，MVP 未完成。
