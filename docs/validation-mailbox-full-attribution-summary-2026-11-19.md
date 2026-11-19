# 有界 mailbox_full 关联摘要验证（2026-11-19）

按 [ADR-0147](adr/0147-bounded-mailbox-full-attribution.md) 补充第 5 项离线证据，基于 `afee47d`。只修改两个 Python 收集器和有界模型；不修改 VM、Rust/BEAM runner、40 B trace、业务协议、测量期限或门禁判定，不重跑长测。

## 模型与开发失败

Debug/Release 的 `verify_measurement.py` 与 `verify_fairness_measurement.py` 均通过。合成模型覆盖：首条到达的 null 间隔、warmup/sample 阶段、非零发生器迟发、前一 admission 间隔、1 起始 actor、1024 条保留及第 1025 条显式截断。原状态/FIFO/期限/资源收据和错误产物用例同时保持。

首轮 open-loop 模型把只有一个非零值的 60000 样本 p99.9 误写为最大值；实际 p99.9 仍为 0，改为分别核对 p99.9=0 与 max=123456ns。第二次失败是 sparse-256 的 actor 期望误写为 4000，按既有 `index % 100 + 1` 修正为 100 后通过。两处都是新增测试期望错误，未改收集器去迎合断言。Python 语法、220 份 Markdown 的相对链接/围栏、`git diff --check`、workspace all-target check、clippy `-D warnings`、fmt 和 Release 完整 `verify_scalar_aot.py` 均通过。

## 冻结 open-loop run-2 重放

只读重放 `target/mailbox-full-diag-20261119/run-2.events.bin` 与原 result，新摘要删除新增字段后与原 manifest **逐字段完全一致**：采样 mailbox_full=211、warmup=0、tail_candidate_pass=false，trace SHA-256 和全部分位不变。

211 条全部保留且未截断，全部属于采样段 actor 1：sent 窗口 32230854042..32231703083ns，跨度 849041ns；前一到达间隔中位数 1000ns、最大 17416ns。拒绝事件自身迟发范围 3083..28854042ns，首次拒绝迟发 28854042ns、前一间隔 1125ns。它与[原逐条归因](validation-mailbox-full-attribution-2026-11-19.md)一致：迟发后的追时刻密集补发与热 actor 满槽同现；拒绝对象的迟发随补发追时刻而下降，不要求每条都仍有数十毫秒迟发。OS 延迟来源仍未定位。

## 冻结 fairness 第 2 对重放与候选修正

只读重放 `target/fairness-measure-final-20261119/2-wings.events.bin` 后，除新增字段外历史摘要同样**逐字段完全一致**：采样 mailbox_full=56、warmup=0、tail_candidate_pass=false。

56 条全部保留且未截断，全部属于采样段、planned 均为 51690000000ns，即同一个 10ms 同刻 burst 的 sequence 516945..517000、actor 46..101。sent 窗口仅 121667ns，前一到达间隔中位数 916.5ns、最大 9416ns；拒绝事件自身迟发只有 102583..224250ns。这里没有 open-loop 的数十毫秒迟发后补发特征，不能再仅因全轮最高迟发 9.86ms 同现就把它称为同一机制候选。同刻 burst 下的排队/准入压力仍是候选，但当前字段不能区分前次消息未排尽、调度进展或其他背压，需另行取证；不得改写成已证明 VM 根因。

派生核对汇总保存在 `target/mailbox-full-attribution-replay-20261119.json`，冻结 trace/manifest 未改写。旧记录只保留当时证据与结论边界，本节及原记录新增的后续链接明确修正 fairness 候选。

## 范围

关联摘要改善可复核性，不删除任何拒绝、不放宽尾候选、不证明公平性或最大可持续吞吐。完整公平/过载/服务故障、CPU 硬配额、完整原生账本、原生双平台和 BEAM 吞吐/整机内存门禁仍未完成。CI 状态另补。
