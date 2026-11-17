# 2026-11-17 UTF-8 字符串值验证

## 范围

本轮按 MVP 第 4 项补业务值，不继续非必要优化。此前锁统计已提交为 `8a25764`（`feat/mvp`），本轮字符串代码基于该提交开发；未推送、未发布，用户原有 `docs/proposals/` 不改动、不纳入提交。

契约见 [ADR-0103](adr/0103-accounted-utf8-string-values.md)：独立 String/StringRef 与叶标签，0..=65536 编码字节，UTF-8 校验、Unicode scalar value 计数、显式 Bytes 复制转换和拼接。局部/state/fn 与原 GC/让出/取消相接；没有字面量、字符串消息/回复、通用记录或内容比较。ABI v16，旧产物须重编译，context/EffectRequest 布局、4352 字节控制预留、原资源额度及测试时限不变。

## 环境和方法

本机原生 macOS 26.6.2 AArch64（Apple M3 Pro、36 GiB），Rust/Cargo 1.99.1，MSRV 1.85.0。执行由 `scripts/verify_scalar_aot.py` 的 `command` 在独立 OS 子进程限制 CPU 20 秒、墙钟 30 秒、core 0；真实 fixture 各业务阶段原 3 秒。单/双 worker 不是 CPU 硬配额，不用本机结果替代原生 Linux x86-64 或 BEAM 性能验收。

源码与日志哈希、实际测试汇总见[机器可读证据](string-values-macos-2026-11-17.json)。`cargo test --no-run` 只生成清单；必须逐个实际执行 harness，不能据清单宣称测试通过。

## 新增回归

- ABI 保证 StringRef 是精确引用、拒绝所有裸整数，新增错误码/操作号与固定布局核对。
- 语义/降低、独立 IR 与清单分别拒绝 String 消息、回复和伪造 Bytes/String 操作；覆盖跨帧/出口根。
- 单文件 LSP 与前端共用 String 诊断、悬停及四项内建的精确补全签名，不执行业务代码。
- 空、ASCII/NUL、中文、四字节字符、组合字符、跨 256 字节边界、64 KiB；非法 continuation、overlong、surrogate、超 Unicode 范围和末尾截断。
- String/Bytes 独立标签与复制存储、GC 后陈旧引用拒绝；拼接双根、精确 64 KiB 接受与超限拒绝。
- 转换/计数/拼接的总预算和取消、未发布暂存释放，以及 process/tenant/VM 各层额度少一拒绝与回滚。
- runtime 回归保留失败前已提交 String 根，不让已写入失败输出替换旧状态；最后拥有者释放全部预留。
- 真实 AOT 的 String state、有界 fn、分支/while 合流、分块 GC/小片让出、显式取消/丢弃挂起、一/双 worker 与类型化失败退出，最终回到 168 字节存活根基线。

## 可重复命令

```sh
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo test --workspace --doc --locked
```

实际 harness 执行仍须加上述 OS 子进程限制。单独运行真实 fixture 时先构建 `examples/string-values.wings`，再用 `verify_scalar_aot.command` 调用 `target/<profile>/examples/string_values --trusted <artifact>`，不在开发宿主直接无约束运行失败注入。

## 开发时失败

初次 fixture 编译误用不存在的 SchedulerSnapshot.state，已改为终态退出检查；另补 runtime 单测直接核对失败后旧提交根。首次真实 fixture 将一次 Clear 内 GC 当作已释放旧提交根，实际该根在 handler 返回前必须保活；增加后续独立 Sweep 再核对一个存活 String 槽，未改变 GC 契约。首次全量单测仍有 ABI v15 的旧断言，已更新为 v16 并追加新操作/状态断言；不把初次失败算作通过。

## 实际结果与未通过项

- workspace check、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check、依赖隔离/禁 unwind 检查通过。workspace doc-test 检查通过，实际测试数为 0。
- debug/release 当前唯一的 **90 个 harness、606 项测试各通过**，比基线新增 12 项。串行日志同时保留修改 LSP 前的三个旧二进制，原始执行计 93/644；按最新 target 唯一去重后才得到 90/606，不把重复旧协议测试计入新增成果。最后扩展预算边界后，两种构建各重新执行完整 VM harness，281 项均通过。
- 真实字符串 AOT fixture 两种 profile 各独立重复五次，均通过直接调用、单/双 worker、Unicode/GC/分支/循环/取消/失败和回收；共十次 fixture、二十组 Scheduler worker 配置。CLI 两种构建各验证正常结果、失败前已输出回复保留、非法 UTF-8 及旧 ABI v15 产物拒绝。另有开发期 debug 五次通过，不重复计入最终十次。
- **完整 AOT 未通过**：首次 debug/release 并行运行各在原 `module_accounting` 超过 30 秒；改为串行、保留原限制后，debug 在 `byte_messages` 超过 30 秒，release 在 TCP 场景五秒等待内未观察到服务启动。不能将其归因为新 String、OS 策略或主机负载，原因未确定；也不能用字符串专项通过替代全量 AOT 门禁。
- 首次全量测试在旧 ABI 断言失败；更新后第一次全量执行在既有 `multi_worker` harness 超过原 30 秒。随后原限制串行执行全部 harness 通过；保留超时与通过两份日志，不放宽时限、不关闭检查点或故障路径。

最终日志使用 `/tmp/wings-string-{check,clippy,fmt,msrv}-complete.log`、`/tmp/wings-string-dependencies.log`、`/tmp/wings-string-doctests.log`、`/tmp/wings-string-tests-serial.log`、`/tmp/wings-string-test-summary.json`、`/tmp/wings-string-vm-latest-tests.log`、`/tmp/wings-string-lsp-tests.log` 和 `/tmp/wings-string-aot-focused-final.log`；失败完整 AOT 日志为 `/tmp/wings-string-aot-{debug,release}.log` 与 `/tmp/wings-string-aot-{debug,release}-serial.log`。所有参与构建的源码及相关日志哈希见机器可读证据，未修改用户提案。

字符串切片留在工作区，未另行提交/推送；完整 AOT 仍有失败，不能将其记为全绿或可合并状态。

## MVP 剩余门禁

字符串字面量与消息/回复、通用记录等业务值，启动/装载/全部可增长分配资源账本，完整长时公平/过载/并发故障恢复，最新源码原生 macOS AArch64/Linux x86-64 + MSRV，以及各平台同机同业务 BEAM 持续吞吐与整机内存双目标仍须完成。allocator/动态装载器、OS 内核和压缩/swap 的观测边界不因逻辑票据回收消失。本轮不提供吞吐提升/内存下降数字，也不宣布 MVP 完成。
