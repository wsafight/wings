# 模块依赖边界验证（2026-11-18）

按 [ADR-0129](adr/0129-native-module-dependency-boundary.md) 收紧计费产物的旁加载范围，基于 `28bbc02`。本机 macOS AArch64、Rust 1.99.1；保持 ABI v22、原 2 MiB 控制预算、4352 B 执行预留以及文件/映像/账户和所有测试时限。

## 修复前证据

原 `CC` 链接接口为普通 Counter 声明一个真实额外 dylib，helper 只导出 1 MiB 的只读零填充段，无构造器/线程，不被业务访问。主模块仍只预留 2129920 B，原精确预算/反复加载回归通过；`otool -L` 同时列出 helper 和系统 libSystem。helper 文件仅 16800 B，映像在主库预算之外。原始回归输出与依赖列表保留在 `target/module-dependency-before-20261118/report.json`。

本轮选择在同一文件快照内拒绝额外依赖，而不新增递归装载器。Mach-O 允许普通系统 libSystem load，ELF 允许 libc 的 SONAME 或没有依赖；其他依赖和加载重定向均在 `Library::new` 前正常失败。平台运行库属于可信宿主基线，实际内存仍须纳入整机测量，不声称系统库成本为零或宿主环境已被认证。

## 定向结果

新增三项测试与原四项映像测试共七项受限通过，包含 ELF/Mach-O、大小端、正常唯一系统依赖，以及未知库/绝对或相对重定向、弱/重导出/延迟依赖、rpath/interpreter/audit/filter 拒绝。ELF 另核对动态表缺终止、重复动态表/字符串元数据、非文件后备/重叠字符串范围、缺字段/越界名字。名称匹配用固定允许字节串含 NUL 前缀比较，不扫描产物控制的任意长名称，也不把它写入错误字符串。

`scripts/verify_module_image.py` 继续运行普通/1 MiB 零填充 Counter 的原精确预算、少一字节、32 次挂起取消、最后代码 owner、跨 VM 和错误退账；新增有效 helper 依赖产物明确返回 `unsupported native dependency`，VM 账目保持基线。显式 `load_trusted` 可加载相同产物且不进入 VM 账本，验证兼容工具入口仍独立存在；该正路径不使工具入口变成计费运行。

Debug/Release 模块专项均通过，实际普通/扩展预算仍分别为 2129920/3178496 B，扩展文件 17392 B。证据保留在 `target/module-dependency-after-{debug-v2,release}-20261118/` 及 `/tmp/wings-module-dependency-aot-{debug-v2,release}-20261118.log`；完整 AOT 也调用同一专项。

## 验证状态

workspace all-target check、clippy `-D warnings`、fmt、Debug/Release doc-tests、两构建 bins/examples、Rust 1.85 all-target workspace check 与 Linux x86-64 VM all-target 交叉检查均通过。Linux 交叉检查不等于原生运行；新增 Linux helper 链接分支仍待原生平台验证。

受限 workspace Debug/Release 各 **104 harness、698 tests** 通过；每 harness 保持 CPU 20s、墙钟 30s、core 0，清单/指纹/逐项日志位于 `target/workspace-validation/module-dependency-{debug,release}-20261118/`。VM lib 每种构建包含 340 项测试。未新增 Cranelift/前端到 VM 的依赖，也未改变 corosensei features。

Debug 完整 AOT 首轮在 `control_tokens` 的原 3 秒阶段断言失败，尚未进入本轮模块区块；日志 `/tmp/wings-module-dependency-full-aot-debug-20261118.log`。没有提高阶段限额、隐去失败或把它归因为本轮依赖预检。

Release 完整 AOT 首轮经过模块普通/扩展/依赖专项后，在 TCP 首个正常场景的原 5 秒 readiness deadline 失败；`elapsed=5.012s`，清理前仍存活，stdout/stderr 均空，清理后退出 -9。日志 `/tmp/wings-module-dependency-full-aot-release-20261118.log`。前一提交完整 Release 通过不覆盖本轮失败。

另做最多三次同一 Release 二进制/同一 TCP 产物的有界启动诊断：若一秒尚无握手才计划采样，仍按原五秒期限清理。实际三次分别约 0.595/0.0076/0.0077 秒握手，均返回 32 B 业务回复并恢复资源基线，未触发任何采样；原始 JSON 保留在 `target/module-dependency-startup-probe-20261118/report.json`。没有捕获慢启动，也没有新根因证据；不将三次正常诊断作为完整套件重试或消除原失败。

## 复验

```sh
cargo build --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

完整资源账本、宿主装载器/allocator 实际内存、Debug 完整套件稳定性、最新原生双平台、公平/故障长测、CPU 硬配额和同机同负载 BEAM 吞吐/整机内存双门禁仍待验收；MVP 未完成。
