# handler 名称错误来源验证（2026-11-18）

## 复现与修正

基于 `0af1b2b`，契约见 [ADR-0122](adr/0122-bounded-handler-lookup-diagnostics.md)。本机 Apple M3 Pro、36 GiB、macOS 26.6.2 AArch64、Rust 1.99.1。原宿主模块查询直接将未命中的 `name` 放入错误字符串；独立分配计数器在 process 精确满额时用 3145728 B UTF-8 名字复现 3145744 B 错误与同尺寸分配，日志 `/tmp/wings-handler-lookup-reproduced-20261118.log`。这是调用方可影响的错误来源放大，CLI 输出端截断不能阻止它。

修复前测试初次将父/子额度都设为 16 MiB，满额填充 process 时先碰到 tenant 已有控制占用，未到达查询；改为原测试常用的 VM 16 MiB/tenant 12 MiB/process 8 MiB。保留 `/tmp/wings-handler-lookup-before-20261118.log` 的测试设置失败，不把它当作 runtime 根因。

产物库公开现有 `MAX_NAME_BYTES=256` 与 `valid_name`，VM 查询及 CLI 解码复用同一规则。空名、超过 256 UTF-8 字节或含控制字符在扫描/错误插值前返回固定 `invalid handler name`；合法已知/未知名集合不变，未知名保留原诊断。没有截断 UTF-8，没有变更语言/清单/ABI。

## 检查内容

`handler_lookup` 在受限子进程中分别填满 process、tenant、VM，核对直接模块查询和进程调用两条路径。覆盖空名、ASCII 256/257 B、UTF-8 256/258 B、换行/NUL/DEL、3 MiB 名字，共 60 个满额错误场景；每次账户不变，释放填充后原计数状态仍为 7，最终回到根启动基线。已存在名字的模块查询不分配，未知合法名错误最多 272 B/单次最大分配不超过 1024 B，非法名字固定 20 B。

debug/release 专项均通过；3 MiB 名字两入口、三级满额下均只分配 20 B。记录为 `/tmp/wings-handler-lookup-fixed-{debug,release}-20261118.log`。产物/VM 单测分别检查合法集合与完整 UTF-8 边界；完整 AOT 加入真实 Counter 的主程序执行，test harness 只用于其编译验证。

```sh
cargo build --workspace --bins --examples
target/debug/wings-compile build examples/counter.wings -o target/handler-lookup-aot
target/debug/examples/handler_lookup --trusted target/handler-lookup-aot
```

修复后 debug/release 专项各通过 60 个满额错误场景：3 MiB 非法名仅生成 20 B 错误/20 B 最大分配，成功 `Get` 查询零分配。`cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和两 profile 的 bins/examples 构建通过。debug/release 全 workspace 分别通过 101 个 harness、681 项测试；独立 workspace/AOT 驱动分别通过 8/6 项测试。

实际证据为 `target/workspace-validation/handler-lookup-{debug,release}-20261118/`，以及 `/tmp/wings-handler-lookup-{check,clippy,fmt,build-debug,build-release,fixed-debug,fixed-release,workspace-debug,workspace-release,runner-workspace,runner-aot}-20261118.log`。普通分配计数器只观测调用 Rust 全局 allocator 的逻辑请求大小，不将其当作 libc 内部大小、RSS、PSS 或物理占用。

两 profile 完整 AOT 与 doctest 均通过；Rust 1.85 全 workspace/all-targets 及 Linux x86-64 的 VM/all-targets 交叉检查通过。日志 `/tmp/wings-handler-lookup-{aot-debug,aot-release,doctest-debug,doctest-release,msrv,linux-cross}-20261118.log`。本轮没有出现运行时/AOT 回归失败；交叉检查不算原生 Linux 结果，未推送或发布。

## 边界

输入名字是测试宿主预先分配的固定有界样本，不将其算作 VM 业务存储；`String` 错误仍属于调用方，没有将 20 B 错误伪报成受管计费。修正的是来源大小可由输入任意放大的一个缺口，完整诊断生命周期、allocator/装载器账本、最新原生双平台、长时过载/公平及 BEAM 吞吐/整机内存双门禁仍独立验收，MVP 未完成。
