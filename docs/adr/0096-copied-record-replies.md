# ADR-0096：RecordInt 回复的独立内容复制

- 日期：2026-11-14
- 状态：已采纳（固定 RecordInt 内容回复切片；平台验证另记，MVP 尚未完成）
- 部分替代：[ADR-0095](0095-typed-scalar-request-events.md) 仅接受标量回复的限制；列表、字节和通用记录回复仍不开放。
- 后续部分替代：[ADR-0097](0097-accounted-int-list-replies.md) 开放计费的有界 IntList 回复；本文保留固定记录切片的原决策理由。

## 决定与边界

沿现有类型拼写开放 `-> RecordInt`，语言 request 的完成入口必须是单向 `(RequestId, Int, RecordInt)`。不增加语法、阻塞调用、外部堆 handle 或共享可变值。发送、状态、GC 和取消/超时生命周期仍遵守已有契约，语法评审稿不因此采纳。

生成入口的 ScalarOutput.reply 在声明 RecordInt 时仍是本目标堆的临时引用。runtime 在提交候选状态之前只在目标当前堆解析编号，检查存活性和 RecordInt 布局，再读取唯一 i64 字段；没有堆、当前堆无法解析、过期或其他布局的引用使目标 InvalidOutput，保留旧状态。既有编码只有槽号/代次，没有堆身份；可信 native 伪造的编号若恰与当前堆对象相同，只会解析为当前对象，不能据此证明编号来源。compiler 与复制通道保证正常引用保持本地，这不是可信 native 内存错误的隔离机制。

目标回复只复制此字段，跨进程、pending 表和宿主结果都不保存堆 handle。复制固定八字节、不分配业务对象、不调用 GC；从生成代码返回至检查/复制期间不得增加会 GC 的操作。

语言完成信封复用已准入的固定参数存储，第三项带现有 record mask，在发起方计费堆重建独立 RecordInt，沿普通记录消息的入口根和导入失败路径执行。Reply 的字段为真实内容；TimedOut/Cancelled/Failed 的字段为 0，导入后是 `record_int(0)`，仍须先判 status。发起方须显式配置计费堆；导入失败使发起方受管失败，不重新选择请求终态、不重发或撤回目标消息。原请求和完成票据继续占用至各自生命周期结束，发起方/目标退出、队满和取消均不得提前复用。

直接宿主 ScalarProcess/ScalarTask、worker、supervisor、Scheduler 和固定 ingress 的成功结果沿现有 `Some(i64)` / `RequestOutcome::Reply(i64)` 返回 RecordInt 的内容副本，由 handler schema 或 `reply_type()` 区分类型，不能将该整数当作 VM 堆引用。内容已内联在原结果槽，无新增可增长结果缓冲，也不让结果保留 actor、模块或目标堆。JSON 输出使用 `{"reply":{"record_int":N}}`；JSON 输入仍只接受原标量，不能伪造记录或系统 RequestId。

原成功/超时/取消/失败语义、副作用调用点生效和状态正常返回后提交均保持。compiler 根计划已将引用回复作为出口活值；必须以真实 AOT 和 GC 验证，不只依赖静态类型检查。ScalarContext、ScalarOutput 和 effect 布局/既有编码不变，继续 ABI v15；旧 runtime 在 Manifest 校验拒绝 RecordInt 回复/完成签名，旧三种标量产物继续兼容。

## 验证要求

独立验证前端/共享 LSP、IR、Manifest 的类型匹配、交叉错配与其余引用回复拒绝。runtime 检查无效回复不提交候选，普通 host/ingress 不接受原引用参数，系统完成入口不可伪造。真实 AOT 覆盖 i64 边界、临时与已提交记录、collect/活栈让出、宿主独立副本、完成处理再 collect、零超时/取消不撤回、目标失败和完成导入失败；分别验证 worker、监督、一/二 Scheduler worker 及退出后的根基线。保持业务三秒阶段及 CPU 20s/墙钟 30s/core 0，执行 workspace/debug/release/MSRV 和可用平台功能验证；本片不等同完整引用回复、业务值/账本、原生双平台或 BEAM 双性能门禁。
