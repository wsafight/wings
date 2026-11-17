# 2026-11-17 字符串内容比较验证

按 [ADR-0111](adr/0111-bounded-string-content-equality.md) 开放 String `==`/`!=`，比较完整 UTF-8 编码而非引用身份。基于 `d6560eb`；源码 ABI v21，旧产物须重编译，原容量、4352 B 执行控制及 CPU/墙钟/业务时限不变。

## 实际结果

本机 macOS AArch64、M3 Pro、36 GiB、Rust 1.99.1，执行：

```sh
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
python3 scripts/verify_workspace_tests.py
python3 scripts/verify_workspace.py --output target/workspace-validation/string-equality-debug-20261117
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/string-equality-release-20261117
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
```

- check/clippy/fmt/MSRV 和 Linux x86-64 交叉检查通过；交叉检查不是原生 Linux 执行。运行器自身 8 项测试通过。
- debug/release 各实际运行 94 个 harness、653 项，其中 VM 库 309 项；失败/忽略/过滤均为 0。各目录保留清单、指纹、原始日志和报告；每项仍受 CPU 20s/墙钟 30s/core 0 限制。两种 profile 的 doctest 均实际为 0 项，不计作业务覆盖。
- 新增 6 项 Rust 回归覆盖语义、IR、lowering、codegen 与 runtime。空/Unicode/NUL、独立同内容对象、首尾差异、不同长度、255/256/257/65536 字节及不规范化文本符合契约。每块触发 GC 后两末用输入仍存活；比较不新增业务存储，错类型/陈旧引用/预算/取消失败不改输出。
- 新 AOT 示例 debug/release 各连续 5 轮通过，CLI 精确 Bool 输出通过；同步长度边界含 65535，原栈逐片恢复、同引用中途取消/预算失败、单 worker 与一/双 Scheduler、消息合计 64 KiB、候选状态失败和末 owner 清理均通过。业务阶段仍为原 3 秒。
- 完整 debug AOT 通过。完整 release 首轮在 TCP 首个正常场景的原 5 秒启动等待超时，原限顺序复验完整通过；两次日志均保留，复验不等于根因解决。字符串专项没有失败。

最终日志为 `/tmp/wings-string-equality-{check-final,clippy-final,msrv-final,linux-check-final,fmt,tests-debug,tests-release,doctests-debug,doctests-release,build-debug-final,build-release,runner-tests,aot-repeat,aot-debug,aot-release,aot-release-repeat}.log`。

## 开发失败与边界

首次 check 因 BinaryOperator 未实现 PartialEq 失败，改用模式匹配；第二次因 worker 请求使用错误的结果查询接口及 execution 借用期间移动 process 失败，改为 `take_reply` 并先释放 execution。分别保留 `check-first`、`check-second` 日志。历史 String 相等负例按新契约改为排序负例，没有删除类型边界检查。

首次 release TCP 超时属于既有间歇失败，本轮不归因于 String，也不放宽超时或隐去失败。最新原生 Linux、CPU 硬配额和同平台同负载 BEAM 吞吐/整机内存双门禁仍缺证据；具名记录传输、完整资源账本与公平/过载/故障门禁未完成，不能宣布 MVP 完成。
