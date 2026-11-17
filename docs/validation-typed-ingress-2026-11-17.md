# 2026-11-17 类型化跨线程入口验证

按 [ADR-0115](adr/0115-typed-cross-thread-ingress.md) 为固定 ingress 增加 MessageValue 内容复制，基于 `0cee108`。原标量方法仍拒绝原引用或内部段编码；ABI v21、4352 B 调用控制、原容量、CPU 20s/墙钟 30s/core 0 和每业务阶段 3s 不变。

## 验证命令

本机 macOS AArch64、M3 Pro、36 GiB、Rust 1.99.1，执行：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_workspace.py --output target/workspace-validation/typed-ingress-debug-complete-20261117
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/typed-ingress-release-complete-20261117
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 -u scripts/verify_scalar_aot.py
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_workspace_tests.py
```

check/clippy/fmt/MSRV 与 Linux VM 交叉检查通过；后者不是原生 Linux 执行。最终 debug/release 全目标各 96 个 harness、665 项，失败/忽略/过滤 0，其中 VM 库 318 项；doctest 各 0 项。新真实 AOT 专项 debug/release 各连续 5 轮通过，包含目标导入失败和挂起停止；运行器自身 6 项和 8 项通过。依赖隔离和 corosensei 无 unwind 特性检查通过。

## 行为覆盖

- 新增 4 项 ingress 回归：String/Bytes/RecordInt/列表/Bool/Unit 混合，64 整数及 String/Bytes 合计 65536 B 含界、超界和严格类型；源修改不改变副本。process/tenant/VM 任一级满额拒绝且不发布、不改账目；满/关闭清理，WakeFailed 保留已接受副本。
- timeout/cancel/Drop 消费调用者后，原 publisher 仍占结果槽和请求票据，不能用同槽发布新请求；实际队列消息释放后才恢复基线，不重试或撤回消息。
- `examples/typed-ingress.wings` 及宿主 `typed_ingress` 在真实发送线程构造 Unicode/NUL、65528 B Bytes、64 整数和其他叶值，发送成功后修改源。目标按单步时间片导入，GC 后仍提交 3065539；请求正常回复、超时、取消和 Drop 均实际执行原消息。
- 无堆容量导入失败保持旧提交 0；导入中停止清理载荷/结果 publisher，未执行业务。旧 sender 拒绝，末 sender/worker 释放后回到同一账户基线；结果不持 actor/代码 owner。
- 原 RecordInt/IntList 消息 AOT 示例改为注册后检查原标量方法拒绝 raw handle/段编码；保留类型边界及拒绝零变化，未删除原失败语义。

## 失败证据

最初补导入失败 fixture 错写不存在的 WorkerEvent::Failed，check 失败后改为已有 Exited 事件，见 `/tmp/wings-typed-ingress-check-final.log`。首次全量两种 profile 各 96/665 通过，但首次完整 debug AOT 在旧 `record_messages` 的“引用签名 ingress 注册必须失败”断言处退出；这是本次显式开放注册后失效的历史契约，不是原标量传引用应成功。保留 `/tmp/wings-typed-ingress-aot-debug.log`，没有放宽限额；修改记录和列表示例后用全新全目标目录重验。

日志前缀 `/tmp/wings-typed-ingress-`，静态/构建最终日志为 `check-complete2`、`clippy-complete2`、`msrv-complete2`、`linux-check-complete2`、`build-{debug,release}-complete2`；全目标为 `tests-{debug,release}-complete`，完整 AOT 为 `aot-{debug,release}-complete`，另有 `repeat`、`doctests-{debug,release}`、`aot-runner`、`workspace-runner`、`doc-links`，均为 `.log`。旧失败和首轮目录不覆盖。

## 验收边界

本轮不提高队列或消息/请求额度，不引入额外线程/通道，不改变 OS 通知、请求双端租约或取消检查。宿主栈/原始切片仍由调用方管理，固定上界复制不是受管时间片；目标业务导入仍受原预算和精确根约束。具名记录回复、全部分配/装载账本、长时公平/并发故障、最新原生双平台和同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 状态不变。
