# ADR-0035：结构化执行故障与单槽退出诊断

- 日期：2026-11-13
- 状态：已采纳（M0 宿主失败定位切片）
- 补充：[ADR-0032](0032-safe-point-diagnostics.md)、[ADR-0023](0023-bounded-host-supervision.md)

## 决定与理由

进程失败后旧 Pid 立即失效，成功的只读快照无法定位刚退出的 handler、预算或 GC 状态。不能为观察故障保留旧活栈/堆或建立无界历史；解析错误字符串也不是稳定的故障分类。

- `ExecutionSnapshot` 增加 `fault: Option<ExecutionFault>`，从原生 ABI 状态码分类算术、取消、预算、控制、栈、堆和根错误；正常 ABI 返回但状态/回复验证失败归为 InvalidOutput，未知状态保留原始 u32。Ready/Suspended/Complete 无 fault，Failed 必有 fault。错误文本与既有事件类型保留，不作为分类来源。
- worker 在受管失败返回、旧 actor 清理前按值复制一份 `ExitSnapshot`，包括 Pid、handler 名、执行结果、state 类型、已提交标量值、排队数、堆统计和清理前进程账目。不复制候选 state、参数、记录 handle、根链或上下文，不保留模块、heap、栈或账户所有权。
- 每 worker 固定一个最近失败槽，后续受管失败覆盖；Idle、成功、主动 stop 或 supervisor 重启不清空。`last_exit` 只借用该值，supervisor 转发相同查询。handler 名采用固定 256 字节 UTF-8 存储，符合清单已有上限，不新建字符串或动态队列。槽内存计入既有 worker 构造前的 size_of 准入。
- 账目表示清理前观察值，不是未归还资源；返回退出事件之前仍完成原有清理。预算/GC 统计来自入口结束边界，已提交状态来自旧 actor，引用 state 只报告类型。排队数不含当前失败消息。
- 宿主参数/准入错误、未开始入口、主动 stop 和未知 native 内存错误不伪装为受管退出；不新增自动重试或修改 request 终结规则。缓存不承诺多次失败历史，也不替代持久日志或语言 monitor。

## 验证与重审

Rust 单测覆盖全部 ABI 分类、未知码、InvalidOutput、成功清空 fault、旧 Pid 失效后仍可查最近失败、成功/stop 保留及第二次失败覆盖、外部查询不改变计费、拒绝原始引用和 supervisor 重启保留旧失败。真实 AOT 检查预算、除零和小堆失败的入口/剩余预算/GC/请求终结及完整清理。debug/release、MSRV 和双平台独立验证。

不改 ABI v6，不延迟 actor 销毁、不存无限事件队列。以后跨 worker 聚合、故障历史、敏感数据脱敏或语言级系统消息须另立容量、租户隔离和生命周期契约。当前不宣称完整 M4 或 MVP。
