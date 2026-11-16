# 2026-11-16 空语言请求表维护优化验证

[文档目录](../docs.md) · [ADR-0101](adr/0101-empty-language-request-maintenance.md) · [前轮失败实验](validation-timer-optimization-2026-11-16.md) · [机器可读汇总](optimization-empty-requests-macos-2026-11-16.json)

## 结论

空语言请求表维护候选已回退，当前代码恢复 `8cc7878`。原负载五对采样中，候选 86 次超时、1350 次队满，原版 0 次超时、475 次队满；候选 p99 4/5 对较低，但 p999 仅 2/5 对较低，没有稳定端到端收益。不能凭少两轮扫描或某个中位数宣布优化成功。

## 基线与范围

按用户要求，先将宿主未就绪结果轮询优化和已回退的 timer 实验证据提交为 `8cc7878`，未推送；用户的 `docs/proposals/` 不纳入提交、冻结输入或本轮修改。

候选只实验 Scheduler 的空语言请求表维护快路径。原锁内存活槽计数随成功发布/最终双端回收更新，零计数在读取时钟和两次扫描前返回，非空表仍执行原算法。宿主请求不占语言表；两版均保留 ADR-0099 和原无条件 timer 通知，不改变业务、额度、片预算、quantum 或请求时限。候选新增计数包含于原控制计费尺寸，没有提高上限。

## 候选正确性验证

新增空表/末槽两端结束顺序、256 槽满表两轮复用与旧 publisher、关闭后保留 handle 三项回归；原 mailbox 满、超时/取消、owner/target/自请求退出、引用完成与 generation 耗尽测试补计数断言。既有真实语言 effect 的正常请求与发布前 schema 拒绝同时核对存活计数。只在目标 mailbox 接受后增加，不因完成事件入队而提前归还请求票据。

最初两项及原 Scheduler 定向测试在 CPU 20s/墙钟 30s/core 0 子进程中共 14 项通过；随后加入满表/旧 publisher 测试。三项新增回归在 debug/release 各重复八次，共 48 次测试执行通过。debug/release 各 **88 个独立 harness、591 项测试通过**，完整受限 AOT 均首次通过；check、all-targets clippy `-D warnings`、fmt 与 Rust 1.85.0 all-targets check 通过。workspace debug doc-test 检查通过，各 harness 无实际测试，不额外计数。依赖边界、禁 unwind feature、Markdown 相对链接/围栏、Python AST 与 `git diff --check` 通过。

release 真实 Wings/OTP 29/Python 的开环 smoke 6 组计划/10 项配置拒绝、并发失败 27 组场景/96 条枚举历史/24 条伪造历史/18 项桥接与 51 项 JSON 拒绝、fairness 两组精确计划及独立原子观测均通过。Scheduler 让出仍零样本，不报告零延迟或公平性门禁通过。本轮功能回归无失败；日志为 `/tmp/wings-empty-tests.log`、`/tmp/wings-empty-repeat.log`、`/tmp/wings-empty-aot-{debug,release}.log` 和 `/tmp/wings-empty-{check,clippy,fmt,msrv,doctests,open-loop,failure,fairness}.log`。

可复用命令（测试清单须另逐个隔离执行）：

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
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/empty-request-open-loop-20261116
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/empty-request-fairness-20261116
```

## 配对协议

实际使用 macOS 26.6.2 AArch64/Apple M3 Pro、Rust 1.99.1，hot-queue 100 actor × 4096 字节、100 活跃、mailbox 100、pending 256、32 B 逻辑记录、两个 worker、10000 次/秒；原 90% 热点路由不变。30s 预热/60s 采样，五对交替，CPU 240s/墙钟 130s/业务阶段 120s/core 0、每文件 10 GiB 和观测 RSS 1 GiB 安全停止不变。

驱动为 `target/optimization-empty-requests-20261116/compare.py`，输出到 `target/optimization-empty-requests-20261116/paired/`。先复核前轮原版全部 712 个冻结输入、确认全部已提交构建/运行源码与其一致，再从 `git archive 8cc7878` 固定原版源码；两版仅 Scheduler mod/state/tests 三文件不同。冻结源码、二进制、业务产物、采集器和独立汇总脚本共 726 个输入，十轮结束及独立重放时全部复核通过。原始文件 SHA-256 保存在汇总中。

原版二进制 SHA-256 为 `cb5e81b6f6bb15c094fb176ccab6aa8411cbd371dbf6da7dda05f6876f15fd77`，候选为 `b8d8ef8bc2fb8356cf9b96a0ef81168f6b6ed870e809eab8ae869935c2202ab8`。测量没有与本项目构建/测试有意重叠，不改变 driver，也不复用之前失败 timer 候选。每轮采样计划 600000 请求、预热 300000 请求，延迟从计划发送到实际回复观测，含发生器迟发，拒绝不混作零延迟成功。

### 五对实际结果

单位为 ms；汇总保留整数 ns。表内顺序为各对实际执行先后。

| 配对 | 次序 | 版本 | p99 ms | p999 ms | 按时回复 | 超时 | 队满 |
|---|---|---|---:|---:|---:|---:|---:|
| 1 | 先 | 原版 | 0.028500 | 1.530792 | 600000 | 0 | 0 |
| 1 | 后 | 候选 | 0.025709 | 0.276334 | 600000 | 0 | 0 |
| 2 | 先 | 候选 | 0.026500 | 0.837167 | 600000 | 0 | 0 |
| 2 | 后 | 原版 | 0.027083 | 0.142125 | 600000 | 0 | 0 |
| 3 | 先 | 原版 | 0.115041 | 4.339000 | 599816 | 0 | 184 |
| 3 | 后 | 候选 | 0.108583 | 10.631791 | 598631 | 86 | 1283 |
| 4 | 先 | 候选 | 0.068375 | 1.437500 | 600000 | 0 | 0 |
| 4 | 后 | 原版 | 0.044500 | 0.140167 | 600000 | 0 | 0 |
| 5 | 先 | 原版 | 0.082416 | 4.138000 | 599709 | 0 | 291 |
| 5 | 后 | 候选 | 0.072417 | 1.771959 | 599933 | 0 | 67 |

每端采样共 3000000 请求：原版按时 2999525、超时 0、队满 475；候选按时 2998564、超时 86、队满 1350。pending 满均为 0。预热原版 1500000 全部按时，候选按时 1499638、队满 362，无超时。既定尾延迟/零拒绝筛选两版各 3/5 轮通过，不等于吞吐门禁通过。

逐轮 p99 的五次中位数为原版 0.044500 ms、候选 0.068375 ms，p999 中位数为 1.530792/1.437500 ms；它们不是合并请求分位或加速倍数。第三对发生器迟发 p999 原版 3.125458 ms、候选 7.040541 ms，第四对为 0.054125/0.735458 ms，不能据未隔离主机数据断言全部差异由 VM 改动单独造成；也不能删除第三对超时来保留候选。

十轮逐条独立重放、最终各 actor 状态、超时后实际回复收齐、pending 清空、回收到 168 字节启动基线均通过；存在采样超时/拒绝与最终正确性通过不矛盾。日志为 `/tmp/wings-empty-paired.log`、`/tmp/wings-empty-offline.log`，独立脚本为冻结目录及实验目录的 `summarize.py`。

### 分口径内存

| 版本 | 完整/总样本 | 每轮 RSS 采样峰值范围 MiB | 每轮 physical footprint 采样峰值范围 MiB |
|---|---:|---:|---:|
| 原版 | 448/448 | 47.125000–71.953125 | 37.703972–40.516518 |
| 候选 | 448/448 | 54.687500–75.265625 | 38.922722–40.219803 |

896 份内存样本全部完整，无已观测身份变化或主机读取错误；独立核验了样本数和峰值，逐 PID/start_id、阶段数、主机 swap/压缩范围和 probe helper 成本保留在汇总中。峰值跨初始化/预热/采样整轮，不仅是 60s。观测包含 driver 和捕获后代，是非原子采样，helper 成本另列；不是 VM 独占或整机内存，没有候选降低内存的证据。

## 回退与当前代码

功能正确不等于值得保留。已撤销 mod/state/tests 三文件的候选与专用回归，当前沿已提交 `8cc7878` 原维护算法；候选源码/二进制留在 `paired/frozen/`，不能用重建产物覆盖。ADR-0099 保持，timer、driver、ABI、资源、预算、safepoint 和 deadline 均不变。

回退后重新构建两种 profile 的 bins/examples 与 harness，debug/release 各 **88 个独立 harness、588 项通过**，完整受限 AOT 均首次通过；原 CPU/墙钟/业务阶段不变，候选 591 项不混作当前计数。check、all-targets clippy `-D warnings`、fmt、MSRV all-targets check 和 workspace debug doc-test 检查通过。回退版真实 Wings/OTP 29/Python 的开环、并发失败和 fairness smoke 再次全部通过，规模/拒绝检查项与候选相同；零让出样本仍不作公平性结论。本轮候选与回退功能验证均无失败。

回退日志为 `/tmp/wings-empty-rollback-tests.log`、`/tmp/wings-empty-rollback-aot-{debug,release}.log` 和 `/tmp/wings-empty-rollback-{check,clippy,fmt,msrv,doctests,open-loop,failure,fairness}.log`；smoke 输出目录使用 `target/empty-request-rollback-{open-loop,fairness}-20261116`，不覆盖候选证据。全部 228 个构建/运行源文件与测量原版相同，重建 release 驱动 SHA-256 也与原版一致。失败实验与记录未另行提交或推送。

本机无 CPU 硬配额或已验证隔离；原生 Linux、长时公平/过载/故障、完整业务值/资源账本与同负载 BEAM 吞吐/整机内存仍未验收。固定速率结果不证明吞吐上限或 MVP，控制计数也不意味着 RSS 下降。
