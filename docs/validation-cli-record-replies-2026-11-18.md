# 2026-11-18 CLI 具名记录输出验证

基于 `245731a` 按 [ADR-0117](adr/0117-streamed-cli-record-replies.md) 接入具名记录 JSON 回复。平台为本机 macOS AArch64；ABI v21、manifest v2、4352 B 执行控制、264 KiB adapter、16 MiB CLI 额度与所有原有时限不变。

## 行为与专项

具名记录输出为 `reply` 下的字段对象，字段按 schema 顺序，嵌套/空记录保留形状。String/Bytes/IntList/RecordInt 叶沿用原包装，不输出原堆 handle。固定 64 槽数组遍历已校验的 schema，直接写 writer；业务返回后才编码，不创建 JSON Value 树、完整字符串或载荷副本。固定控制大小回归通过，原 adapter 预算未提高。

真实 AOT 使用新临时产物，核对 i64 最小值和接近最大值的完整内容、GC 后嵌套回复及下一条 Read 的已提交值、空根记录、Unicode/引号/换行/反斜线。65535 B String 加 1 B Bytes 正好上限，65536 B NUL String 的转义输出和 65536 B Bytes 的数字数组均大于 64 KiB，核对完整内容及逐条换行。额外构建并执行 32 层记录。超界回复、业务除零和非法 JSON 参数均无成功输出，真实关闭输出管道返回正常 CLI 错误。writer 定向测试覆盖部分写入失败及 flush 失败。

共用 ReplyRejected 状态的文字原来仅描述列表 64 整数，本次改为列出 64 节点/整数、65536 字节或 schema 拒绝，保留原列表错误的 `exceeds 64` 兼容片段，不改变失败分类或候选提交规则。

## 验证结果

执行以下命令，检查与运行日志使用 `/tmp/wings-cli-record-` 前缀、`-20261118.log` 后缀：

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo build --locked -p wings-vm -p wings-compile --bins --examples
cargo build --release --locked -p wings-vm -p wings-compile --bins --examples
python3 scripts/verify_workspace.py --output target/workspace-validation/cli-record-debug-20261118
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/cli-record-release-20261118
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo +1.85.0 check --workspace --all-targets --locked
cargo +1.85.0 check -p wings-vm --all-targets --target x86_64-unknown-linux-gnu --locked
python3 scripts/verify_scalar_aot_tests.py
python3 scripts/verify_workspace_tests.py
```

check、clippy、fmt、两种构建、Rust 1.85 全目标与 Linux VM 全目标交叉检查通过；交叉检查没有运行 Linux。debug/release 的受限全目标各 97 个 harness、677 项通过，其中 VM 库 324 项、CLI 二进制 27 项；doctest 均通过。AOT/全量运行器自身分别 6/8 项通过。报告位于两个上述 workspace 目录的 `report.json`，检查/构建最终日志使用 `check-final`、`clippy-final`、`build-debug-final` 和 `build-release`。

完整 debug AOT 通过。完整 release AOT **未通过**：在既有 `verify_measurement.py` 的新编译 C probe 无参数启动处触发原 2 秒超时；日志为 `/tmp/wings-cli-record-aot-release-20261118.log`，没有删除失败或放宽时限。该位置先于新增 CLI 记录专项。另以原 CPU 20s/墙钟 30s/core 0 运行 release 的具名记录宿主示例及完整 CLI 专项，两者通过，日志为 `/tmp/wings-cli-record-aot-targeted-release-20261118.log`；这不替代失败的完整 release 入口。debug 最终专项为 `aot-targeted-final`，完整入口为 `aot-debug`。

## 内存 probe 启动失败取证

`measure_memory.c` 的 `main` 在无参数时第一条条件直接返回 2，既有文件在本次代码中未修改。独立诊断重新编译并保留原两秒限制：首个无参数启动仍超时、stdout/stderr 为空；之后六种非法参数的同文件启动耗时约 6.5–8.8ms，均正确返回 2。保留目录 `target/cli-record-probe-diagnostic-20261118/` 含 C probe、源码/可执行文件 SHA-256、构建命令、全部退出/输出/耗时和采样。

该独立诊断在启动 0.2s 后采样一秒，728 个样本中 727 个位于 `_dyld_start`，另一条为 dyld 的 Mach 消息调用，未观察到 C `main`。采样本身会增加扰动，不能据此证明原始超时具有完全相同原因，也不能定位到具体系统服务。原始全量失败与诊断失败均继续保留；后续热启动成功不抵消失败，本轮不声明 release 全量 AOT 已通过。

## 边界

JSON 输入仍仅 Int/Bool/Unit，未开放记录/字符串/列表/字节参数；这不改变语言和类型化宿主/跨线程入口的已实现能力。输出失败不回滚已经提交的 state。完整分配/装载账本、长时公平/过载/并发故障、最新原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。不推送或发布。
