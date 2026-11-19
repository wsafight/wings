# ADR-0146：长测运行端的产物 schema 前置核对

- 日期：2026-11-19
- 状态：已采纳（实验输入前置校验）

## 背景

开环扫描曾误用 `beam-counter.wings` 产物。runner 在创建 trace、装载模块、创建 Scheduler 和 spawn 完全部 actor 后，才在首次三参数 `Configure` 请求发现签名不符；0.83 秒后以通用 `scheduler handler identity, phase or arguments mismatch` 退出。fairness 有同类的延迟检查。错误产物没有开始 90 秒业务，但失败位置与诊断不能清楚地区分实验配置错误和运行期调度错误。

## 决定

v6 open-loop 与 v8 fairness 的 Wings 运行端在模块装载后、tenant/Scheduler/actor 创建和业务 `t0` 前核对：

- open-loop：state 为 `BytesRef`，`Configure(Int, Int, Int) -> Int`，`Apply(Int, Int, Int, Int) -> Int`。
- fairness：state 为 `BytesRef`，`Configure(Int) -> Int`，`Cpu(Int, Int, Int, Int) -> Int`，`Pulse(Int, Int, Int, Int) -> Int`。

不满足时分别返回固定的 `open-loop artifact schema mismatch` 或 `fairness artifact schema mismatch`。fairness 先核对 state/Configure 再查找 Cpu/Pulse，使已知二参数 Counter 产物得到明确的 schema 错误；其他缺失 handler 仍沿用原未知 handler 错误。

trace 的独占创建仍在模块装载前完成，因此错误产物留下 0 B 新 trace；stdout 只有原 `oracle_error` 终态，不得出现 `start` 或 `sampling`。正确产物的事件编码、业务状态、actor/mailbox/pending、时钟、预算、资源限制、90 秒协议和停止判据全部不变。

## 范围与限制

两个验证 harness 都用真实 `beam-counter.wings` 错误产物覆盖固定拒绝、0 B trace 和无业务开始事件，另用原正确产物短回归覆盖接受路径。该切片只提前拒绝无效实验输入，不修复 VM，不改变既有长测结果，也不重新解释 mailbox_full。CPU 硬配额、过载分组、完整公平/故障、原生双平台和 BEAM/MVP 门禁保持开放。实际验证见[记录](../validation-measurement-schema-preflight-2026-11-19.md)。
