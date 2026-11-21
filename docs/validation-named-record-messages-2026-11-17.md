# 2026-11-17 具名记录消息参数验证

按 [ADR-0113](adr/0113-bounded-named-record-message-expansion.md) 将源码具名记录参数有界展开为既有叶值，目标在业务前重建独立记录，基于 `e5be219`。不改 ABI v21、4352 B 控制预留、原 CPU 20s/墙钟 30s/core 0/业务阶段 3s 和全部运行时容量。

## 命令与结果

本机 macOS AArch64、M3 Pro、36 GiB、Rust 1.99.1，执行：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_workspace.py --output target/workspace-validation/named-messages-debug-complete-20261117
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/named-messages-release-complete-20261117
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_workspace_tests.py
WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
```

- check/clippy/fmt/MSRV 及 Linux VM 交叉检查通过；交叉检查不是原生执行。新代码只在 sema/lowering，VM 二进制受同一 ABI 约束，不新增依赖。
- debug/release 各实际执行 95 个 harness、656 项，失败/忽略/过滤 0，其中 VM 库仍为 309 项；新增 3 项编译/类型回归覆盖叶签名、独立构造、单次实参调用、空记录/合计 64 节点边界、32 层重复子树快速拒绝、名义类型及记录回复拒绝。doctest 实际各 0 项。
- 新 AOT 示例最终版本 debug/release 各连续 5 轮通过；完整 AOT 两种 profile 首轮均通过，含原 TCP 所有场景。运行器自身分别 6 项和 8 项通过。
- 真实生成代码覆盖 String/Bool/列表/Bytes/RecordInt/空记录/Unit 混合嵌套，分块 GC/逐片恢复、65535 B String 加 1 B Bytes 正好上限、源共享局部与跨 worker 独立重建。语言 send_self/send/request、宿主清单叶值、一/双 Scheduler 和单 worker 均实际执行。
- 源发布后除零失败保留旧提交 65554、子 actor 仍执行已发布消息；宿主取消已发布请求仍执行目标并提交 276。66 元素列表在发布前拒绝且账目不变；超出字节合计受管 SendRejected。单/双 worker 的目标堆不足在业务前失败、保留提交 0；挂起导入后停止及末 owner 清理均回到原基线。Scheduler 两次异步请求使用不同结果，避免误把旧完成 state 当新请求完成。

最终日志 `/tmp/wings-named-messages-{check-complete,clippy-complete,msrv-complete,linux-check-final,build-debug-complete,build-release-complete,tests-debug-complete,tests-release-complete,doctests-debug,doctests-release,aot-runner-tests,workspace-runner-tests,aot-final-repeat,aot-debug,aot-release}.log`；全目标目录保留原始输出、指纹与报告。

## 保留的失败

最初 lowering 专项因测试源码缺函数括号、请求表达式未以分号丢弃而失败，修正 fixture 后通过；最初 sema 专项同样有请求尾表达式错误，均保留 `lower-first`、`sema` 日志。初次示例构建错误地对单 worker 非 Option 的 execution 快照调用 unwrap，修正后编译通过，见 `build-debug` 日志。

首次全目标 debug 执行 16/95 harness 后停止：新参数数目诊断不含历史断言要求的 `too many parameters`，29 项通过、1 项失败；没有删除断言，恢复兼容诊断后使用全新目录重跑。失败目录 `target/workspace-validation/named-messages-debug-20261117` 和 `/tmp/wings-named-messages-tests-debug.log` 不覆盖。其他首次/中间专项不替代最终冻结版本的回归。

## 尚未完成

清单参数名为展开后的按位置叶名，宿主须按清单传值；源码 LSP 仍展示名义记录。不支持具名记录回复、JSON 对象或跨进程原引用。全部分配/装载账本、公平/过载/并发故障、最新原生双平台和各平台同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 状态不变。不推送或发布。
