# 2026-11-18 具名记录回复验证

按 [ADR-0116](adr/0116-bounded-named-record-replies.md) 接入具名记录 handler 回复及完成事件，基于 `c1d0da4`。ABI v21、4352 B 调用预留、执行/队列限额及既有 CPU/墙钟限制不变；manifest format 改为 v2，旧产物须重编译。

## 本机全量验证

本机 macOS AArch64 执行：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_workspace.py --output target/workspace-validation/named-replies-debug-verified-20261118
cargo test --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/named-replies-release-20261118
cargo test --release --workspace --doc --locked
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_workspace_tests.py
```

debug/release 各通过 97 个 harness、675 项测试，VM 库各 324 项；全部 test target 由固定清单及 executable 指纹验证器实际执行，子进程保留 CPU 20 秒、墙钟 30 秒与 core 0 限制。两种 profile 的 doctest 和完整真实 AOT 通过。check、clippy、fmt、Rust 1.85 全目标检查及该版本 Linux x86-64 VM 全目标交叉检查通过；AOT 运行器自测 6 项、workspace 运行器自测 8 项通过。交叉检查没有执行 Linux 原生代码。

全量报告位于 `target/workspace-validation/named-replies-debug-verified-20261118/report.json` 和 `target/workspace-validation/named-replies-release-20261118/report.json`。命令日志前缀为 `/tmp/wings-named-replies-`，日期后缀为 `-20261118.log`；最终 clippy 使用 `clippy-verified`，debug 全量使用 `tests-debug-verified`，完整 AOT 使用 `aot-debug`/`aot-release`。未覆盖旧失败目录。

真实 AOT 已验证嵌套记录、空记录、全部叶值、GC 后回复、独立内容及结果 clone，单 worker 与 1/2-worker Scheduler 的 success/timeout/cancel/failure 完成重建。String 65535 B 加 Bytes 1 B 正好上限，65536 B 加 1 B 拒绝；worker 保留旧提交 1 并报告 ReplyRejected。非成功完成读取全部默认叶，标量为零、列表/字节/String 为空，不携带真实回复。还覆盖持久记录满堆时 completion 导入失败、非法 handle/错布局拒绝、三级额度、schema 副本末 clone、名义匹配和伪造 IR completion；原 3 秒业务阶段不变。

最终专项先连续通过五轮，日志为 `/tmp/wings-named-replies-aot-targeted-20261118.log`。手动构建及执行方式为：

```sh
target/debug/wings-compile build examples/named-record-replies.wings -o /tmp/wings-named-replies-artifact
target/debug/wings-compile build examples/named-record-reply-state.wings -o /tmp/wings-named-replies-state-artifact
target/debug/examples/named_record_replies --trusted /tmp/wings-named-replies-artifact /tmp/wings-named-replies-state-artifact
```

自动验证使用受限子进程及新临时产物目录；此手动命令仅说明两个产物参数，不替代受限全量入口。

## 保留的失败

接续编译首先定位并修正 `tcp_counter` 的重复 match arm 括号。产物测试发现格式从 v1 升至 v2 后，重复 JSON 字段 fixture 仍硬编码 v1；修正 fixture 后 31 项通过。编译器测试发现旧语义层尚未允许记录型完成签名，并有一个“具名记录回复必须拒绝”的历史断言；修正后 31 项 lowering 与 15 项编译器集成测试通过。

首次 Scheduler fixture 在目标完成之前发送 Read，读到合法旧提交 10；改为在原 3 秒总 deadline 内逐次查询并只允许旧值或预期完成值，不假设跨 actor 全序。取消用自请求确保发生在目标开始前，避免要求跨 worker 竞争必定 Cancelled。后续 fixture 两次误用条件分支内的 let 和函数尾表达式换行，静态构建拒绝；改为已有 fn/let/单表达式语法，不扩展语法。

首轮全量 `named-replies-debug-20261118` 在语义历史断言失败。后续 `named-replies-debug-final-20261118` 的 VM 库有四项旧 Bytes/String completion 回归失败：新增 payload 校验错误拒绝旧 reply。修正为允许原字节回复 mask，并拒绝同时持有两种回复；旧回归与新增记录路径均已通过。`named-replies-debug-complete-20261118` 曾通过 674 项，随后增加非法 handle/错布局回归，以最终 675 项报告为准。初次及中间 clippy 的递归专用参数和惰性闭包警告已修正，失败日志继续保留。

## 验收边界

结果独立复制，语言完成事件按名义 schema 在接收方重建，不传原 handle。JSON CLI 仍限既有参数和回复；宿主通过 `RecordReply` 获取 schema 与叶值。取消和超时不撤回已发布消息，失败不提交候选状态。当前未取得本切片最新原生 Linux、原生双平台 CI 和各平台同机同负载 BEAM 吞吐/整机内存双门禁；完整分配/装载账本、长时公平/过载/并发故障仍未闭环，MVP 未完成。不推送或发布。
