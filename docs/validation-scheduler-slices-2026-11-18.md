# Scheduler 恢复区间验证（2026-11-18）

基于 `1d73f9f`，契约见 [ADR-0124](adr/0124-bounded-scheduler-slice-observation.md)。本机 macOS AArch64；本记录只覆盖新增诊断的来源、边界与清理，原生 Linux、长时公平性、CPU 硬配额和同负载 BEAM 吞吐/整机内存门禁保持独立。

## 观测与回归

`SchedulerConfig::slice_samples` 默认关闭，独立于 `yield_samples`。启用后每 worker 固定计费 FIFO 保存实际恢复调用两侧时间、完整 Pid、worker/序号和三种结果；没有原子请求也可出现步骤片返回。该区间包含 OS 暂停和恢复调用内处理，不是独占 CPU 时间，不覆盖入队等待、创建或返回后的 effect。

单测检查容量 0/1/65536、越界 worker/容量、FIFO 保留旧值与循环复用、丢样序号缺口、所有计数饱和、tenant/VM 准入失败和末 handle 清理。另在恰好只够首个 worker FIFO 的额度下让第二个创建失败，核对全部启动预留回滚。

`scheduler-slices.wings` 的 Work 在正常返回时提交准确循环计数；Hold 保留活栈，Fail 修改候选后除零。真实 AOT 一/双 worker × 关闭/容量 1/256，在原片预算 1、quantum 1ms、每阶段 3s 内检查：两个 Work 返回/提交 64；Hold 已实际 Suspended 时健康 actor 完成 256；停止 Hold 与 Fail 各形成一次失败恢复返回，Fail 保留旧提交 256。停止后统计恰有五次完成、两次失败和非零 Suspended，消费后 `published = taken + dropped = suspended + completed + failed`。原子观测始终关闭，开关不改变业务结果，资源最终回根。

```sh
cargo build --workspace --bins --examples
target/debug/wings-compile build examples/scheduler-slices.wings -o target/scheduler-slices-aot
target/debug/examples/scheduler_slices --trusted target/scheduler-slices-aot
```

默认完整 AOT 入口执行该主程序，CPU 20s/墙钟 30s/core 0，不提高原时限。

## 实际专项结果

debug/release 六组专项均通过，完整 JSON 保留 `/tmp/wings-slices-aot-specific-{debug,release}-20261118.log`。关闭模式无 FIFO/统计；四组启用模式的全部返回计数与丢样如下，所有组均有五次完成和两次失败，其余为 Suspended：

| worker 数 | 每 worker 容量 | debug 返回/丢样 | release 返回/丢样 |
|---|---|---|---|
| 1 | 1 | 3886 / 3884 | 3883 / 3881 |
| 1 | 256 | 3884 / 3372 | 3891 / 3379 |
| 2 | 1 | 3882 / 3878 | 3940 / 3936 |
| 2 | 256 | 3807 / 2783 | 3914 / 2890 |

宿主有意只在两个已停止业务的点消费，以验证满 FIFO 不覆盖旧样、后续循环复用和累计守恒；这些丢样量没有被隐瞒或当作完整时间分布，不能用于 p99。Hold 的调度交错会改变总返回数，这里只核对实际完整计数，不要求两个构建有同一交错。计费根均回到 232 B。

首次 VM harness 330 项通过、一项失败：测试将 VM 与 tenant 字节上限设为相同，忽略 tenant 节点已占父级额度，导致用于填满 tenant 的辅助 reserve 提前被 VM 拒绝。保留 `/tmp/wings-slices-vm-tests-20261118.log` 和 `/tmp/wings-slices-vm-harness-20261118.log`。修正测试树为 VM 128 MiB、tenant 64 MiB，使两个拒绝层独立可达；生产默认/业务额度不变。随后 331 项全部通过，日志 `/tmp/wings-slices-vm-{tests-final,harness-final}-20261118.log`，该 harness 指纹为 `8f5122c274415a2b0c45988e2422a13bd45d902bca8517161578e4f2807edc3d`。

## 全量验证与剩余门禁

本机 debug/release 各实际执行 103 个 harness、686 项测试，均通过；报告保留 `target/workspace-validation/slices-{debug,release}-20261118/`。两种构建的完整 AOT 和 doctest、workspace all-target check/clippy、格式检查、Rust 1.85 all-target 检查、Linux x86-64 的 VM all-target 交叉检查、8/6 项验证驱动测试均通过。原始日志为 `/tmp/wings-slices-{workspace-debug,aot-debug,workspace-release,aot-release,doctest-debug,doctest-release,check-final,clippy-final,fmt,msrv,linux-cross,runner-workspace,runner-aot}-20261118.log`。

前一切片的 [Counter v1 debug 超时](validation-quota-reply-2026-11-18.md#实际验证与保留失败) 保留不变。本次新源码完整通过不构成该次超时根因已解决的证据；另一次对运行中 loop_execution 的采样尝试晚于子进程退出，没有获得栈，驱动输出保留 `/tmp/wings-slices-loop-execution-sample-driver-20261118.log`，不据此推断加载器或 VM 故障。

Linux 交叉检查只验证编译。当前新增观测尚未接入冻结的长测协议，FIFO 的有意丢样也不能提供完整分位；每 actor 长时等待、公平性过载分组、CPU 硬配额、最新原生双平台以及同机同负载 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
