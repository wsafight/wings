# Scheduler finish 条件广播候选验证（2026-11-20）

按 [ADR-0152](adr/0152-scheduler-finish-ready-notification-candidate.md) 单独实验 `finish` 只在新增 ready 时广播，基于 `fba107a`。结果表、timer、控制路径、业务和测量协议不变。

## 正确性前置

- `enqueue` 只在实际设置 `ready_worker` 后返回 true；ready 测试覆盖首次/重复、stale、owned、stopping 和 generation 替换。
- `maintain` 对每个 completion 独立投递后聚合结果，不使用短路。新增同次两个 completion 的回归，核对两条 ready、两个完成消息、重复维护返回 false、票据和账目清理；既有 mailbox 满、starting 和可投递路径分别核对 false/false/true。
- `finish` 先分别执行 `enqueue` 与 `maintain`，再按聚合结果通知。`claim` 内维护和 start error 的原 1ms 重查/退避不改；stop/close/cancel 与语言 effect 仍无条件通知。
- fairness 长测执行期间没有编译或运行测试，避免给配对测量增加 CPU 扰动。长测结束后 `cargo test --locked -p wings-vm` **425 项通过**，workspace all-targets Clippy `-D warnings` 通过，fmt 已应用；Release runner 构建成功。

## 冻结 A/B

复用 `target/openloop-artifact-cfg-20261120` 和已冻结的原 runtime runner（SHA-256 `ab1bceb286a2c5637c87bc132625d65fee34d9a74cadffb712f5487c16ef0f85`），候选 runner SHA-256 为 `def99746d640b14a7ea52ae18e9b07ae3cb64a50d96135ea7f3798bfaddb0d10`。hot-queue 10000/s、30s/60s、五对交替由 `scripts/measure_reply_candidate.py` 执行，逐轮核对 runner、产物、源码和探针 hash；输出为 `target/scheduler-finish-ab-20261120/run/`。

## 五对结果与回退

十轮全部完成，原始报告 `target/scheduler-finish-ab-20261120/run/report.json` 的 SHA-256 为 `a92bfd93aa5ff294cff86ea6ecf3164ed18d27edfa353539440a6aeed9cca2b7`。每轮冻结输入前后 hash 一致，全部实际结果/状态收齐；失败轮次不删除、不重跑。

| 对 | 基线尾/拒绝/超时 | 候选尾/拒绝/超时 | 基线 p99 端到端/服务 (ns) | 候选 p99 端到端/服务 (ns) | 基线服务 p999 | 候选服务 p999 |
|---|---|---|---|---|---|---|
| 1 | 通过/0/0 | **未过/281/0** | 45875/44083 | 51833/48333 | 94250 | 307542 |
| 2 | **未过/15/0** | 通过/0/0 | 90667/80542 | 53125/48625 | 1491958 | 143458 |
| 3 | **未过/121/0** | 通过/0/0 | 226000/137542 | 54708/49084 | 3359666 | 242458 |
| 4 | **未过/35615/490** | 通过/0/0 | 41455291/34783583 | 73250/64125 | 84633083 | 321000 |
| 5 | **未过/144/0** | **未过/460/0** | 81208/74542 | 60084/52042 | 608791 | 1465334 |

基线/候选服务 p99 中位数为 80542/49084ns，端到端 p99 中位数为 90667/54708ns；候选在第 2–5 对服务 p99 较低，第 1 对较高。基线第 4 对严重退化，发生器迟发 p99 也达到 14356083ns，保留该现场，但不把单轮大幅差值外推成候选稳定机制收益。候选仍只有 3/5 尾候选通过，两次共 741 次拒绝，不能称为稳定全绿。

候选 RSS 相对当对基线四升一降，差值依次 +14794752/-2621440/+7176192/+18595840/+8388608 B；物理占用两升三降。候选服务分位与拒绝总数虽优于本次退化基线，但没有同时满足稳定尾候选与同轮内存改善，按预定条件回退。

已恢复四个 Scheduler 源码/测试文件到原无条件 `finish` 广播，候选只保存在冻结实验目录。恢复后原 **424 项**VM 测试、workspace all-targets Clippy 和 fmt 通过。该实验不否定所有调度通知优化，只否定当前候选在本轮双目标下的保留价值；不宣称最大持续吞吐、完整内存、公平性或 BEAM/MVP 门禁通过。
