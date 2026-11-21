# 记录回复配额拒绝分配验证（2026-11-18）

## 复现与变更

基于 `74aa924`，契约见 [ADR-0123](adr/0123-status-only-resource-rejection.md)。本机 Apple M3 Pro、36 GiB、macOS 26.6.2 AArch64、Rust 1.99.1。原记录回复在准入失败时先构造 `resource_exhausted` 的 String，又立即丢弃并返回 HeapExhausted；最终宿主执行接口另生成一次自身错误。

新增真实 AOT `quota-reply.wings`：初始 state=7，Value 将候选设为 99 并返回空具名记录。先创建计费堆/栈/token，开始 invocation，再精确填满 process/tenant/VM 的某一级；只计 `resume` 到失败结果的分配。修复前首个 process 场景实际为两次，预期一次的断言失败，原 CPU 20s/墙钟 30s/core 0 证据保留 `/tmp/wings-quota-reply-reproduced-20261118.log`。

核心准入事务内部返回失败 ResourceLevel，公开 `reserve` 沿原文本包装。记录回复直接将该失败级别映射为 HeapExhausted；锁、准入顺序、票据和公开 API 不变。没有复制第二套准入算法，没有将所有错误调用改为零分配。

## 专项检查

debug 修复后 process/tenant/VM 三个场景均只余一次最终宿主诊断分配，仍得到 `managed record heap exhausted`，snapshot 为 Failed/HeapExhausted，不保留活栈；最终全部资源回到根启动基线。随后允许额度的全新实例正常返回 Empty schema，Read 为提交后的 99。分配计数器只观测 Rust 全局 allocator 调用，不等于 libc/RSS/物理内存。

```sh
cargo build --workspace --bins --examples
target/debug/wings-compile build examples/quota-reply.wings -o target/quota-reply-aot
target/debug/examples/quota_reply --trusted target/quota-reply-aot
```

账户回归覆盖三个限制层 × 四维资源：内部状态与公开文本报告同一首个拒绝层，失败后三个账户均不变，释放填充后成功票据逐级计入，Drop 回基线。另保留 u64 加法溢出拒绝；这是逻辑账面测试，没有分配 u64::MAX 的实际内存。默认完整 AOT 执行新主程序，延续所有受限堆/根/恢复/BEAM 模型回归。

## 实际验证与保留失败

debug/release 的独立配额专项均通过，三层每次只分配一个最终宿主错误，足额实例返回 Empty 并提交 99；原始日志为 `/tmp/wings-quota-reply-final-{debug,release}-20261118.log`。两种构建的 workspace 实际各执行 102 个 harness、682 项测试，报告保留于 `target/workspace-validation/quota-reply-{debug,release}-20261118/`。

release 完整 AOT、两种构建的 doctest、workspace all-target check/clippy、格式检查、Rust 1.85 all-target 检查、Linux x86-64 的 VM all-target 交叉检查，以及 8/6 项验证驱动测试均通过。日志分别为 `/tmp/wings-quota-reply-{aot-release,doctest-debug,doctest-release,check,clippy,fmt,msrv,linux-cross,runner-workspace,runner-aot}-20261118.log`。交叉检查只验证编译，不替代原生 Linux 执行。

**debug 完整 AOT 本轮未通过**：原 CPU 5s/墙钟 10s/core 0 的 Counter v1 子进程超时，使外层验证停止，尚未执行到新增配额主程序。原错误与完整输出保留 `/tmp/wings-quota-reply-aot-debug-20261118.log`，没有扩大时限或覆盖失败。原驱动没有输出该次失败的具体输入，无法从该日志确定是哪一个用例，更不能认定根因已修复。

随后用独立诊断驱动逐次保留输入、输出、PID 与耗时，并在运行超过 4s 时尝试原生采样；CPU/墙钟边界和业务不变。在固定 target 路径及原 AOT 临时路径各执行一轮，45 个合法场景、28 个 JSON 拒绝和 14 个 bridge 拒绝均通过，没有达到采样时点。诊断脚本 `/tmp/wings-counter-timeout-diagnostic-20261118.py`，日志 `/tmp/wings-counter-timeout-{diagnostic,temp-path}-20261118.log`，逐次证据 `target/counter-timeout{-evidence,-temp-path-evidence}-20261118/`。这两轮是带取证的定位运行，没有重跑原完整失败命令；未复现不等于解决超时，也不将 release 通过替代 debug 的缺口。

## 边界

这是一条真实受管记录回复拒绝路径中去掉一次丢弃错误分配的证据。最终宿主错误仍分配一次，其他文本错误路径、allocator/动态装载器账本仍未闭环。没有以分配次数推导吞吐、内存领先或全部 OOM 可恢复；原生双平台、长时过载/公平及 BEAM 同负载吞吐/整机内存双门禁仍独立验收，MVP 未完成。
