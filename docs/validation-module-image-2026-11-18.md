# 模块映像准入验证（2026-11-18）

按 [ADR-0128](adr/0128-native-module-image-admission.md) 补计原生库的映像跨度，基于 `8b485b7`。本机 macOS AArch64、Rust 1.99.1、OS 页 16384 B；不改变 ABI v22、4352 B 执行控制、文件/账户上限或验证时限。

## 修复前证据

普通 `examples/counter.wings` 经原编译器生成 17344 B 的 `program.dylib`，SHA-256 为 `6f6abb98f81fab281b134e6b1e675347914aa83adce7f6e83d855f648697dcc9`。旧 `module_accounting` 回归通过，库票据只有 20480 B；原生 `__TEXT` 与 `__LINKEDIT` 各 16384 B，整个映像跨度为 32768 B，比文件票据多 12288 B。此结论不依赖恶意 native 代码、可变全局、TLS 或 ABI 外行为，也不是 RSS 测量。原段表/指纹/旧回归输出保留于 `target/module-mapping-before-20261118/report.json` 和同目录日志。

修复后先保留原控制/文件准入，在文件票据内 fallible 分配固定长度快照；复用 `object 0.36.7` 原始格式读取，不创建段/节对象向量。释放快照后追加映像与文件页差额，成功才 `Library::new`。常驻总额为 `2 MiB + max(文件页, 原生页对齐映像跨度)`，包含零填充与段间空洞；映像超过 64 MiB 先拒绝，不调整 VM 限制。历史 ADR 保留原规则并链接替代决定。

## 定向验证

`scripts/verify_module_image.py` 使用真实编译器和只读零填充对象重链原 Counter，原业务入口/清单不变。Debug/Release 各通过：

- 普通 Counter 总预算为 2129920 B；增加 1 MiB 只读零填充后，文件仍只有 17392 B，总预算为 3178496 B，精确增加 1048576 B。Mach-O fixture 的 `__WINGS` 段为 `r/r`、`filesize=0`，代码不访问或修改该段。
- 独立 Python 段表预言提供预算；精确总额成功，少一字节返回 VM 配额拒绝并归零，同路径第二次加载不借 OS 可能共享映射逃避准入。
- clone/HandlerId/实例/活栈共持租约；32 次挂起取消循环、最后持有者、跨 VM/换池拒绝、坏清单/库/符号回滚和原状态保持均通过。原有 16/12/8 MiB 执行绑定检查与其余账户上限保持不变。
- 修改副本段表使映像超过 64 MiB，明确核对装载前映像上限错误及账目归零；损坏副本不交给动态装载器。

新增四项受限单测覆盖 ELF64/Mach-O64、两种端序、4096/16384 B 页、零填充/空洞/重叠/空段、精确 64 MiB、文件范围错误、地址/页取整溢出、逐字节截断、错误类型及 Mach-O 不完整命令拒绝。跨线程末 owner 测试将原 1024 B 分为 512/256/256 B 三票据，核对三者同时保留和释放；未提高测试预算。

初次单测的预期成功 fixture 将 512 B 文件段写成 1 B 内存段，正确触发范围拒绝；已修正 fixture。初次 4 MiB 零填充真实回归在原执行绑定填额阶段先被 VM 父额度拒绝，未到目标断言；将 fixture 降至 1 MiB 后通过，不提高任何账户限制。另一次命令误用 zsh 只读变量 `status`、两处测试租约构造漏新字段，均为开发期工具/测试接线错误，已修正，非运行时通过结果。

Linux ELF 交叉链接布局另检查了 1 MiB NOBITS：文件 5328 B，首个 PT_LOAD 内存 1052672 B。Homebrew lld 首次因与 LLVM dylib 版本不匹配启动失败；改用现有 Rust 1.85 自带 `rust-lld` 成功，不修改系统工具。该检查只生成并读表，未执行 Linux 代码，不能替代原生 Linux 验收。

## 全量结果

- 受限 workspace Debug/Release 各 **104 harness、695 tests** 通过；每 harness 沿用 CPU 20s、墙钟 30s、core 0。清单、SHA-256 和逐项日志保留于 `target/workspace-validation/module-image-{debug,release}-20261118/`。最后跨线程三票据调整后，VM lib 的 Debug/Release **337 tests** 再次受限通过。
- workspace all-target check、clippy `-D warnings`、fmt、Debug/Release doc-tests、两构建 bins/examples、Rust 1.85 all-target check 与 Linux x86-64 VM all-target 交叉检查通过；依赖隔离和 corosensei 无 unwind feature 检查通过。
- Python TCP 取证 6 项、workspace 运行器 8 项及资源回执的 37 种伪造拒绝通过。
- **Release 完整 AOT 一次通过**，含新增模块 fixture、TCP、原有公平性 smoke、全部业务值/GC/恢复/清理；日志 `/tmp/wings-module-image-full-aot-release-20261118.log`。
- **Debug 完整 AOT 未通过**：模块专项和前序账本回归已通过，TCP 首个正常场景仍未在原 5 秒内得到 readiness；`elapsed=5.004s`，清理前仍存活，stdout/stderr 空，清理后 -9。日志 `/tmp/wings-module-image-full-aot-debug-20261118.log`。不推断根因、不延长时限、不以 Release 通过消除失败。

可复验命令：

```sh
cargo build --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
python3 scripts/verify_workspace.py --output target/workspace-validation/module-image-debug-fresh
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

## MVP 边界

本轮只补模块映像逻辑准入，不覆盖 dyld/ld.so、依赖库、allocator 内部成本或 OS RSS/PSS/物理占用。完整资源账本、Debug 全量稳定性、最新原生 Linux、CPU 硬配额、公平/过载/故障长测和同机同负载 BEAM 吞吐/整机内存双门禁仍待验收；MVP 未完成。
