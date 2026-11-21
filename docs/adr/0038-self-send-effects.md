# ADR-0038：语言自发送与调用点副作用

- 日期：2026-11-13
- 后续：[ADR-0039](0039-local-process-capabilities.md) 以 ABI v9 扩展按值 effect 协议及同 worker 创建/发送，调用点契约保留。
- 状态：已采纳（M0/M1 消息副作用前置切片）
- 补充：[ADR-0020](0020-single-worker-mailbox.md)、[ADR-0036](0036-bounded-inline-functions.md)

## 决定与理由

先用同进程自发送验证调用点准入和失败不回滚，不一次开放 Pid、跨进程 schema、spawn 和异步语言事件。选择协程按值交出 effect 请求，避免让原生回调持有 worker/actor 裸指针或重入正在执行的实例。

- handler 可用 `send_self(Message, args...)`，Message 必须是本 process 已声明的单向入口，实参类型/数量与入口匹配；返回 Unit，参数仍限 Int/Bool/Unit。Message 为静态协议名，不是整数、函数或一等消息值。支持 forward handler 声明，拒绝请求入口、未知名字和普通绑定冒充目标。initializer 和 fn 内禁止该副作用。
- ABI 升至 v8，context 尾增加可空 EffectCall 指针。编译器在操作前发布所有活跃根并执行预算检查，然后调用 effect 回调；回调复制最多 64 个标量参数及 handler 序号，并让出原栈。worker 同线程校验 schema，在现有进程账户下准入并立刻入队，随后保存成功/失败确认；下一片恢复原调用，不重放表达式或发送。
- 每个 tick 至多执行到一次 effect 请求或普通让出/返回；已准入消息在该 tick 返回前可见，排在已有队尾，只保证该单 worker 的既有 FIFO。发送继承当前入口 max_steps 作为新消息独立总预算，不转移或补充当前预算；新消息仍须后续活栈准入。自发循环仍受有限队列、进程/VM/租户额度和每个入口预算约束，宿主可 stop，不承诺全系统业务最终终止。
- mailbox 满或消息资源预留失败返回受管 SendRejected，当前入口失败不提交候选；此前成功发送的消息不撤回。若原进程随后退出，这些消息按原有进程退出清理丢弃，不代表回滚发送。成功入队与后续处理/成功回复不是同一保证。诊断的 queued 和清理前资源能观察这一界限。
- 一次 effect 请求存在固定 Invocation 槽，参数与确认按值复制，不保留 context/根指针；消息复制/预留在 worker 原有计费路径执行。返回前不销毁挂起栈。Drop 若确认已排队则只取消原调用，不尝试撤回或重新发送；未被 worker 接受的请求不产生副作用。
- 独立 ScalarProcess/ScalarTask 与 CLI 未连接 worker，effect 回调为空，调用返回 InvalidControl，不默默忽略或忙循环。worker 配置入口回调，initializer 一律不提供。`ExecutionStatus` 不新增需要外部手工处理的 effect 状态，内部查询/确认仅 worker 可见。

## 验证与重审

前端/IR 校验静态目标、kind、参数、函数/初始化禁用及不可伪造序号；LSP 定义/hover 指向 handler。真实生成代码验证调用点前后逐片暂停、FIFO、正常提交后处理自消息、已发送后算术/预算失败的队列与清理、队满/三级消息额度拒绝、无 worker 拒绝、Drop 和 stop 不重放。每次 effect 交接保持精确根，包含引用候选与 GC。debug/release、MSRV、双平台分别验证。

不使用 handler 返回后的 effects 列表，不宣称跨进程 send/spawn/request 已实现或事务/exactly-once。后续 Pid、引用消息或跨 worker 投递须独立制定协议/身份/计费转移和根证明；本切片不是完整 MVP。
