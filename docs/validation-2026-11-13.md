# 2026-11-13 验证记录

此记录对应 `972c229`（含 [ADR-0049](adr/0049-record-message-copies.md)、[ADR-0050](adr/0050-account-node-admission.md)），只描述已执行的功能验证。不改变 [MVP 门槛](roadmap.md#mvp-范围与开发顺序)，没有吞吐、尾延迟或整机内存领先结论。

## macOS AArch64

- 宿主 macOS 26.6.2、AArch64，Rust 1.99.1；MSRV 另用 Rust 1.85.0。
- 通过 workspace check/test、clippy all-targets `-D warnings`、fmt 检查及 Rust 1.85.0 all-targets check。
- 通过 debug/release workspace 测试、全部受限 AOT 回归和 TCP 示例单测。TCP 专项两种构建各重复三次；BEAM oracle v1 的 11 个 checked/FIFO/exit 用例与 23 个非法输入通过，不代表 Wings 与 BEAM 已运行等价性能负载。
- 预编译入口分别以 `WINGS_PREBUILT_EXAMPLES=1`、绝对 `WINGS_TARGET_DIR` 运行 debug/release 全回归，保持每子进程 20 秒 CPU、30 秒墙钟和禁 core dump。

## Linux x86-64：隔离仿真功能验证

QEMU TCG 在 Apple Silicon 上运行独立 Ubuntu 24.04 x86-64 客体，2 个虚拟 CPU、4 GiB RAM、12 GiB 虚拟磁盘，不挂载宿主目录。内核 6.8.0-146-generic，glibc 2.39，GCC 13.3.0，Python 3.12.3。该环境是 Linux 内核/ABI 的功能验证，**不是原生 Linux 性能平台**。

Rust 1.99.1 在 macOS 交叉构建 `x86_64-unknown-linux-gnu`，Zig 0.15.2 作为 linker；Rust 标准库按发行清单 SHA-256 校验。Cranelift 生成的业务产物在客体内编译、由 GCC 链接成 `.so`、由 Linux VM 加载执行，不复用 macOS 动态库。交叉 linker 给 release 产物的兼容性提示为 `ignoring deprecated linker optimization setting '1'`，不视为 Linux 原生 clippy 已通过。

| 验证 | 实际结果 |
|---|---|
| debug 独立测试可执行文件 | 25 个 harness、397 项通过，0 失败 |
| release 独立测试可执行文件 | 25 个 harness、397 项通过，0 失败 |
| TCP 示例 debug 单测 | 2 项通过；与 release 独立测试一起执行时共 26 个 harness、399 项 |
| release 受限 AOT 全回归 | 通过；包括受管帧、RecordInt 复制/入口根、GC、跨线程入口/请求、TCP 与清理 |
| debug 受限 AOT 全回归 | 未通过：大型 `timer.wings` 编译在仿真中返回 `-9`，符合既有 CPU 上限终止；未放宽上限重标通过 |
| 混合 release 编译器/debug VM 与示例 | 未通过全套：debug `frame_limits` 同样返回 `-9`；不能当全 debug 通过 |
| Linux 原生构建、clippy、MSRV、doc-tests、release TCP 单测 | 本次未执行，仍由后续原生 CI 分别验证 |

测试 harness 从 `cargo test --workspace --target x86_64-unknown-linux-gnu --no-run --message-format=json` 中选择 `profile.test` 且有 `executable` 的产物；在客体内逐个以 `timeout 120`、禁 core 执行。CLI 测试内嵌编译时二进制绝对路径，客体只在相同路径建立指向已复制二进制的链接，不修改测试断言。不把这次运行称为客体内完整 `cargo test` 或双平台 CI 已通过。

## 复现入口与后续

### ABI v12 增量：`6e390af`

同一 macOS 宿主通过 check/clippy/fmt、Rust 1.85.0 all-targets check、debug/release workspace 各 403 项测试、两种构建的 TCP 示例单测及完整受限 AOT。新增列表消息覆盖全部 64 元素、超限拒绝、混合载荷与 800 个单步 stop/Drop 边界。

复用上述隔离 Linux x86-64 客体，从 `6e390af` 重新交叉构建 release compiler/VM/examples、workspace 测试及 **release TCP 示例单测**。26 个 harness 共 405 项通过；完整 release AOT 通过，包含实际 `.so` 编译/链接/加载、列表源根/目标重建、跨线程入口和 TCP。每业务子进程仍为 CPU 20 秒、墙钟 30 秒、禁 core，独立测试仍为 `timeout 120`；未使用仿真时间/内存作排名。

此次增量未重跑 Linux debug、原生 clippy/MSRV/doc-tests，也不替代原生双平台 CI。表中的 debug AOT 上限失败仍是旧代码测量，不能据本次 release 通过重标 debug 成功。

### 跨线程模块增量：`3529ed7`

macOS 的 debug/release workspace 各 406 项测试、完整受限 AOT、TCP 示例单测，以及 check/clippy/fmt/MSRV 检查通过。模块代码、Library 与预算现在位于同一个 Arc 控制节点，不使用两个可能并发析构的独立 Arc。

同一隔离 Linux 客体重新运行 release workspace 独立测试及 release TCP 单测：26 个 harness 共 408 项通过。完整 release 受限 AOT 通过，新增四线程独立 worker 共享已计费代码、单步恢复与状态隔离、跨 VM 拒绝、最后 handler 跨线程释放；没有放宽既有子进程限制。Linux debug/native CI 与性能仍未验证，不将该用例称 M:N。

### 无活栈镜像增量：`70c4cb9`

macOS debug/release workspace 各 409 项测试及 check/clippy/fmt/MSRV、TCP 示例单测通过。debug 完整受限 AOT 通过；release 首次在既有 TCP `frame_limit` 读取首回复时遇到 ConnectionResetError，未修改该测试或截止时间，重跑完整脚本通过。保留此偶发失败，不将一次重跑当稳定性验收。

隔离 Linux release 独立测试含 release TCP 单测共 26 个 harness、411 项通过；完整受限 AOT 通过。新增镜像在三个新线程重绑同账户池，持续保存/更新 IntList 根，不重跑初始化；错 VM/账户/池及预算拒绝不丢镜像，取消后失败标志保留，未接回镜像在新线程 Drop 后资源归零。没有 unsafe Send、活栈迁移、worker 执行权路由或性能结论；Linux debug/native 门禁仍未补齐。

```sh
cargo build --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" python3 scripts/verify_scalar_aot.py
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PREBUILT_EXAMPLES=1 WINGS_TARGET_DIR="$PWD/target" WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py
```

交叉构建时指定 `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` 和 `--target x86_64-unknown-linux-gnu`；把目标二进制及源码复制到隔离 Linux，`WINGS_TARGET_DIR` 指向客体内的 `debug`/`release` 父目录。不要把跨架构仿真的 CPU 时间、内存占用或预留字节用于 BEAM 性能排名。

### 多 worker 调度增量：`eff95a4`

基于 `da57324` 的本次实现，macOS debug/release workspace 各 421 项测试通过；check、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check 与两种构建的 TCP 示例单测通过。新增 12 项 scheduler 内部回归，覆盖唯一 owner、返回后轮转迁移、并发生产者、固定代次、schema/载荷拒绝、队满、start 前准入失败保留队首、请求终态和销毁后结果租约。

两种构建的完整受限 AOT 均通过。`multi_worker` 分别用 1/2/3 个 worker 运行六个父 actor 及六个语言创建的子 actor，检查跨 worker RecordInt/IntList 消息、请求完成、GC、持久状态、活栈固定及忙 actor 不阻塞其他 actor。每业务子进程仍为 CPU 20 秒、墙钟 30 秒、禁 core。

保留失败记录：release 第一次完整回归在已有 TCP 非法帧用例得到 `invalid=2, io_errors=1`，与预期 `invalid=3` 不同；后续完整重跑通过。补齐初始化控制预留后，最终 release 回归又在已有 TCP overflow 场景遇到 ConnectionResetError，未修改 TCP 代码、断言或限制，完整重跑通过。不能将通过重跑写成无偶发失败或稳定性门禁通过。

隔离 Linux 从 `eff95a4` 重新交叉构建 release compiler/VM/examples、workspace 测试与 TCP 示例单测，26 个 harness 共 423 项通过；完整受限 release AOT 通过，包含实际 Linux `.so` 的多 worker 运行、消息/请求、迁移与清理。首次测试打包遗漏独立 `wings-lsp` 二进制，CLI help 用例报 NotFound；补传该未改动二进制后完整重跑全部 harness 通过，不将首次运行标成通过。

多 worker 监督、负载自适应调度、全资源账本、原生双平台最新 CI 和同负载 BEAM 吞吐/整机内存双目标仍未完成；固定共享扫描、编号轮转与每 worker 全容量栈池没有性能结论。Linux debug/native clippy/MSRV/doc-tests 本次仍未执行。M0/M1/MVP 均未完成。
