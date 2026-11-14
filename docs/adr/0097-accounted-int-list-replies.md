# ADR-0097：计费的有界 IntList 回复

- 日期：2026-11-15
- 状态：已采纳（计费列表回复切片；平台验证另记，MVP 尚未完成）
- 部分替代：[ADR-0096](0096-copied-record-replies.md) 对 IntList 回复的限制；Bytes 和通用记录回复另行实现。

## 决定

沿现有 `IntList` 类型开放回复与精确匹配的 `(RequestId, Int, IntList)` 单向完成入口，单次回复最多 64 个整数，顺序与列表一致。没有新语法、共享业务堆、阻塞调用或外部堆编号。原 Int/Bool/Unit/RecordInt 回复保持，列表结果使用显式拥有者，不将段编码伪装成整数结果。

生成入口返回后，runtime 仍在原协程内为候选引用状态与回复建立出口 shadow frame，逐节点沿既有 runtime checkpoint 检查取消、总预算与时间片，复制列表内容；正常完成全部检查、预留与复制后才能提交候选状态。出口帧在暂停期间保留，失败/取消正常返回后再解除，不用 Rust unwind 丢弃活栈。64 个元素和末尾空节点均须验证；第 65 个元素正常返回 ReplyRejected，无效引用正常失败，不提交状态或成功结果。

独立的 `IntListReply` 对象按目标进程/租户/VM 在分配前预留控制与精确内容容量。空列表只持控制容量，不在每个请求槽内放一份 64 元素数组。对象拥有不可变内容，Clone 仅共享已计费的结果存储；它不持 actor、模块、业务堆或执行栈。最后结果拥有者释放才归还预留，目标退出和结果被消费不能提前归还仍保留的内容容量。结果预留失败沿受管 HeapExhausted 失败，不提交候选。

`RequestOutcome::ReplyList` 和 `ExecutionStatus::CompleteList` 携带拥有的结果；原标量成功变体保持。同步宿主使用显式 `invoke_list`，原整数调用在执行前拒绝列表入口；低层 ScalarExecution/ScalarTask 可直接消费 CompleteList。JSON 输出 `{"reply":{"int_list":[...]}}`，输入仍不接受列表或系统 RequestId。固定 ingress 与 Scheduler 共享结果表按 schema 保留 ListRef 类型，结果在目标停止/关闭后仍可消费，结果类型不借此开放输入引用。

语言完成信封将至多 64 个整数复制到已预留的原 ListPayload，第三参数 mask 为 4，使用原列表导入/入口根机制在发起方计费堆重建。非成功终态导入空列表；必须先判 status。队满保留结果和完成路由，发起方退出/导入失败、目标退出、超时/取消与迟到结果沿原唯一终态和发布者票据规则清理，不撤回目标消息或副作用。

布局不变，继续 ABI v15。新增 ReplyRejected 状态只用于新增列表回复路径；旧 runtime 在清单检查拒绝新增回复/完成签名。对可信 native 内部编号仍只检查当前堆槽号/代次/布局，来源边界沿 ADR-0096，不声称能识别所有伪造的跨堆编号。

## 验证要求

检查器/共享 LSP、IR、Manifest、两种 runtime 独立验证精确类型与错配，Bytes 回复仍拒绝。独立测试结果预留的三级拒绝、空/最大/越界、容量与 Clone/Drop 生命周期；真实 AOT 验证内容顺序、i64 边界、GC/出口活栈、总预算/取消、65 元素拒绝及候选不提交。覆盖同步、任务、worker、supervisor、固定 ingress、一/二 Scheduler worker，以及成功/零超时/取消/目标失败、队满完成保留、发起方退出与导入失败，核对最终根基线。沿业务三秒阶段与 CPU 20s/墙钟 30s/core 0、workspace/debug/release/MSRV 和可用平台验证，不替代完整业务值/账本、原生双平台或同负载 BEAM 双门禁。

实际通过项、失败记录和原生/仿真边界见 [2026-11-15 验证记录](../validation-2026-11-15.md)。
