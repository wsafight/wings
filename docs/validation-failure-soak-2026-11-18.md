# 持续失败生命周期验证（2026-11-18）

## 范围与条件

基于 `46917a4`，契约见 [ADR-0120](adr/0120-persistent-concurrent-failure-soak.md)，协议见 [v10](beam-workloads.md#v10持续环境的失败生命周期)。本机 Apple M3 Pro、36 GiB、macOS 26.6.2 AArch64、Rust 1.99.1、OTP 29。原 v9 仍保留两个 sender、窗口各 16、mailbox 32、256 请求和原三秒阶段；公共 Rust/BEAM 业务核心同时服务原短入口和新增持续入口，v9 的真实三端 27 组、96 枚举/24 伪造历史及 18/51 协议拒绝已通过。

三种 Bytes/binary 状态的真实 Wings/BEAM/Python 三轮回归通过：同一 Scheduler/supervisor 内复用槽，失败旧提交前缀、健康目标完整业务、上一代入口拒绝和账目回收。独立模型另拒绝 34 组伪造历史，覆盖轮次/时钟/短采样、前缀/假失败、缓存/三维资源和清理；四种不满足“两 sender、失败、健康业务”的输入拒绝，已有 trace 独占保护和驱动失败保留通过。此处是短回归，没有把三轮当作 90 秒长测。

初期编译发现执行栈常量不是公开 API，改为从实际计费 StackPool 的公开 stats 取得固定槽大小，并释放该探测池后记录冷基线；没有为测试新增 VM API。初期 clippy 指出抽取 helper 后多余的借用，已修正；相应编译/clippy 失败日志保留 `/tmp/wings-failure-soak-check-initial-20261118.log` 和 `/tmp/wings-failure-soak-clippy-initial-20261118.log`。

## 验证口径

普通三轮每子进程 CPU 5 秒/墙钟 10 秒/core 0，每轮三秒不变；全量仍按原 CPU 20 秒/墙钟 30 秒运行 harness/AOT 子命令。显式长测才用 30 秒预热、60 秒采样和每 100ms 计划的 900 轮，CPU 240 秒/墙钟 130 秒/总阶段 120 秒/每轮 3 秒/core 0，64 MiB trace 和 1 GiB RSS 安全停止。原 VM/tenant 256 MiB、process 4 MiB、两 worker、堆/栈/执行预留不变。

每轮全部到达、回复/失败/关闭、counter/两 sender 序号与下一条注入失败必须有合法 FIFO 交错；不能仅核对最终加法。Wings 保留冷控制和至多四个实际 guarded stack 槽，逐轮 process/message/request 为零，最终回到实际根启动基线。BEAM 以 monitor/DOWN、旧 gate 拒绝和空尾消息核对终止，进程/port 数只作本端观察。没有将显式新建说成自动恢复，没有将逻辑字节、RSS、物理占用或完整状态导出混同。

`cached_stacks` 来自账面差额除以已知槽字节，未直接普查各 worker 的映射；它验证有界账面等式，不能排除所有同尺寸漏计，也不替代完整资源账本或内存验收。

## 全量回归

冻结代码后的 debug/release 各通过 100 个 harness、679 项测试及完整 `verify_scalar_aot.py`，其中新增持续入口只跑三轮普通回归。两种 profile 的 doctest 通过，workspace/AOT 验证驱动分别通过 8/6 项测试。

`cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、debug/release 的 bins/examples 构建、Rust 1.85 全 workspace/all-targets 和 Linux x86-64 的 VM/all-targets 交叉检查通过。日志为 `/tmp/wings-failure-soak-{check-final,clippy-final,build-debug,build-release,msrv,linux-cross,fmt}-20261118.log`；完整测试日志为 `/tmp/wings-failure-soak-{workspace-debug,aot-debug,workspace-release,aot-release,doctest-debug,doctest-release,runner-workspace,runner-aot}-20261118.log`。Linux 交叉检查不等于原生执行。

最终 release 产物再次通过真实 v9 三端全部 27 组与 v10 三端三种状态的三轮回归；日志为 `/tmp/wings-failure-soak-v{9,10}-release-final-20261118.log`。

## 原生 macOS 长测

256 B、4 KiB、64 KiB 三组每端单独执行一次固定 30s/60s、900 轮，顺序为 Wings 后 BEAM，未与本项目构建或其他测试并行；共 5400 轮、1382400 条输入结果，全部通过。它验证持续失败生命周期，没有五对重复或吞吐饱和扫描。原始目录为 `target/failure-soak-macos-{256,4k,64k}-20261118/`，包含冻结副本/输入、环境、每端逐轮 trace、控制输出、stderr、RSS 安全观测和 manifest；每组的 `independent-replay.json` 是退出后重新逐条重放并复核全部冻结 SHA 的结果。

| 4 KiB 采样段 | Wings | BEAM |
|---|---:|---:|
| 完整轮数 | 600 | 600 |
| 正常回复 | 109200 | 109200 |
| 已接收后失败 | 5711 | 6000 |
| 关闭后拒绝 | 38689 | 38400 |
| 注入失败目标 | 600 | 600 |

两端预热均为 300 轮；每端完整 900 轮共检查旧身份 5397 次，全部通过合法失败前缀、健康目标业务与逐轮清理。竞争下 `failed`/`closed` 分类数可以不同，未要求两端的跨 sender 交错相同。Wings 每轮逻辑保留 8186304 B，即冷基线 7072192 B 加四个 278528 B 固定槽；process/message/request 为零，环境关闭后从根 232 B 回到 232 B。BEAM 每轮结束后的进程数为 43、port 数为 1，仅为本端观察。整段最长一轮分别为 10.387ms/10.898ms，均低于原 3 秒；这些含创建、失败和清理的单轮时间不是消息尾延迟或吞吐门禁。

4 KiB 两份 trace SHA-256 分别为 `63694df99151c68a97a68463bd85a47ec50220c7455834c815107c675fdeafb4`（Wings）、`b79cc54295209c10f8cc39d2003669f374b4038a4fa7c8765549a5a6ddaf7fb8`（BEAM）；文件分别为 14492174/14552574 B，未触及 64 MiB 上限。

其余两组采样段如下；两端均保留 300 轮预热、600 轮采样和 5397 次旧身份检查。每组最终 Wings 根均为 232 B；BEAM 结束观察均为 43 个进程/1 个 port。

| 状态/运行端 | 回复 | 已接收后失败 | 关闭后拒绝 | 全段最长一轮 | 逐轮 Wings 逻辑保留 |
|---|---:|---:|---:|---:|---:|
| 256 B / Wings | 109200 | 5236 | 39164 | 8.748ms | 7629248 B（两栈槽） |
| 256 B / BEAM | 109200 | 5988 | 38412 | 1.798ms | 不适用 |
| 64 KiB / Wings | 109200 | 6000 | 38400 | 16.433ms | 8186304 B（四栈槽） |
| 64 KiB / BEAM | 109200 | 6000 | 38400 | 5.905ms | 不适用 |

256 B 的 Wings/BEAM trace SHA-256 为 `3ea36292ea9a7825ff7122b8b638ace4d4dd6e51bf61e996de058bab04d47a2c` / `9dcd7294a76cf299bc2728726410eabbbf217e1d81c32416e1a6d685cdbe5024`；64 KiB 为 `fcc0c2c2b4a260740a200f24995107f6451803a08eeb733f668b64b3b97ed9cb` / `ba596fabc0cac0f3ddb04f2e9c21ebd7bcfbd613d64af9ce331cd4f31b51085a`。文件大小/完整分组/计划迟发保留在各独立重放结果；六端均无 stderr 或 invalid manifest。原始日志为 `/tmp/wings-failure-soak-long-{256,4k,64k}-20261118.log`。

## 尚未完成的门禁

长时 timeout/cancel、自动监督重启强度、过载竞争与服务 I/O 不在本切片；CPU 硬配额、主机/热/电源隔离、完整平台内存和监测偏差、最新原生双平台及各平台同机同负载 BEAM 吞吐/整机内存双目标仍待验收，MVP 未完成。不推送或发布。
