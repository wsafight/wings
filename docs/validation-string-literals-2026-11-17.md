# 2026-11-17 字符串字面量验证

## 范围与基线

按 [ADR-0104](adr/0104-bounded-string-literals.md) 补 MVP 必需业务值，基于 `3ec0a72`（String 值切片），分支 `feat/mvp`。支持双引号 UTF-8 与六种转义、单值 64 KiB/IR 合计 1 MiB；代码只读常量与进程独立计费副本分开。ABI v17，旧产物须重编译，context 88 B/EffectRequest 72 B、4352 B 控制预留和原资源上限保持。用户 `docs/proposals/` 不读取为需求、不改动、不提交。

本机 macOS 26.6.2 AArch64、Apple M3 Pro、36 GiB，Rust/Cargo 1.99.1，MSRV 1.85.0。测试通过独立 OS 子进程限制 CPU 20s、墙钟 30s、core 0，真实 fixture 业务阶段 3s，TCP 启动等待 5s；不将 worker 数量当 CPU 硬配额。无可用最新原生 Linux x86-64 门禁结果。

## 新增检查

- lossless token/容错解析、中文/emoji/NUL 和六种转义；未知转义、控制字符、未闭合/跨行恢复；按解码字节而非字符/源码长度检查 64 KiB。
- lowering 常量不逐字节生成 IR，跨帧根继续准确；独立 IR 校验类型/单值/全程序合计，原生只读数据复制不占大受管栈数组。
- runtime 对静态 UTF-8 分块校验/复制，保留精确 String 标签，GC/陈旧引用与三级回滚；在暂存复制中途取消/耗尽预算不写 output、不泄漏票据。
- LSP 使用 UTF-16 位置验证带中文/emoji 源码的推断 String 悬停，以及错误转义诊断，不执行业务代码。
- 真实 AOT 在 initializer、fn 和 handler 使用文字及转义，跨帧 GC、活栈让出/取消、一/双 worker 与失败清理保持；生成 0/65536 字节 ASCII/emoji、跨块中文及超限拒绝。

## 构建与验证命令

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

`--no-run` 只生成清单，实际 harness 须独立受限执行。单独 fixture 同样通过 `verify_scalar_aot.command` 调用，原 CPU/墙钟/阶段不放宽。提交仅保存当前阶段，不推送或发布，不将源码提交当成验收通过。

## 本机结果

- workspace check、all-targets clippy `-D warnings`、fmt 与 Rust 1.85.0 all-targets check 均通过；LSP/VM 依赖隔离和 corosensei 禁用 unwind 检查通过。doc-test 实际 0 项。
- debug/release 分别实际执行当前 90 个独立 harness，各通过 615 项、失败 0；其中 VM 各 284 项。清单由本次 Cargo JSON 产物去重生成，不把历史可执行文件当成本次测试。
- 字面量专项真实 AOT 在 debug/release 各重复 5 次，均通过 direct、单/双 worker、GC/取消与失败清理。release 另通过空值、65536 B ASCII、16384 个 emoji、跨 256 B 边界中文/emoji 和转义内容；65537 B ASCII 与 16385 个 emoji 在生成产物前拒绝。
- 匹配 ABI v17 且运行期间保持源码/脚本/二进制不变的完整 AOT，debug/release 各一次全部通过，包含 TCP、Bytes 消息、宿主值、bootstrap 计费及 record/list/bytes 回复。原 CPU 20s、墙钟 30s、业务阶段 3s 与 TCP 启动 5s 均未放宽。较早失败仍按下一节保留，不由复验通过抹除。
- 机器可读环境、结果与 SHA-256 见 [string-literals-macos-2026-11-17.json](string-literals-macos-2026-11-17.json)。该结果仅覆盖本机，不是最新原生 Linux 或 BEAM 双门禁。

## 失败记录

原 String 提交的全量 AOT 曾有原墙钟/启动失败，历史记录保留。本轮第一次重新验证旧基线时，没有冻结 Python 检查脚本就同时编辑 ABI 断言，子检查因 v16 产物/v17 脚本错配失败；这两次基线运行无效，不能算旧代码回归或新实现失败，日志 `/tmp/wings-mvp-baseline-aot-{debug,release}.log` 保留。修正方法是构建和验证匹配的源码/脚本，并在运行期间保持其不变，而不是放宽断言。

初次专项遍历所有 harness，即便过滤后 0 项测试的旧 `scalar_supervisor` 二进制也在原 30s 墙钟超时；保留 `/tmp/wings-literal-focused-debug.log`。实际专项测试与后续完整执行分别统计，超时原因未确定，不归因于主机或新 String 操作，不改限额。

## MVP 边界

String 消息/回复、通用记录、完整启动/装载/可增长分配账本，长时公平/过载/故障恢复、最新原生双平台与同机同负载 BEAM 吞吐/整机内存双门禁仍未完成。字面量不是通用业务值或完整内存隔离；保留所有失败证据，不以功能单测替代采用价值门禁。
