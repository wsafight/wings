# 2026-11-16 计时器通知优化验证

[文档目录](../docs.md) · [前一轮轮询优化](validation-optimization-2026-11-16.md) · [ADR-0100](adr/0100-coalesced-deadline-timer-notifications.md) · [机器可读汇总](optimization-timer-notify-macos-2026-11-16.json)

## 结论

计时通知合并候选已回退，未作为优化保留。hot-queue 10000/s 五对采样中，候选共 1240 次超时、41747 次队满，原版无超时、2485 次队满；候选 p99 仅 2/5 对较低，p999 仅 1/5 对较低，现有数据不支持稳定收益。前两对发生器迟发明显不同，不能把全部差异归因给 timer，但也不能只保留最后一对或用中位数掩盖失败。

当前源码恢复原有每次 arm/disarm 无条件通知，保留 ADR-0099 的未就绪结果轮询快路径。冻结的失败候选、全部十轮原始事件/内存及统计仍保留。这不是 MVP、吞吐上限或 BEAM 内存/性能门禁通过记录。

## 假设与范围

候选在已有 ADR-0099 上，尝试消除每时间片 disarm 的无条件计时线程通知，以及无需提前唤醒的 future arm 通知。实际等待期限与当前业务期限分开记录，仍使用同一控制锁和 Condvar；取消、抢占、预算、quantum、ABI 与资源上限不变。不把减少通知当作实测收益或全部尾延迟根因。

## 候选功能验证

四项新 timer 测试首次在 CPU 20s/墙钟 30s/core 0 子进程通过，覆盖通知决定表、真实长等待撤销/更早重置、空闲与过期 arm、延后取消旧期限和长等待关闭。原控制大小另有固定上限断言，不提高逻辑预算。

- debug/release 各 **88 个独立 harness、592 项测试通过**；先检查构建成功，再逐个执行，`--no-run` 本身不算测试通过。
- 两种构建的完整受限 AOT 均通过；另将四项新 timer 回归每种构建重复八次，共 64 次测试执行通过。每 harness CPU 20s/墙钟 30s/core 0、原业务三秒阶段不变。
- `cargo check --workspace --locked`、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 与 workspace debug doc-tests 通过。
- release 真实 Wings/OTP 29/Python 的开环 smoke 6 组计划/10 项配置拒绝、并发失败 27 组场景/96 条枚举历史/24 条伪造历史/18 项桥接与 51 项 JSON 拒绝、fairness 两组精确计划和独立原子观测均通过。这些不是长时公平性或性能验收。

可复用验证命令：

```sh
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --workspace --locked --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/timer-open-loop-smoke-20261116
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/timer-fairness-smoke-20261116
```

日志为 `/tmp/wings-timer-tests.log`、`/tmp/wings-timer-aot-{debug,release}.log`、`/tmp/wings-timer-repeat.log`；候选功能回归无失败。592 项属于已回退候选，不能当作当前源码的测试数。

## 配对实验

平台为 macOS 26.6.2 AArch64、Apple M3 Pro，Rust 1.99.1。固定 hot-queue：100 actor × 4096 字节状态、100 活跃、mailbox 100、pending 256、32 B 逻辑业务记录、两个 worker、10000 次/秒；每 100 个计划请求中 90 个发往热点 actor，其余分给另外 99 个。两版均含 ADR-0099，业务产物/发生器/计量协议相同，只比较计时通知；原版使用前一轮已冻结的 candidate 二进制及其源码，不回退到最初未优化轮询版。

每轮 30s 预热/60s 采样，五对交替次序，运行时不有意并行启动本项目构建/测试。CPU 240s/墙钟 130s/阶段 120s/core 0、每文件 10 GiB 和观测 RSS 1 GiB 安全停止不变；不降低工作量、放宽尾延迟或删拒绝/超时。逐条离线验证、最终状态与启动基线、固定源码/二进制/产物哈希及全部重复均保留。

驱动为 `target/optimization-timer-notify-20261116/compare.py`，冻结副本在输出目录 `target/optimization-timer-notify-20261116/paired/frozen/`；原始事件、内存、stdout/stderr、每轮 manifest 和汇总保留在 `paired/`。启动前检查前一轮原版全部 712 个输入哈希，并确认本轮两版构建/运行代码只有 `crates/wings-vm/src/timer.rs` 不同。本轮冻结 718 个输入，十轮结束后及独立重放时全部复核通过。

原版二进制 SHA-256 为 `cb5e81b6f6bb15c094fb176ccab6aa8411cbd371dbf6da7dda05f6876f15fd77`，候选为 `0f953bcd6d09365234ef8c9e45e7dc72c4ac2f557f524301adfab480d9d8525e`。基准提交为 `f8cab8c`，两版另外均含尚未提交的 ADR-0099，不能仅凭提交号重建它们。

### 延迟、拒绝与超时

每轮采样计划 600000 请求、预热计划 300000 请求。延迟从计划发送时间到终态观测，包含发生器迟发；分位覆盖已接收请求的按时回复和超时，拒绝另列，不当作零延迟成功。单位为 ms，表内保留六位小数；机器可读汇总保留整数 ns。次序列出每对实际先后。

| 配对 | 次序 | 版本 | p99 ms | p999 ms | 按时回复 | 超时 | 队满 |
|---|---|---|---:|---:|---:|---:|---:|
| 1 | 先 | 原版 | 2.062000 | 23.779958 | 597871 | 0 | 2129 |
| 1 | 后 | 候选 | 34.573833 | 119.871167 | 565021 | 1191 | 33788 |
| 2 | 先 | 候选 | 12.246125 | 48.087458 | 592001 | 49 | 7950 |
| 2 | 后 | 原版 | 0.104250 | 9.337416 | 599647 | 0 | 353 |
| 3 | 先 | 原版 | 0.078083 | 3.453417 | 599997 | 0 | 3 |
| 3 | 后 | 候选 | 0.074042 | 4.919417 | 599991 | 0 | 9 |
| 4 | 先 | 候选 | 0.059333 | 2.114292 | 600000 | 0 | 0 |
| 4 | 后 | 原版 | 0.045959 | 0.445084 | 600000 | 0 | 0 |
| 5 | 先 | 原版 | 0.077333 | 3.942708 | 600000 | 0 | 0 |
| 5 | 后 | 候选 | 0.053334 | 0.218459 | 600000 | 0 | 0 |

五轮采样各计划合计 3000000 请求：原版按时 2997515、超时 0、队满 2485；候选按时 2957013、超时 1240、队满 41747。两版 pending 满均为 0。预热也不删失：原版按时 1498861、队满 1139、无超时；候选按时 1491785、超时 239、队满 7976。

逐轮 p99 的五次中位数为原版 0.078083 ms、候选 0.074042 ms，逐轮 p999 的中位数为 3.942708/4.919417 ms；它们不是合并请求分位，也不是加速倍数。前两对候选发生器迟发 p99 为 11.494333/4.154875 ms，原版为 0.630458/0.037208 ms；发生器最大迟发候选为 175.859333/51.414125 ms。主机未隔离，不能断言是通知改动单独造成差异。

原采样尾延迟/拒绝筛选两版均仅 2/5 轮通过，不把原版无超时写成吞吐门禁通过。十轮业务轨迹有效：原始事件逐条独立重放、最终各 actor 状态、超时后仍收齐、pending 清空和回收至 168 字节启动基线均验证通过；这与存在业务超时/拒绝并不矛盾。独立核验日志为 `/tmp/wings-timer-offline.log`，配对日志为 `/tmp/wings-timer-paired.log`。

### 分口径内存

| 版本 | 完整/总采样 | 每轮采样 RSS 峰值范围 MiB | 每轮 physical footprint 峰值范围 MiB |
|---|---:|---:|---:|
| 原版 | 449/449 | 44.984375–71.234375 | 35.907394–38.782372 |
| 候选 | 446/446 | 54.859375–72.578125 | 36.188622–38.422997 |

十轮共 895 个样本，已核验每轮样本数、各口径峰值、进程身份和错误记录；无 census/probe 不完整样本、主机探针报错或身份变化。汇总另保留逐 PID/start_id 峰值、初始化/预热/采样分期数、主机压缩/swap 范围和 helper 成本。表中峰值跨整轮采样记录，不仅是 60s 业务区间；RSS 与 footprint 分别统计，不能合并。

观测范围是发生器及捕获到的后代，非原子快照且不含 helper，安全停止 RSS 另含保守 helper 余量；不是 VM 独占或整机内存。主机压缩与 swap 不归因给 VM，样本完整也不证明连续覆盖或观察扰动相等。数据没有证明候选降低内存。

## 回退与当前源码

只撤销 `timer.rs` 候选及依赖候选状态的四项测试，保留原 timer/worker/Scheduler 抢占回归；不改量子、资源上限或时限。当前全部 228 个构建/运行源文件哈希与本轮冻结的原版一致，ADR-0099 不受影响。冻结目录仍保存候选源码和二进制，不用当前重建产物覆盖失败证据。

回退后重新构建 debug/release 的全部 bins/examples 与 harness，各 **88 个独立 harness、588 项测试通过**；两种构建的完整受限 AOT 均首次通过。本轮没有提高 CPU 20s/墙钟 30s/core 0 或任何业务阶段时限，未用候选的 592 项通过替代当前结果。

`cargo check --workspace --locked`、all-targets clippy `-D warnings`、fmt、Rust 1.85.0 all-targets check 与 workspace debug doc-tests 检查通过；当前 doc-test harness 均无测试，不额外计入 588 项。LSP/VM 依赖边界与 corosensei 禁 unwind feature 检查通过；变更 Markdown 围栏/相对链接、JSON 解析、Python AST 和 `git diff --check` 通过。

重建 release 后再次执行真实 Wings/OTP 29/Python：开环 smoke 6 组计划/10 项配置拒绝、并发失败 27 组场景/96 条枚举历史/24 条伪造历史/18 项桥接与 51 项 JSON 拒绝、fairness 两组精确计划和独立原子观测全部通过。Scheduler 让出观测仍为零样本，不报告零延迟或长时公平性通过。复现采用前述相同命令，开环/fairness 输出目录分别换为 `target/timer-rollback-open-loop-20261116`、`target/timer-rollback-fairness-20261116`，避免覆盖候选证据。

日志为 `/tmp/wings-timer-rollback-tests.log`、`/tmp/wings-timer-rollback-aot-{debug,release}.log`、`/tmp/wings-timer-rollback-{check,clippy,fmt,msrv,doctests,open-loop,failure,fairness}.log`，本轮回退验证无失败。未新增提交或推送，未改动用户的 `docs/proposals/`。

本机仍无 CPU 硬配额/可靠隔离，原生 Linux 和同业务 BEAM 双目标未验收。固定速率实验不能推出吞吐上限；重新尝试前应先补发生器迟发/CPU 竞争与实际 timer 开销的归因证据，不继续盲改等待策略。
