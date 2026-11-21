# ADR-0080：初始化与所有执行入口统一预留控制容量

- 日期：2026-11-14
- 状态：已采纳（执行资源准入修正）
- 部分替代：[ADR-0019](0019-accounted-stack-pools.md) 不覆盖控制/参数存储的边界、[ADR-0076](0076-accounted-byte-messages.md) 由 worker 外层持有控制票据的安排。
- 沿用：原 4352 字节控制上界、计费栈净转移、强制检查点与正常取消后释放，不修改 ABI v15 或语言拼写。

## 问题与决定

单 worker handler 和 Scheduler 在外层预留控制容量，直接 `ScalarProcess`/`ScalarTask` 调用与单 worker 初始化却没有统一预留。传入计费栈池只转移栈票据，不包含参数 Vec、协程控制、token 与运行时暂存容量。显式标量初始化也没有保存传入池，后续同步调用会重新取默认未计费池。将准入移至执行核心，使计费路径不依赖调用者记得额外预留。

- initializer 与 `Invocation::new` 在创建控制/参数/协程、借出栈前，向绑定 process 及其 tenant/VM 预留原 4352 字节。未绑定的计费池在执行前拒绝；旧未计费工具池保持原工具边界，不伪装完整资源计费。
- 内部同步调用和单 worker handler 通过延迟工厂创建默认取消/让出标志；核心完成控制准入和借栈后才调用工厂。额度不足或栈池已满时不创建标志。公开受控入口传入的宿主 token 仍由宿主拥有，核心在准入后克隆其租约，不将宿主预先创建 token 写成 VM 已准入分配。
- initializer 用本地票据持有至协程正常返回或取消清理结束。Invocation 独占票据，ready/挂起/effect/GC 全程保持，正常完成、受管失败或 Drop 正常取消退出后再释放。准备失败不移动 heap/Bytes 载荷、不丢 mailbox，不毒化实例，完整回滚控制及栈预留。
- 移除 ScalarWorker handler、Scheduler initializer/handler 的外层重复票据，同一路径只收一次原控制容量；不提高账户上限、栈容量、mailbox/request 容量或业务预算。worker 固定结构尺寸按实际类型继续计费，不保留已删除票据的虚假存储成本。
- `instantiate_in_pool` 保存传入的同线程池，使标量同步 `invoke`/默认 `start_controlled` 继续使用显式配置的池；带堆路径沿用同一行为。显式 `start_in_pool` 的已有标量池选择规则、堆原账户校验保持；detach 仍丢弃线程本地池，attach 继续绑定接收线程池，不给活栈添加 Send/Sync。
- 预付费栈的 tenant/VM 净转移规则不变。但新控制存储是额外真实逻辑容量，原 VM 已满且没有控制余量时必须拒绝，不能因栈已预付费而放行其他分配。测试先保留精确控制余量，再验证控制准入使 VM 满额后栈仍可净转移；同时验证完全满额的拒绝及账目/栈状态零变化。

固定 4352 字节继续用实际控制类型上界测试约束。该容量是保守逻辑预算，不是 allocator 实际占用，也不包含代码装载器内部开销。默认未计费工具入口、CLI 适配层及完整 VM builder 的统一仍需后续收敛，不能据此宣布全部分配或 MVP 门禁已完成。

## 验证

先用显式计费池的直接执行复现“仅计栈、不计控制”旧行为，再验证 process/tenant/VM 各级精确控制边界与失败零变化、初始化/调用使用相同池、池满、ready/挂起/取消/失败清理、最大参数与固定控制上界。检查 worker 与 Scheduler 活栈只收一次、初始化失败回滚、开始前拒绝保留队首 Bytes/旧状态、未消费结果租约及最后归零。

真实 AOT 运行直接标量/Bytes、同步/显式/owned task、单/多 worker、消息/请求/GC/监督和 CLI 堆/TCP 全场景；更新原净转移测试的填充票据以保留精确控制余量，不扩大资源上限或超时。执行 workspace check、debug/release 测试与完整受限 AOT、all-target clippy、fmt、MSRV、依赖/unwind 守卫和可用 Linux 功能回归，保留原 CPU/墙钟/3 秒阶段边界。原生双平台、公平性与 BEAM 吞吐/整机内存门禁仍独立验收。

旧 worker 与 timer 的极小额度 fixture 只留 actor 和栈，不能继续期待初始化成功。保留原额度并新增初始化拒绝/零变化断言；再用 actor、栈和原控制容量构造可初始化的精确正边界，队列消息使 handler 超额，继续检查队首、旧状态、未完成请求及 timer 清理。这是修正 fixture 对实际成本的假设，不是扩大原失败边界以放行请求。
