# 2026-11-18 worker CPU 区间验证

对应 [ADR-0134](adr/0134-opt-in-worker-cpu-observation.md)，延续[同业务锁诊断](validation-open-loop-locks-2026-11-18.md)。只在已有可选锁观测启用时，worker 函数入口/退出读取两次本线程 CPU 时钟；新固定原子存储自动纳入原统计区先准入，默认不分配、不读 CPU 时钟。ABI v22、4352 B、业务、期限及资源上限保持。

## 验证范围

原生读取、非法/溢出/倒退/失败与合法零区间分别验证；Pending 不作为完成值，跨线程查询只在单原子终态发布后获得结果。guard 保持原线程，观察区随最后 handle 退账。一/双 worker 真实 AOT 保留正常状态、失败、取消及清理核对。工程、原限完整测试、MSRV 和交叉检查的实际结果另记，交叉检查不算原生 Linux。

## 已执行检查

本机 macOS 26.6.2 AArch64、Rust 1.99.1，顺序执行：

- CPU 三项、原锁六项 Rust 定向测试及九项 Python 模型通过；覆盖时钟状态/边界、默认关闭、退出发布、跨线程读取及原统计区三级准入与末 owner 退账。
- Debug/Release 各 104 个 harness、702 项测试在原 CPU 20s/墙钟 30s/core 0 的 OS 子进程通过；各包含原示例七项测试，不重复计算为新增测试数。
- 预构建入口的 Debug/Release 完整 AOT 通过，真实一/双 worker 的正常/失败状态、原锁次数与 join 后可用 CPU 值均核对；既有取消、超时、监督、GC 和消息/回复回归通过。
- `cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets 与 Linux x86-64 all-targets 交叉检查通过。没有原生 Linux 执行。
- 189 份 Markdown 相对链接/围栏、32 份 Python AST 和 `git diff --check` 通过；未读取或修改 `docs/proposals/`。

Rust 清单和逐 harness 日志在 `target/worker-cpu-workspace-{debug,release}-20261118/`，定向/构建/工程/AOT 日志为 `/tmp/wings-worker-cpu-*-20261118.log`，指纹摘要在 `target/worker-cpu-preflight-20261118/report.json`。本轮成功不覆盖先前原限启动超时；本切片没有修复其根因。

## 长测方式

复用 `measure_lock_statistics.py` 的显式独立入口；原 hot-queue 100×4096 B、mailbox 100、pending 256、两个 worker、80000/s，每轮 30s/60s、两对关闭/开启与反序，固定原 CPU 240s/墙钟 130s/阶段 120s。冻结驱动核对原始 40 B 事件、handler 数及全部两个 worker 的 CPU 终态，Unavailable/缺失使实验失效并保留原始输出，不重试或回填旧数据。

```sh
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_lock_statistics.py
WINGS_PROFILE=release python3 scripts/measure_lock_statistics.py --artifact target/worker-cpu-aot-new --output target/worker-cpu-new
```

产物须事先以 `wings-compile build examples/beam-multi-counter.wings` 构建。CPU 区间包含 worker 内初始化和清理，不包含 timer、发生器、其他线程及 OS 启动/退出；它不是六十秒采样 CPU、整机 CPU 或锁内 CPU，不能将持锁墙钟除以此值当 CPU 比例。两对诊断不替代五对正式性能门禁。

## 实际两对长测

冻结提交 `74678b96fddb09567c345ce001933e0e7911e886` 的 499 项输入，Apple M3 Pro/macOS 26.6.2 AArch64 顺序运行关闭/开启、开启/关闭。四轮各按原 30s/60s 与固定上限完成，原始事件再次独立重放、全部冻结输入及逐轮文件 SHA 核对通过，状态与根启动 232 B 清理核对通过。运行期间未并行启动本项目构建/测试，未隔离其他主机活动，也没有 CPU 硬配额。

每轮采样计划 4800000 次，下表延迟单位 ms，从计划到达到结果观察，包含发生器迟发：

| 对/统计 | 按时回复 | 队满 | pending 满 | 超时 | p99 | p999 |
|---|---:|---:|---:|---:|---:|---:|
| 1/关闭 | 4786556 | 13444 | 0 | 0 | 0.092667 | 5.098375 |
| 1/开启 | 4787847 | 12153 | 0 | 0 | 0.092458 | 4.862583 |
| 2/开启 | 4768760 | 31105 | 108 | 27 | 0.552833 | 5.573916 |
| 2/关闭 | 4777286 | 22714 | 0 | 0 | 0.237584 | 5.920084 |

每轮 2400000 次预热的队满依次为 16968、282632、5136、8723，预热 pending 满/超时均为 0；第一开启轮的预热拒绝不能删除。第二开启轮的 27 次超时未撤回已接受消息，最终仍收齐并核对实际提交值；108 次 pending 满仍为拒绝。四轮零拒绝候选均不通过，两对诊断不能确定统计开销，也不证明吞吐改善。

两个开启轮的完整 worker 函数 CPU 区间为：

| 开启轮 | worker 0 CPU s | worker 1 CPU s | 两 worker 合计 s |
|---|---:|---:|---:|
| 1 | 43.639398375 | 43.577474541 | 87.216872916 |
| 2 | 45.616847542 | 45.359594333 | 90.976441875 |

两个 worker 均是 join 后的实际可用终态，无 Pending/Unavailable。它们不能除以六十秒当作采样 CPU，也不能当作整进程 CPU；关闭轮没有 CPU 观测，不能为其回填或据此计算开关的 CPU 差异。

第一开启轮的 claim/start/finish 次数为 22362516/6905315/6905315，snapshot 没有样本；第二开启轮为 23478565/7163751/7163751，snapshot 只有 1 次。start/finish 均恰好等于 Configure 加全部实际提交，含已超时后实际完成的请求。四阶段等待墙钟之和为 5.338295/5.367769s，持锁墙钟为 11.780379/12.066613s。CPU 区间和这些锁墙钟的范围、性质不同，不能相除当作锁内 CPU 比例；一次 snapshot 也不形成公平性分布或门禁。

进程树加 driver 的观测 RSS 峰值依次为 55607296、68698112、71073792、78331904 B，原生完整样本数为 89/89、88/88、89/89、89/89。driver 跨轮复用，不能当作 Wings 独占内存，也未做 BEAM 同轮比较或整机内存验收。

## 原始证据

`target/worker-cpu-locks-macos-20261118/` 保存冻结副本、逐轮事件/stdout/stderr/内存与 manifest，`report.json` 是原报告，`independent-replay.json` 保存再次独立重放和所有指纹；日志为 `/tmp/wings-worker-cpu-{measure,replay}-20261118.log`。

| 轮 | 事件字节 | SHA-256 |
|---|---:|---|
| 1/关闭 | 574783520 | `2959e4bbe1b6f71c167fae5a1468ea38346ff677bec5fdde946320fabc4e7def` |
| 1/开启 | 564208600 | `322af87ac8731802f0b07a1941d63bc271b4b19e1bc6401ed31fcae577e85b73` |
| 2/开启 | 574546040 | `e1546844d9e2bb396f22ca0d74bc1f5d9c0e27c2b16ce68f2b97841d95f868da` |
| 2/关闭 | 574742520 | `c7782157b3038c116389d91e16cba0440de6dd883c23a726ad705b143510db68` |

完整资源账本、启动超时根因、公平/过载/服务 I/O、CPU 硬配额、原生双平台与 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
