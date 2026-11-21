# 2026-11-17 String 回复验证

## 范围与限制

基于 `5c5a980`、分支 `feat/mvp`，按 [ADR-0106](adr/0106-accounted-string-replies.md) 开放 String 内容回复、独立 StringReply/ReplyString/CompleteString、匹配的系统完成事件及 CLI 结构化文字输出。源码 ABI v19，EffectRequest 80 B/context 88 B/控制预留 4352 B 保持；旧产物重编译，不改配额或 quantum，用户 `docs/proposals/` 不读取/修改/提交。

本机 macOS 26.6.2、Darwin 25.6.0、AArch64 Apple M3 Pro、36 GiB、Rust/Cargo 1.99.1，MSRV 1.85.0。实际测试/AOT 独立 OS 子进程原限 CPU 20s、墙钟 30s、core 0，业务阶段 3s、TCP 启动 5s。worker 数不是 CPU 硬配额；无可用最新原生 Linux x86-64 结果。

## 验证内容

- 语义/IR/清单/LSP 接受 String 回复及精确匹配完成类型，继续拒绝 Bytes/String 错配和裸整数 handle；CLI 通过 serde 输出中文/emoji/NUL/转义，不接收 JSON 引用参数。
- 结果构造分块校验 UTF-8，BytesReply 拒绝 String 掩码，独立 StringReply 保持只读 str 不变量；clone 共享计费结果、源 actor/模块退出不破坏内容，末 owner 归还数据和控制租约。
- 出口候选 state/结果精确根、GC/逐片恢复、预算/取消/Drop、三级数据/结果控制拒绝回滚与旧提交保留；无效/截断 UTF-8 和跨 256 B 字符边界。
- 真实 AOT direct、单 worker、单/双 worker Scheduler、宿主 ingress/监督、成功/超时/取消/失败 72 组语言完成及目标/owner 独立生命周期。失败完成导入有效空 String，满完成队列保留结果或 owner 退出清理；不撤回已发布请求。

## 命令

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

`--no-run` 仅生成清单，实际 harness 逐个受限运行。完整 AOT 运行时保持源码/脚本/二进制不变，失败完整保留。

## 开发失败与修正

首次批量移除旧 String 拒绝条件时补丁截断条件，fmt 发现多余 delimiter；精确删除残段后继续。新增枚举变体要求旧示例 match 覆盖 String；补独立错误分支，不吞掉新结果。扩展 worker 测试时漏写 ValueType 路径，check 失败后修正；这些是实现编译错误，不归因为平台。

首轮真实 AOT 的导入失败 fixture 沿用 Bytes 的 1 个堆槽，但 String 初始回复需要 Bytes→String 的第二槽，导致 target 提前失败，最近 exit 不是 owner。fixture 固定为恰好 2 槽：足够 target 构造回复，而 owner 完成导入后再构造候选时仍按原容量受管失败；没有提高 VM/业务配额或延长时限。日志 `/tmp/wings-string-reply-aot-focused-debug.log` 保留；修正后 debug 五轮全部通过 72 组，CLI Unicode 输出正确。

扩展 VM 单测首轮 289 项通过、4 项失败：两处满队列断言仍写死 2 B 而 Unicode 结果为 7 B；String 陈旧根错分为 InvalidOutput；出口多一段 UTF-8 校验后预算边界仍沿用 Bytes。修正为真实长度/InvalidRoot/770 步边界，不调整 runtime 原限，后续 293 项通过。失败日志 `/tmp/wings-string-reply-unit.log` 与通过日志 `/tmp/wings-string-reply-unit-2.log` 分开保留。

## 完整回归的启动失败

完整 AOT 首轮 debug 在 `list_messages --trusted` 达到原 30s 墙钟，release 在 `module_accounting --trusted` 达到原 30s；首次复验 debug 在 `host_values --trusted` 达到原 30s，release 在 TCP 第一个正常场景的原 5s 启动等待失败。四份日志分别保留，不合并成通过结果，不延长期限或禁用系统检查。

另对 release 零测试 `loop_execution` harness 的一次启动作独立 sample，观察到 96 KiB physical footprint 和 `_dyld_start + 0`，尚无业务栈。它只描述该样本，不能将所有超时归因于 dyld、宿主负载或 runtime。其他测试仍按原限继续。

第二次完整复验 debug 的 `verify_fairness.py` 报告 `beam_fairness --trusted ... 10` 达到它自己的原 10s 墙钟，退出 3；完整 release 复验通过。此时没有并行运行全量 harness，仍不能据此断定启动失败根因；debug 再独立原限复验，保留每次日志。

## 已完成结果

- workspace all-targets check、clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 均通过；debug/release bins/examples 构建通过。
- 当前 workspace executable 清单实际执行：debug/release 各 91 个 harness、625 项通过、失败 0，VM 各 293 项；doc-test 实际 0 项。没有用 `--no-run` 代替测试。
- String 回复专项真实 AOT debug/release 各连续 5 轮通过，每轮 72 组语言完成事件；release CLI 完整核对 0/1/255/256/257/65536 B 输出，另核对中文/emoji/NUL/转义。
- LSP/VM 依赖边界、corosensei 禁用 unwind、Python 语法、变更 Markdown 相对链接/围栏和 `git diff --check` 通过。完整 AOT 结果按上一节逐次保留，不把局部通过称作双平台或性能门禁。
- debug 第三次独立原限复验完整通过；release 第二次复验完整通过。两者均包含 TCP、OS 唤醒、各类型消息/回复、GC、恢复与清理。此前五次失败完整保留，根因仍未确定；机器记录见 [string-replies-macos-2026-11-17.json](string-replies-macos-2026-11-17.json)。

## MVP 边界

字符串交互不是通用业务值/完整账本。通用记录、全部可增长分配/启动/装载计费、长时公平/过载/并发故障恢复、最新原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，不宣布 MVP 完成，不推送或发布。
