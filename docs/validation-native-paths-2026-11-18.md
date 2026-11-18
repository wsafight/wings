# 2026-11-18 固定原生路径缓冲验证

对应 [ADR-0136](adr/0136-fixed-buffer-native-module-paths.md)。本机 Apple M3 Pro/macOS 26.6.2 AArch64、Rust 1.99.1；基线 `f021aeb`。ABI v22、4352 B、模块控制 2 MiB、文件/映像预算及原测试期限不变。

## 改动与边界

计费模块不再通过拥有型 canonicalize 取得长度未知的 Rust 输出；先在原控制准入内检查原生来源的 16 KiB/NUL，再以固定 16385 B 来源与 `PATH_MAX` 输出数组调用原生 realpath。验证终止/绝对结果后，只按已知长度请求一次 Rust 存储。macOS 原生 PATH_MAX 为 1024 B，Linux 交叉目标为 4096 B；不以较大的 SDK 原始来源上限提高系统路径限制。工具加载保留标准 canonicalize。

五项定向回归核对来源前置拒绝、恰好 16 KiB/空/非 UTF-8 输入、模拟最大/未终止/非绝对/非 UTF-8 输出、固定容量及原生相对路径/Unicode/符号链接/缺失/循环链接。原生非 UTF-8 文件名的成功与失败按平台实际与 canonicalize 比较，不把模拟输出说成真实文件系统支持。

本机 `man 3 realpath` 明确非空输出指针须提供至少 PATH_MAX 存储，并在成功时返回此指针；Rust 1.99.1 已安装的 std 源码则调用 `realpath(source, NULL)` 再复制结果。对应手册和源码节选保留在 `target/native-path-initial-failure-20261118/`，只说明原生接口/旧实现，不推断 libc 内部分配为零。

## 失败证据

初次受限子进程定向执行为 4 通过、1 失败（exit 101）：测试先假定 macOS 允许创建非 UTF-8 文件名，实际 `create_dir` 在进入 resolver 前以 errno 92（Illegal byte sequence）失败。随后原二进制确认同一失败并保存 stdout/stderr/指纹至 `target/native-path-initial-failure-20261118/`；首次失败保留在工具历史，第二次为显式确认，不冒充首次落盘。测试改为创建 Unicode 真实目录；非 UTF-8 在 Linux 创建，在两平台都比较实际成功值或错误，另有精确原生字节模拟测试。未放宽运行时边界或跳过该比较。

## 验证状态

修正后的五项定向回归在原 CPU 20s/墙钟 30s/core 0 的 OS 子进程通过，日志 `/tmp/wings-native-path-focused-20261118.log`。Debug/Release 各 105 harness、707 项测试全部通过，报告为 `target/native-path-workspace-{debug,release}-20261118/report.json`；比基线增加五项。

原十二场景分配探针两种构建各通过，保留可执行副本、stdout/stderr 和 SHA 至 `target/native-path-probe-{debug,release}-20261118/`。3 MiB 三种来源与 16385 B 来源仍为一次 38 B 固定错误；全满仍先以一次 60 B 原资源错误拒绝。恰好 16384 B 来源由原四次/16457 B 总请求/16385 B 最大请求，变为三次/72 B/36 B；空来源维持三次/100 B/50 B。全部退账正确。这些只计 Rust 分配请求，不是存活内存、libc scratch 或端到端性能结果。

顺序执行的 Debug/Release 完整 AOT 均通过，日志 `/tmp/wings-native-path-full-aot-{debug,release}-20261118.log`；不关闭原启动超时。首次工程检查的 workspace check 通过，Clippy 因两个测试内常量断言触发 `assertions_on_constants` 拒绝，日志 `/tmp/wings-native-path-clippy-20261118.log` 保留；只将断言移入 const block，不改变实现或门槛。修正后的工程与平台检查另记；未完成项不作为通过。

最终 `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets、Linux x86-64 all-targets 交叉检查通过，对应 `/tmp/wings-native-path-{check,clippy,fmt,msrv,linux-cross}-final-20261118.log`。Debug/Release workspace doctest、八项 workspace 运行器和六项 AOT 运行器自检通过。两种构建完整测试和 AOT 后仅调整了测试常量断言的 const 写法，没有变更运行实现；最终定向执行另保留日志。交叉检查不执行 Linux 代码，不能称原生 Linux 已验证。

复现命令：

```sh
cargo build --workspace --bins --examples --locked
cargo build --release --workspace --bins --examples --locked
python3 scripts/verify_workspace.py --output target/native-path-workspace-debug-new
python3 scripts/verify_workspace.py --profile release --output target/native-path-workspace-release-new
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo +1.85.0 check --workspace --all-targets --locked
cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu --locked
```

libc realpath 内部 scratch、动态装载器和 allocator 元数据仍未逐项闭环；失败错误的 String、宿主线程栈及完整资源门禁也不因固定缓冲而自动完成。启动超时根因、公平/故障长测、原生 Linux/双平台和同机同负载 BEAM 吞吐/整机内存双门禁仍待验收，MVP 未完成。
