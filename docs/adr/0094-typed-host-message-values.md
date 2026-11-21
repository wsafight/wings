# ADR-0094：宿主的类型化复制消息

- 日期：2026-11-14
- 状态：已采纳（直接宿主消息切片；平台验证另记，MVP 尚未完成）
- 后续：[ADR-0115](0115-typed-cross-thread-ingress.md) 将相同内容复制检查接入固定跨线程入口，保留原标量方法拒绝原 handle。
- 扩展：[ADR-0049](0049-record-message-copies.md)、[ADR-0051](0051-bounded-int-list-messages.md)、[ADR-0076](0076-accounted-byte-messages.md)
- 部分替代：上述决定对直接宿主消息只能接收标量的限制；原始整数、JSON 和 IngressSender 接口的限制保留。

## 决定与理由

语言已经可以发送独立的 RecordInt、IntList 和 Bytes 副本，宿主仍不能将自己的业务数据安全地送入同一路径。为 ScalarWorker、ScalarSupervisor 和 SchedulerHandle 增加 `send_values`、`request_values`；参数使用借用的 `MessageValue`，显式区分 Int、Bool、Unit、RecordInt 的整数内容、整数列表切片和字节切片。不接受堆 handle、Pid、RequestId 或调用方提供的 mask/分段编码，不引入新的语言语法、类型拼写或 ABI。

先检查完整 schema、参数数量与所有合计边界，再分配复制存储。至多 64 个参数，全部列表合计至多 64 个整数、全部字节合计至多 65536 字节；零长度合法。标量值必须与声明类型精确匹配，Int(0/1) 不自动作为 Bool 或 Unit。记录字段、列表和字节复制到 VM 独占载荷，宿主修改或丢弃原切片不影响已经接受的消息。

独立字节载荷先向目标 process→tenant→VM 预留精确容量，再 fallible 分配；列表与记录使用既有 envelope 的固定存储。消息、请求票据和目标导入堆沿既有准入路径计费，拒绝、准备中目标退出、停止/关闭或发布竞争失败均归还未发布的预留。宿主调用栈及宿主提供的原始切片仍由调用方持有，不因此被宣称为 VM 管理的内存。

Scheduler 在 registry 锁内检查目标身份、phase、模块和入口模式，取得目标账户租约；在锁外完成至多 64 KiB 的有界宿主复制，再持锁重新验证完整 Pid/generation 和目标状态后发布。复制期间旧目标可能退出，账户租约保留其准备中载荷的计费，不能将副本发送到复用 slot 的新 actor。宿主复制不占 actor 的执行栈，不在持 registry 锁时执行长复制。

接收方仍在独占执行权下，通过既有入口根和分块导入复制到自己的精确非移动堆。目标没有堆或堆不足时沿既有受管失败路径处理；接受消息不等于 handler 已成功。请求期限从调用开始计算，超时、取消和结果丢弃不撤回已接受消息，只有正常返回才提交状态。回复保持 Int/Bool/Unit，引用回复、字符串、通用记录、JSON 编码扩展与 IngressSender 的复制载荷分别另行决策。

## 对照方案与验证

不开放原始引用整数：它们不能证明所有权、存活或跨堆安全。暂不把宿主内存零复制借给 actor：宿主可继续修改或释放原数据，且会扩大资源转移和根契约。采用既有有界复制路径，保留以后按测量单独评估共享不可变 blob 的空间。

单测检查严格类型、规范 mask/分段、空值、最高参数位、64/65536 合计边界、拒绝前不分配、三级额度失败回滚及源修改后的独立性。worker/Scheduler 检查错误模块、入口模式、stale Pid、满 mailbox/结果表、准备与退出竞争、超时/取消保留和最终清理。真实 AOT 在原三秒阶段与 CPU/墙钟上限内通过三类宿主入口混合导入、GC、原始 handle 拒绝及标量回复；运行 workspace、debug/release、MSRV 和可用平台验证，不放宽额度、不将 TCG 或功能结果作为原生双平台/BEAM 性能门禁。
