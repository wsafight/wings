# String 索引与切片验证（2026-11-18）

按 [ADR-0127](adr/0127-accounted-string-indexing-and-slicing.md) 增加 Unicode scalar value 索引及独立计费切片。ABI v22；测试未提高 64 KiB 字符串上限、4352 B 执行控制预留或原 CPU/墙钟限制。

## 定向结果

本机 macOS AArch64、Rust 1.99.1。`cargo test --workspace --locked`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `cargo fmt --all -- --check` 通过。

Debug 和 Release 均由真实 `wings-compile` 编译 `examples/string-slicing.wings`，执行 VM 示例及 CLI。覆盖 ASCII/多字节 code point、Unicode scalar value 切片、空/尾部切片、UTF-8 跨 256 B 分块、64 KiB 边界、负数/越界/`i64` 加法溢出错误、单步暂停、取消和堆清理。示例以 16/1024 槽堆分别验证 GC 后只保留已提交根、取消不泄漏账户额度；CLI 逐条得到 12 个预期回复，并对四种负例核对独立错误文本。Release 工具通过：

```sh
cargo build --release --locked -p wings-compile -p wings-vm --bins --example string_slicing
```

TCP adapter 专项完整场景复验通过，但不消除完整套件的间歇启动失败。

## 完整套件失败

`python3 scripts/verify_scalar_aot.py` 两次在 TCP readiness 的原 5 秒期限失败。清理前子进程仍存活，stdout/stderr 为空；期限到达后由有界清理杀死，报告 `elapsed=5.012s`、`exit=-9`。其后 TCP 专项所有既有场景通过，未延长时限，也未推断启动根因。

第三次全量运行越过 TCP 与前序 AOT 场景，停止于 fairness 子验证：`observed_yield --trusted <artifact>` 超过既有 10 秒限制，`verify_fairness.py` 返回 3。该轮未到达全量脚本中的 String slicing 区块。另一次初始定向 harness 误用会自动附加 `--trusted` 的通用 helper，已改为只传 artifact 路径的 `cargo_example`；对应的定向 Debug/Release AOT 随后通过。

因此本轮不能报告完整 scalar AOT suite 通过；上述启动/公平性失败仍有效，不归因于 String 实现，也不因定向通过而删除。

## MVP 边界

该功能的定向 Debug/Release AOT 结果不证明完整资源账本、完整 AOT 套件、最新原生 Linux x86-64、公平/过载/并发故障长测、CPU 硬配额或双平台同机同负载 BEAM 吞吐与整机内存双门禁完成。MVP 未完成。
