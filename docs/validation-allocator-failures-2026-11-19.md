# 真实分配失败注入验证（2026-11-19）

按 [ADR-0141](adr/0141-bounded-allocator-failure-injection.md) 推进 MVP 的资源失败回滚和故障验证，基于 `5b00224`。本机 macOS 26.6.2 AArch64、Rust 1.99.1；没有原生 Linux 运行。本切片仅增加独立测试 allocator、真实 AOT fixture 和验证入口，不更换生产分配器，不改变 ABI v22、4352 B 执行控制、资源容量或原期限。

## 注入与结果

`allocation_failures` 先核对 OS 子进程 CPU 至多 20s、core=0，父进程保持 30s 墙钟限制。按分配序号或请求大小，对有限次 Rust `alloc`/`alloc_zeroed`/`realloc` 返回 null，其他请求交给 System；不是访问空指针，也不制造宿主 OOM。

最终冻结 Debug/Release 可执行与各自新编译的 AOT 产物分别通过，stderr 均为空。每个构建实际注入 18 次：固定堆四份存储 4 次、根表 1 次、带名义记录布局的 AOT 堆构造 7 次、调用参数 1 次、受管 Bytes 一次失败后 GC 重试 1 次、连续失败 2 次、宿主消息副本 1 次、候选回复副本 1 次。各失败必须观察到注入计数，不以未命中故障的正常执行代替。

- 构造失败后三级账户、请求存储和栈池回到调用前；正常根构造/收集仍可执行。
- 调用参数失败没有启动 handler 或丢失原堆，同一任务重试返回 8。
- 宿主消息复制失败没有入队，状态不变；相同对象重试后提交完整长度 65536 B 和既有前缀快照。
- 受管 Bytes 首次失败沿既有一次 GC 重试完成；连续两次失败正常退出为 HeapExhausted。回复导出失败同样不提交候选。失败状态只核对既有长度/32 B 前缀，不声称完整状态导出。
- 两次受管分配场景中的健康 actor 各完成一次回复，失败没有阻止其进展；所有 owner 释放后 VM 账面回到原根 232 B。

可执行、产物、逐次 stdout/stderr 和 SHA 位于 `target/allocator-validation-20261119/`；专项报告为 `null-{debug,release}/report.json`，总命令/退出/日志指纹见 `report.json`。先前扩展探针及其旧输出保留于 `target/allocation-failures-expanded-20261119/`；该旧输出尚无总注入字段，不冒充最终严格报告。

## 回归与失败保留

四项报告突变测试、六项 AOT 运行器测试、八项 workspace 运行器测试通过。最终 Debug/Release workspace 各 108 harness/719 项测试通过；新增 example 的零项 libtest harness 不替代上述真实 main 注入。workspace all-target check、clippy `-D warnings`、fmt、两种构建 bins/examples 与 doctest、MSRV 1.85 all-target 和 Linux x86-64 VM all-target 交叉检查通过。首次 fmt 和 clippy 分别拒绝排版及两处 `err().expect()`，修正后独立 final 日志通过，初次失败日志保留；没有修改旧测试或降低门槛。

Debug 完整 AOT 在旧 Int TCP normal 的原五秒握手期限再次失败：elapsed=5.007s、清理前活跃、清理 exit=-9、双流前缀为空，尚未进入新增注入入口。日志 `full-aot-debug.log` 保留，不把定向通过当完整 Debug 通过。随后顺序 Release 完整 AOT 通过，包括新注入及原业务/调度/故障入口；日志 `full-aot-release.log`。没有并行构建或性能长测干扰这些运行。

此前显式 dyld 日志诊断保留于 `target/dyld-startup-20261119/`：首 TCP 在约 0.844s 就绪并完成四请求，但诊断本身输出到 stderr，违反原回归的空 stderr 要求，完整入口以失败结束。它不是启动修复或完整通过，不能通过过滤诊断 stderr 改写原结果。原启动缺陷仍未定位；[先前采样](validation-startup-followup-2026-11-18.md)与原失败均保留。

## 复现入口与边界

```sh
cargo build --workspace --bins --examples --locked
target/debug/wings-compile build examples/allocation-failures.wings -o target/allocation-aot-new
python3 scripts/verify_allocation_failures_tests.py
python3 scripts/verify_allocation_failures.py --artifact target/allocation-aot-new --output target/allocation-check-new
python3 scripts/verify_workspace.py --output target/allocation-workspace-new
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

Release 使用相应 release 构建、编译器和 `WINGS_PROFILE=release`，输出目录另建。只注入已审计的 fallible 分配，不承诺 Arc/Box/标准库 infallible 控制分配或系统全局 OOM/abort 的进程内恢复。Rust 请求存储与逻辑退账不是 allocator 内部成本、原生装载、RSS、PSS 或物理占用。完整资源账本、启动稳定性、公平/过载/服务长测、CPU 硬配额、最新原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
