# 持续取消与监督恢复验证（2026-11-18）

## 范围与命令

基于 `593e487`，契约见 [ADR-0121](adr/0121-persistent-cancellation-recovery.md)。本机 Apple M3 Pro、36 GiB、macOS 26.6.2 AArch64、Rust 1.99.1；没有 BEAM 等价恢复对照。原 `concurrent_recovery` 默认仍分别对一/双 worker 各跑原一轮，新短入口才使用同一 Scheduler/supervisor 连续三轮。保留两 actor、4 KiB Bytes、mailbox 16、结果 17、两 sender 各八条 Add 与原三秒阶段，根使用实际计费构造，额度不变。

```sh
cargo build --workspace --bins --examples
python3 scripts/verify_recovery_soak.py
cargo build --workspace --bins --examples --release
WINGS_PROFILE=release python3 scripts/verify_recovery_soak.py
target/release/wings-compile build examples/concurrent-recovery.wings -o target/recovery-soak-aot
WINGS_PROFILE=release python3 scripts/measure_recovery_soak.py --artifact target/recovery-soak-aot --output target/recovery-soak
```

默认完整 AOT 加入一/双 worker 三轮及独立模型，只有最后的显式测量命令才启动每配置 30s 预热/60s 采样的 900 轮。恢复 trace 版本 11 只标识这个独立诊断，不扩展 v9/v10 的 BEAM 业务协议。

## 验证口径

每条排队请求有一次可消费终态：每 sender 前两条立即超时、接着两条取消，后四条与 stop 竞争只允许取消/失败；提前消费的结果不会被 stop 复活。队列和请求票据在 publisher 存活时仍各占 16，下一条队满且账户零净变，健康 sibling 独立 +7。stop 后创建的新 child 执行两次已经取消结果的 Fail，实际均须除零并保存旧提交零状态；第一次全新重启状态零，第二次强度超限。原政策一次/60s 不变，每轮显式新 child 的历史独立。

当前/上一轮的四个完整 Pid 与三个 child ID 必须失效；上一轮拒绝在新目标存活时核对。每轮三维活动资源归零，逻辑字节最多为冷 Scheduler 基线加每 worker 两个原栈槽，最终回到根启动基线。缓存数仍由账目差额推导，不是独立 worker 映射普查；状态快照只包含长度/32 B 前缀，不据此声称完整状态导出或账本。

短子进程 CPU 5s/墙钟 10s/core 0；完整 AOT 原 CPU 20s/墙钟 30s。显式长测每配置 CPU 240s/墙钟 130s、阶段 120s/每轮 3s/core 0，trace 64 MiB、固定 64 KiB 缓冲、RSS 1 GiB 安全停止。复用失败长测驱动的受限捕获与失败保留，仅将独立重放函数注入；原 v10 仍使用原默认 oracle。

## 开发检查

初期拆出单轮后、接回入口前的编译检查发现旧 `verify` 名称尚未替换；接回后通过。首次 clippy 指出 `map().flatten()`，改用 `flat_map`，不涉及运行时语义。失败日志保留 `/tmp/wings-recovery-reuse-initial-20261118.log`、`/tmp/wings-recovery-reuse-clippy-r1-20261118.log`。首轮一/双 worker 三轮已通过；模型另检查合法竞争终态和 900 轮合成历史，合成历史不算真实长测。

debug/release 各通过 100 个 harness、679 项测试及完整 AOT；两 profile doctest、8 项 workspace 驱动和 6 项 AOT 驱动测试通过。Rust 1.85 全 workspace/all-targets 与 Linux x86-64 的 VM/all-targets 交叉检查通过，交叉检查不等于原生运行。证据为 `target/workspace-validation/recovery-reuse-{debug,release}-20261118/` 和 `/tmp/wings-recovery-reuse-{workspace-debug,aot-debug,workspace-release,aot-release,doctest-debug,doctest-release,msrv,linux-cross,runner-workspace,runner-aot}-20261118.log`。

最终复审保留原默认单轮在关闭/join Scheduler 后的三秒检查，并让独立模型同时核对队满时 process=2/message=17/request=16，不能用两份相同的错误账目蒙混过关。这些断言随后单独重建 debug/release，复验默认入口、持续三轮和 v10 三端；`check --workspace --all-targets`、`clippy --workspace --all-targets -- -D warnings`、`fmt --all -- --check` 也重新执行。相应日志后缀为 `*-postreview-20261118.log`，不会将较早的全量日志写成更新后的源码指纹。

最终一/双 worker 的 debug/release 三轮通过，57 组伪造历史全部拒绝；已有 trace 字节保持，非法 mode/worker 在创建 trace 前拒绝。修改捕获驱动后 v10 的三种状态 Wings/BEAM/Python 三轮、34 组伪造历史、非法 workload、独占和失败证据保留再次通过，日志 `/tmp/wings-recovery-reuse-v10-regression-20261118.log`。

## 原生 macOS 长测

冻结 release runner、产物、源码和独立 oracle，按一 worker 后两 worker 的顺序各执行一次 30s/60s、900 轮，没有与本项目构建/测试并行。原始目录 `target/recovery-soak-macos-20261118/` 保存每轮请求/状态/失败记录、stdout/stderr、manifest、环境和 RSS 安全观测；每配置退出后重新逐行重放，复核全部冻结 SHA，并保留 `workers-{1,2}/independent-replay.json`。两配置均为 300 轮预热、600 轮采样，无 stderr/invalid，未作五对重复或吞吐/内存验收。

| 采样段（600 轮） | 一 worker | 两 worker |
|---|---:|---:|
| 排队请求超时 | 2400 | 2400 |
| 排队请求取消 | 6718 | 6655 |
| 排队请求停止失败 | 482 | 545 |
| 实际自动重启 | 600 | 600 |
| 后续强度超限 | 600 | 600 |

两端每轮健康目标均提交 +7；两个取消结果的 Fail 仍实际失败、候选 99 不提交、第一次全新状态为零，第二次强度超限。每配置 900 轮共 1800 个 Fail 结果均观测为 Cancelled；真实监督事件仍各 1800 个，模型和短回归允许合法的 Failed 竞争结果，没有伪称长测已观测到所有交错。每配置旧完整 Pid 检查 7196 次，child ID 另逐项拒绝，下一轮新目标存活时同样验证。

一/双 worker 冷账面为 4593497/7061473 B，逐轮逻辑保留为 5150553/7618529 B，均恰为额外两个 278528 B 栈槽；process/message/request 每轮为零。最终环境关闭后根从 232 B 回到 232 B。全段最长一轮为 30.986/28.458ms，含活栈、GC、线程竞争、失败、重启和清理，均在原三秒内；不当作消息尾延迟或性能领先结论。

两个 trace 分别为 1671433/1671072 B，SHA-256 为 `1c26f7087d1680d1bb6267fdf8733cd916696d2a327d91df01e90a0f637edd52` / `f97128251fc6acc214ee6ef000cf1bb247c1b6376162193e95b4e817f30e80a6`。日志 `/tmp/wings-recovery-soak-long-20261118.log`；全部单调时间、迟发和完整分组保留在原始 trace 与独立重放结果。

## 未完成门禁

等价 BEAM 恢复业务、长时过载/服务 I/O 竞争、完整资源与平台内存、监测偏差、CPU 硬配额、主机隔离/热状态和最新原生双平台仍待独立验收。不能从持续生命周期通过推导吞吐或整机内存领先，MVP 保持未完成。
