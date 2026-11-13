# ADR-0049：RecordInt 消息的按值复制与入口根

- 日期：2026-11-13
- 状态：已采纳（引用消息切片）
- 扩展：[ADR-0026](0026-language-temporary-records.md) 的记录消息限制
- 相关：[ADR-0025](0025-aot-record-roots.md)、[ADR-0039](0039-local-process-capabilities.md)、[ADR-0040](0040-language-request-events.md)

## 决定与理由

让现有不可变 RecordInt 能进入同 worker 同模块的单向消息和整数异步请求，不共享进程堆或跨进程搬运原 handle。该切片不改基础类型拼写，不提前采纳待评审的语法草案；IntList、通用记录、引用回复仍单独设计。

- 未发布 ABI 升至 v11，EffectRequest 末尾增加 record_mask。编译器按参数类型发布 Ref 位，消息字段允许 RecordInt，回复仍限标量；IntList/Pid/普通 RequestId 边界保持拒绝。旧 v10 artifact 明确拒绝，不改已发布 0.0.2。
- effect callback 在原调用栈上、源对象仍有 compiler 精确根时校验 mask 和源堆布局，将每个 RecordInt 的 Int 字段复制进固定 64 槽信封，带原 mask 交给 worker。每字段一个节点、最多 64 字段，固定上界；不保留源 heap/context 指针，不在 worker 暂停期间直接访问 coroutine 所有的源堆。
- worker 只在语言 effect 路径接受带匹配 mask 的记录载荷，校验目标入口后用目标进程的消息票据预留信封空间，再入队。普通宿主 send/request、JSON 和跨线程 ingress 继续拒绝引用，不能把外部整数伪装为引用或复制载荷。send/request 调用点生效，源 actor 后续失败不撤回已入队记录。
- 目标消息启动后、进入生成 handler 前，在目标计费堆为各记录字段创建独立 RecordInt；每次分配遵守 runtime 预算、原子取消/让出和分块 GC，分配失败为目标受管失败。已接受消息不是已预留堆槽保证：目标堆满允许失败退出，但不得形成跨堆 handle 或破坏其他 actor。
- 已复制的参数存放在宿主精确入口根表，最多 64 项；GC 同时扫描 compiler 根、已提交 state 和入口根。入口根覆盖后续参数分配、生成入口前检查点以及 handler 活栈暂停，正常完成/失败/取消清空；不会把原始载荷当 root。重复参数独立复制，不承诺保留源对象别名身份。
- 信封 ring 与 Message 已按固定槽计费，新增 mask/根容量纳入 size_of 及 heap 根表容量；退出归还消息/堆/栈票据。源堆可在消息交接后独立回收，目标状态可以保留自己的复制引用。没有共享可变业务数据、移动 GC 或跨线程活栈恢复。

## 验证

源码/sema/IR/manifest 允许 RecordInt 参数并拒绝列表/引用回复/能力伪造；LSP 复用检查器。生成代码在真实不同 actor 间复制记录，接收端 collect 后读值并可保存到 state，覆盖自发送、异步整数请求参数、源失败不撤回、目标满堆、混合标量/多记录参数、全部检查点暂停/取消与清理。普通宿主/JSON/ingress 拒绝原 handle；mask/schema/无堆/stale/layout 不匹配均受控失败。

沿用 workspace check/test/clippy/fmt、MSRV、debug/release AOT 受限子进程；两平台分别记录。此切片不等于通用消息图复制、完整业务值、M:N 或 MVP，所有性能与内存门禁保持不变。

2026-11-13 在 macOS AArch64 通过 workspace check/test/clippy/fmt、Rust 1.85.0 all-targets 检查、debug/release workspace 测试及受限 AOT 全回归，TCP 示例单测亦通过。强制 GC 回调验证发送点最后一次使用的记录仍有 compiler 根；内部测试覆盖全部 64 个导入参数、最高 mask 位及 129 个汇总根槽。Linux x86-64 尚未验证此切片，无双平台或同业务 BEAM 性能结论。
