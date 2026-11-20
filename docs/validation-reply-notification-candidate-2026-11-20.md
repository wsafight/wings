# 结果完成通知候选验证（2026-11-20）

按 [ADR-0151](adr/0151-reply-completion-notification-candidate.md) 单独实验结果表 finish 的无等待者通知省略，基于 `6ee73d6`。Scheduler、timer、业务、产物和测量协议不变。

## 正确性前置

- 首次 `cargo test --locked -p wings-vm ingress_requests` 保留 **21 通过、1 失败**：新增 `waiters` 后，`readiness_storage_is_admitted_before_allocation_at_each_tier` 仍按旧字节数计算，精确额度下创建结果表被拒绝。没有提高额度或重跑掩盖；测试随后显式加入 `size_of::<usize>()`。
- 新增两个真实等待者回归：主线程在同一把结果表锁下确认计数为 2 后分别发布 11/12，两个 `wait` 都实际收到对应回复，join 后计数和请求票据归零。既有零等待者发布、正 deadline、publisher Drop、cancel/发布竞争、迟到回复和槽复用保持。
- 修正后 ingress 定向 **23 项通过**；`cargo test --locked -p wings-vm` **425 项通过**，workspace all-targets Clippy `-D warnings` 通过，fmt 已应用。
- 独立复核随后发现实现与测试都把 `waiters` 多计了一次：`size_of::<ReplyTable>()` 已包含内联 `Mutex<ReplyState>`。首个 A/B 在完成前被显式终止，`target/reply-notify-ab-20261120/run/` 与日志保留并标记 `INVALIDATED.md`，不用于保留/回退判断。修复删除实现和测试的额外 `size_of::<usize>()`，没有提高额度。

## 冻结 A/B

基线 runner SHA-256 为 `ab1bceb286a2c5637c87bc132625d65fee34d9a74cadffb712f5487c16ef0f85`，已作废候选为 `e1f06dd2c9a6456e2b4ca53e33ce4cc0374c6bdd6ccd24f95649086b4ad9fcbd`。修正后候选为 `75e63e701f91792105618238342f2a6e3f1e06eeebc4a31fe77c0e7491ee7f27`，使用独立路径与全新 `target/reply-notify-ab-20261120/run-fixed/`，不复用部分结果；修正后 425 项 VM 测试与 Clippy 再次通过。

复用 `target/openloop-artifact-cfg-20261120`，hot-queue 10000/s、30s/60s、五对交替；`scripts/measure_reply_candidate.py` 冻结两个 runner、产物、源码和内存探针，逐轮核对 hash。

## 五对结果与回退

修正后十轮全部完成，原始报告为 `target/reply-notify-ab-20261120/run-fixed/report.json`，SHA-256 `82f212dddad4e7466ca9fbeca81221c5feec52914be5d865b4f7393bc6662822`。十轮均收齐实际结果、最终状态正确、零超时/零 pending_full；候选第 2/4 对的 170+135 次拒绝全部保留。

| 对 | 基线尾/拒绝 | 候选尾/拒绝 | 基线 p99 端到端/服务 (ns) | 候选 p99 端到端/服务 (ns) | 基线服务 p999 | 候选服务 p999 |
|---|---|---|---|---|---|---|
| 1 | 通过/0 | 通过/0 | 40875/39417 | 95708/80333 | 87750 | 910500 |
| 2 | 通过/0 | **未过/170** | 77875/70292 | 101333/85500 | 499750 | 903000 |
| 3 | 通过/0 | 通过/0 | 48084/45250 | 97667/82125 | 108542 | 847792 |
| 4 | 通过/0 | **未过/135** | 57958/52125 | 56417/50375 | 192250 | 377417 |
| 5 | 通过/0 | 通过/0 | 53084/48667 | 44208/43334 | 168875 | 80000 |

基线/候选服务 p99 中位数分别为 48667/80333ns，端到端 p99 中位数为 53084/95708ns；候选只在第 4/5 对服务 p99 略低，其余三对显著更高。候选 RSS 相对基线三升两降，物理占用同样没有稳定改善，不用不同轮次拼出内存收益。

结果满足预定回退条件，已恢复 `ingress_requests.rs` 到原无条件 finish 通知；候选源码只保存在冻结实验目录，不进入当前 runtime。恢复后 `cargo test --locked -p wings-vm` 的原 **424 项**测试通过，fmt/diff 与 228 份 Markdown 链接/围栏检查通过；随后重建 Release 并进入 fairness。该实验不证明通知成本不存在，只否定当前省略通知候选在本负载下的端到端收益；不归因发生器、OS 或 Condvar 内部机制，不宣称最大吞吐、完整内存或 BEAM/MVP 门禁通过。

回退与测量驱动提交为 `fba107ae7173504db9f9d4c800a519a347b7a78f`。push CI [`44733880540`](https://github.com/wsafight/wings/actions/runs/44733880540) 与 PR CI [`44733888504`](https://github.com/wsafight/wings/actions/runs/44733888504) 均成功。
