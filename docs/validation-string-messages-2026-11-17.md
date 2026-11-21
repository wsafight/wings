# 2026-11-17 String 消息参数验证

## 范围

基于 `9554361` 的 `feat/mvp`，按 [ADR-0105](adr/0105-accounted-string-messages.md) 开放语言 send_self/send/request 的 String 参数与宿主 `MessageValue::String(&str)`。String/Bytes 共用单条合计 65536 编码字节，独立类型掩码和目标计费副本；不开放 String 回复、JSON 引用参数或通用记录，不读取/改动/提交用户 `docs/proposals/`。

源码 ABI v18，EffectRequest 80 B、context 88 B，执行控制预留仍 4352 B；旧产物须重编译，配额和 quantum 不提高。本机 macOS 26.6.2/Darwin 25.6.0 AArch64、Apple M3 Pro、36 GiB、Rust/Cargo 1.99.1，MSRV 1.85.0。

## 检查内容

- 前端/lowering/IR/manifest 保留 String 参数类型；String 回复/完成值继续拒绝，Bytes/裸整数不冒充 String。ABI 布局、独立掩码重叠/越界与固定控制预留有断言。
- 先检查合计长度再复制；空值、中文/emoji/NUL、跨 256 B 边界、65536 B ASCII/emoji 与重复引用超限；源缓冲修改、源堆/actor 退出不改变已复制内容。
- 导入混合 String/Bytes/RecordInt/IntList，逐块检查和入口根保活；无效 UTF-8、预算/取消以及 process/tenant/VM 三级额度拒绝后清理守恒。
- 真实 AOT 的单 worker 与一/双 worker Scheduler 验证宿主/语言消息、完整内容回复为 Bytes、原生 String 请求输入及标量完成事件。显式返回 Bytes 不称 String 回复。
- 超时/取消后原消息仍执行，发布方失败不撤回已送达子 actor 的文字，停止/末结果 handle 归还计费；不把旧状态 handle 当文本快照。

## 命令与限制

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

Cargo `--no-run` 只生成当次 executable 清单，实际测试逐 harness 独立 OS 子进程执行。测试/真实 AOT 子进程原限 CPU 20s、墙钟 30s、core 0，fixture 每业务阶段 3s、TCP 启动等待 5s，不放宽。完整 AOT 启动后保持参与构建源码、脚本与二进制不变；worker 数不是 CPU 硬配额。

## 开发中失败

初次 check 因新增 MessageValue 后测试 match 未覆盖 String 而失败；随后 manifest 测试错误地比较没有 PartialEq 的整个 Manifest，再改为精确字段断言。这些是实现/测试编译错误，不归因为平台。

初次专项运行 VM 286 项通过、1 项失败：新 String 缺堆检查前已耗尽旧测试的单步预算，得到 BudgetExhausted 而非预期 InvalidRoot；在该分支明确重置测试预算后原断言通过，未改 runtime 检查顺序。日志 `/tmp/wings-string-msg-focused.log` 保留。

初次真实 AOT 源 fixture 将 `+` 放在新行开头，违反既有换行分句语法，编译报 S003/T005，未运行 VM；改为操作符后换行。日志 `/tmp/wings-string-msg-aot-focused-debug.log` 保留。后续专项三轮及补充请求/失败断言后的五轮分别记录，不覆盖失败日志。

首次全量执行暴露 LSP 旧测试仍要求拒绝 String 参数，已按新契约改为接受参数、继续拒绝 String 回复；实现未绕过共用分析链。该失败以及 debug `execution_control`/release `build_aot_records` 零测试 harness 的原 30s 启动超时均保留于 `/tmp/wings-string-msg-tests-{debug,release}.log`。debug 完整 AOT 的 `worker_wakeup --trusted` 也在原 30s 墙钟超时；不能称首次全量通过。独立抽样另一个启动中的零测试 `worker_ingress`，只见 `_dyld_start + 0`、96 KiB footprint；这仅定位该样本尚未进入业务，不能推断所有超时的原因，未关闭系统保护或延长时限。

修正 LSP 后先单包构建/执行，debug/release 各 40 项通过；随后误复用旧 workspace JSON 的 executable 路径（单包与 workspace 的 feature 组合产物名不同），再次运行了旧协议测试，日志 `/tmp/wings-string-msg-tests-recheck.log` 不作为修正后基线。重新生成当前 workspace 清单并实际执行，debug/release 各 90 个 harness、620 项、失败 0；两个启动超时 harness 单独原限复验也均通过。这些复验不抹除首次失败或证明超时根因。

## 已完成结果

- workspace all-targets check、clippy `-D warnings`、fmt 和 Rust 1.85.0 all-targets check 均通过；LSP/VM 依赖边界及 corosensei 禁用 unwind 检查通过，doc-test 实际 0 项。
- 当前 workspace 清单的 debug/release 各 620 项通过（VM 各 288 项），新增 5 项；不使用 `--no-run` 数量代替实际测试。
- String 专项真实 AOT debug/release 各连续 5 轮通过，含单 worker/一双 worker Scheduler、最大编码长度、混合载荷、GC/预算/取消、请求输入、超时不撤回和发布后失败清理。release 完整 AOT 一次通过，debug 首轮失败按前节保留；复验结果单列。
- debug 完整 AOT 按原限重跑通过，覆盖 TCP、单/多 worker、OS 唤醒、各类型消息/回复和清理；首次失败仍未归因。机器可读环境、测试、失败与源码/日志 SHA-256 见 [string-messages-macos-2026-11-17.json](string-messages-macos-2026-11-17.json)。

## 未完成门禁

String 回复、通用记录、全部可增长分配/启动/装载资源账本、长时公平/过载/并发故障恢复、最新原生 Linux x86-64，以及双平台各自同机同负载 BEAM 吞吐/整机内存双门禁仍未完成。String 参数切片不等于 MVP 完成，不推送或发布。
