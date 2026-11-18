# String 索引与切片验证（2026-11-18）

按 [ADR-0127](adr/0127-accounted-string-indexing-and-slicing.md) 增加 Unicode scalar value 索引及独立计费切片。ABI v22；测试未提高 64 KiB 字符串上限、4352 B 执行控制预留或原 CPU/墙钟限制。

## 定向结果

本机 macOS AArch64、Rust 1.99.1。`cargo test --workspace --locked`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `cargo fmt --all -- --check` 通过。

Debug 和 Release 均由真实 `wings-compile` 编译 `examples/string-slicing.wings`，执行 VM 示例及 CLI。覆盖 ASCII/多字节 code point、Unicode scalar value 切片、空/尾部切片、UTF-8 跨 256 B 分块、64 KiB 边界、负数/越界/两个 `i64::MAX` 的极端范围、单步暂停、取消和堆清理。示例以 16/1024 槽堆分别验证 GC 后只保留已提交根、取消不泄漏账户额度；CLI 逐条得到 12 个预期回复，并对四种负例核对独立错误文本。该极端范围超出字符数，在 64 位平台上并不触发 `usize` 加法溢出。Release 工具通过：

```sh
cargo build --release --locked -p wings-compile -p wings-vm --bins --example string_slicing
```

TCP adapter 专项完整场景复验通过，但不消除完整套件的间歇启动失败。

## 后续回归

在 `3387e28` 后补齐 LSP 两个新内建的补全和共享语义诊断，协议测试 41 项通过。新增两个 runtime 单测逐块 GC 验证源根和独立结果，并覆盖负数、边界、极端范围、总预算耗尽、取消、错误类型及陈旧引用；失败输出保持哨兵值，未发布 buffer 额度归还。运行时测试显式发布根，真实 compiler 根仍由 IR 和 AOT 独立验证。

真实 AOT 示例增加 process/tenant/VM 三层切片准入失败：先初始化、填入 64 KiB String、清扫并启动 invocation，再用逻辑 Reservation 填满指定账户，不实际分配对应内存。切片返回 HeapExhausted/Failed，任务不再 active，原状态根的对象数和计费字节保持；释放占额票据后 process 用量精确回到调用前，最终 tenant 回到基线。Debug/Release 均通过，不允许已失败实例继续执行。

开发期曾错误使用调用完成后的稳态用量推导实例化上限：初始 7531 B、Fill 后 138603 B，另留 4096 B 仍不能覆盖借用临时初始化栈的峰值，数次测试在实例化前被拒绝，未执行到切片。之后采用既有 quota-reply 的调用准入后占额模式；相同的父子额度还曾导致填 process 时先受 tenant 拒绝，已将测试账户改为 VM 64 MiB、tenant 48 MiB、process 32 MiB，确保分别命中指定层。没有修改 runtime 上限或准入算法。

受限全量 Debug/Release 各 104 个 harness、691 项测试通过，清单、SHA-256 和逐项原始日志分别保留在 `target/workspace-validation/string-slicing-followup-{debug,release}-20261118/`。每个 harness 沿用 CPU 20s/墙钟 30s/core 0。all-target check/clippy、fmt、Rust 1.85 all-target workspace 检查和 Linux x86-64 VM all-target 交叉检查通过；后者不是原生 Linux 执行。两种 profile 的 bins/examples 构建通过。

## 完整套件失败

`python3 scripts/verify_scalar_aot.py` 两次在 TCP readiness 的原 5 秒期限失败。清理前子进程仍存活，stdout/stderr 为空；期限到达后由有界清理杀死，报告 `elapsed=5.012s`、`exit=-9`。其后 TCP 专项所有既有场景通过，未延长时限，也未推断启动根因。

第三次全量运行越过 TCP 与前序 AOT 场景，停止于 fairness 子验证：`observed_yield --trusted <artifact>` 超过既有 10 秒限制，`verify_fairness.py` 返回 3。该轮未到达全量脚本中的 String slicing 区块。另一次初始定向 harness 误用会自动附加 `--trusted` 的通用 helper，已改为只传 artifact 路径的 `cargo_example`；对应的定向 Debug/Release AOT 随后通过。

补充回归后的 Debug 完整 AOT 在 `verify_counter_equivalence.py` 的 `beam_counter` 子进程超过原 10 秒限制，返回 3，仍未到达 slicing 区块；日志 `/tmp/wings-string-slicing-followup-aot-debug-20261118.log`。Release 完整 AOT 随后一次通过，包含 slicing、三层配额、TCP、fairness smoke、各类消息/回复、GC、监督和清理；日志 `/tmp/wings-string-slicing-followup-aot-release-20261118.log`。Release 通过不覆盖 Debug 缺口，也不说明先前超时根因已解决。

上述启动/公平性/Counter 失败仍有效，不归因于 String 实现，也不因定向或 Release 通过而删除。

## MVP 边界

该功能的定向 Debug/Release AOT 结果不证明完整资源账本、完整 AOT 套件、最新原生 Linux x86-64、公平/过载/并发故障长测、CPU 硬配额或双平台同机同负载 BEAM 吞吐与整机内存双门禁完成。MVP 未完成。
