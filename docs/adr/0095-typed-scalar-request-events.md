# ADR-0095：类型匹配的标量请求完成事件

- 日期：2026-11-14
- 状态：已采纳（标量完成事件切片；平台验证另记，MVP 尚未完成）
- RecordInt 内容回复后由 [ADR-0096](0096-copied-record-replies.md) 扩展，原标量和生命周期决定保持。
- 部分替代：[ADR-0040](0040-language-request-events.md) 仅接受 Int 回复与固定第三参数类型的限制。

## 决定与理由

宿主请求已有 Int/Bool/Unit 回复，语言异步 request 却只接受 Int，业务代码必须自行编码布尔和确认结果。沿现有 `request(pid, Message, Completed, timeout, args...)` 接入全部三种既有标量回复，不增加语法、类型拼写、阻塞调用或隐式转换。

目标必须有显式 `-> Int`、`-> Bool` 或 `-> Unit`；无箭头入口仍是单向消息，不能作为请求目标。完成入口必须为无回复的三个参数 `(RequestId, Int, T)`，其中 T 与目标声明的回复类型精确相同。不能把 Bool 或 Unit 静默送到 Int 完成入口；三个类型可以分别有不同完成入口。RequestId 仍不得在普通消息、状态或回复中使用，系统完成入口不能由普通 send、宿主或 JSON 伪造。

status 沿原协议：0=Reply、1=TimedOut、2=Cancelled、3=Failed。仅 Reply 的第三参数表示实际结果；其他终态用该类型的规范默认值，Int 为 0、Bool 为 false、Unit 为 ()，业务必须先判 status。回复输出仍在状态提交前校验，非法 Bool/Unit 输出使目标受管失败，不提交候选状态；失败完成入口仍收到 status=3 和规范默认值。

取消只对尚有 pending 完成路由的能力有效，已转为 mailbox 事件后取消沿原规则拒绝。跨 worker 子目标与取消竞争时，可以先选 Reply/Cancelled，也可能在取消调用前已投递完成事件、使发起 handler 因 RequestRejected 失败；不通过放宽能力有效期改变既有语义。同一 actor 的自请求取消在目标入口开始前确定发生，必须得到 Cancelled。

前端、IR、Manifest、单 worker 与 Scheduler 都检查对应边界；runtime 在预留请求/事件和发布目标消息之前核对 target/callback 类型。不只依赖编译器防止可信手写入口产生错配 effect。请求唯一终态、取消不撤回、deadline、队满保留、能力号、预算、三级预留、清理与恢复均按 ADR-0040/0078 保持。

context、EffectRequest、ScalarOutput 布局与标量编码不变，继续 ABI v15。旧 Int 完成产物仍可加载；旧 runtime 对含 Bool/Unit 完成签名的 Manifest 会在 RequestId 签名检查中拒绝，不把它解释成普通可发送入口。没有开放引用回复、通用结果联合类型或关联状态。

## 验证与重审

检查器/共享 LSP 分析验证三种匹配、交叉错配、单向目标、错误 status 类型和不可伪造完成入口；IR/Manifest 独立拒绝非法 kind/类型及参数协议。runtime 检查错配 effect 的发布前回滚，检查 Bool/Unit 输出的规范性与旧状态不提交。真实 AOT 通过自请求/子目标、正常 false/true/Unit、零超时、取消、目标失败及完成处理副作用，在一/二 worker 下核对终态、候选提交和最终清理。沿原三秒阶段与 CPU/墙钟上限，执行 workspace/debug/release/MSRV 和可用平台验证。此片不替代引用回复、完整业务值/账本、长时故障或原生双平台 BEAM 吞吐/整机内存门禁。
