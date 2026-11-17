# ADR-0123：受管记录回复的无文本准入拒绝

- 日期：2026-11-18
- 状态：已采纳（资源拒绝路径修正）
- 延续：[ADR-0045](0045-concurrent-resource-admission.md) 的单门闩准入、[ADR-0116](0116-bounded-named-record-replies.md) 的计费回复和 [ADR-0122](0122-bounded-handler-lookup-diagnostics.md) 的错误来源收敛。

## 问题与决定

`RecordReply::reserve` 只需要 `STATUS_HEAP_EXHAUSTED`，但原 `ResourceAccount::reserve` 在失败时先分配包含账户级别的 `String`，随即由回复层丢弃。真实 AOT 的固定空记录回复在 handler 已开始、process 满额时产生两次分配：内部立即丢弃的准入错误和最终返回宿主的执行错误。

将现有准入事务抽为 crate 内部 `reserve_status`，失败只返回已有的 `ResourceLevel`，无文本构造。公开 `reserve` 仍调用同一事务，并在锁已释放后构造原 `String`；不改变现有 API/诊断。记录回复直接将内部失败级别映射为 ABI 状态。三级四维检查、共享门闩、溢出失败、失败零变化、成功票据/Drop 和净转移协议不变，不维护两套准入算法。

不更改执行预算或取消检查，不将宿主最终拥有的错误字符串伪报成零分配；该 AOT 拒绝路径预期从两次降至一次。其他仍返回文本的宿主/runtime 路径没有借此宣称已闭环。ABI v21、manifest v2、4352 B 控制、各级额度和栈/堆容量不变。

## 验证

保留修复前原限制子进程内的分配计数器失败。在 process/tenant/VM 分别满额时，真实 AOT 仍返回 HeapExhausted、单一宿主错误、清理回根；成功回复和正常读取继续正确。账户层另覆盖三个失败级别、四个资源维度、溢出和原错误文本，证明拒绝无净变化及同一准入事务。

单路径减少分配不等于端到端吞吐提升、完整资源账本、所有 quota/OOM 可恢复或 BEAM 双目标达成。故障只在原 CPU/墙钟/core 限制子进程内运行；原生双平台和 MVP 其余门禁保持独立。
