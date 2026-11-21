# ADR-0052：跨线程只读模块与代码租约

- 日期：2026-11-13
- 状态：已采纳（多 worker 前置切片）
- 扩展：[ADR-0044](0044-native-module-leases.md)、[ADR-0045](0045-concurrent-resource-admission.md)

## 决定与理由

资源账户与票据已线程安全，但计费模块仍以 Rc 租约人为绑定原线程。将计费租约移入代码本身的 Arc<ModuleData>，使只读 ScalarModule 与 HandlerId 自动满足 Send/Sync；不添加 unsafe Send/Sync，也不转移 actor、堆、栈池或挂起协程。

- ModuleData 用同一个 Arc 同时持有 manifest、入口地址、Library 和计费租约；租约字段排在 Library 后，由最后一个 Arc 所有者依次交还库再归还预算。禁止代码与预算分用两个 Arc：并发末次 drop 时，一线程正在执行 Library 析构，另一线程可能提前归还独立预算；单一所有权节点消除这个窗口。其他 worker 退出不卸载尚有持有者的代码。
- 模块绑定同一 VM 资源身份，各线程必须在本线程创建独立 worker、栈池与 actor；同一只读模块可供多 worker 执行，各自保留独立 state、mailbox、请求、堆和原栈。HandlerId 的模块身份及跨 VM 拒绝规则不变。
- unsafe 装载契约新增明确要求：可信生成代码可由不同线程中的不同进程并行调用，不得依赖装载线程 TLS、可变原生全局或原线程卸载；最后一次 Library drop 可在任意持有者线程发生。仍禁止自注册回调、后台 native 线程和脱离最后代码租约的析构行为。
- 线程的建立、join 与外部控制只属于受限验证宿主。验证预留线程控制逻辑预算；不引入 VM 自动 worker 池、共享可变 actor、跨 worker 业务消息或进程迁移。StackPool 及协程仍维持非 Send/Sync，不能把并行独立 worker 称 M:N。

## 替代方案与重审

每线程重新加载并重复预算可保安全但失去已有只读模块共享；保留 Rc 并强制 unsafe Send 会绕过类型检查，拒绝。原生线程内部依赖、自注册回调和热更新仍需静止期/退出协议，不能沿用本切片。

## 验证要求

编译期确认 ScalarModule/HandlerId/ModuleData/ModuleLease 为 Send/Sync。真实受限 AOT 用四线程分别创建 worker、启动独立 actor、反复单步恢复，主线程提前丢模块；核对 state 不串扰、模块只计一次预算、各线程收到同一模块的 handler、外来 VM 拒绝。不同 worker 退出后模块由剩余 handler 保活，最后 handler 转到另一线程 drop 后账目归零。

沿用 workspace check/test/clippy/fmt、MSRV、debug/release AOT 与最新双平台验证；没有 M:N、公平性、吞吐或 BEAM 内存领先结论。

2026-11-13 在 macOS 26.6.2 AArch64、Rust 1.99.1 通过 workspace check、debug/release 各 406 项测试、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check、两种构建的完整受限 AOT 和 TCP 示例单测。编译期另用歧义约束确认 worker/process/task/execution/pool 仍不满足 Send/Sync；单一 Arc 控制节点的预算随原始代码节点末持有者释放，跨线程并发 drop 和保留内部代码节点回归通过。Linux 隔离仿真的 release 独立测试和完整受限 AOT 亦通过，详见[验证记录](../validation-2026-11-13.md#跨线程模块增量3529ed7)；仍非原生双平台 CI 或 M:N 验收。
