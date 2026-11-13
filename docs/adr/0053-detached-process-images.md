# ADR-0053：无活栈进程镜像的独占线程转交

- 日期：2026-11-13
- 状态：已采纳（迁移前置切片）
- 相关：[ADR-0015](0015-owned-scalar-tasks.md)、[ADR-0019](0019-accounted-stack-pools.md)、[ADR-0052](0052-cross-thread-module-leases.md)

## 决定与理由

只读代码可共享后，先验证无活栈状态的独占转交，不为 corosensei 活栈添加 unsafe Send。新增 DetachedProcess 只按值持有模块、已提交 state、失败标志及独占堆，允许自动 Send；它不含栈池、协程、shadow frame 裸指针或执行接口，也不实现 Clone。

- ScalarProcess::detach 消费原进程并丢弃原线程栈池绑定。借用该进程的 ScalarExecution 存活时不能调用 detach；ScalarTask 需先显式 into_process，原有“取消活调用并标记失败”规则不变，不伪装暂停迁移。
- 将 AotHeap 的原线程 StackPool 引用移到 ScalarProcess；堆只保留线程安全的原 process 账户。镜像保留精确已提交状态根、完整堆代次/布局/额度和失败状态，不编码序列化、不复制堆、不回收尚有根的状态。
- DetachedProcess::attach(&mut self, pool) 在当前线程验证代码 VM 身份；带堆镜像要求目标池绑定**同一个 process 账户**。同账户可在同 tenant 的新线程栈池重新绑定，错账户/跨 VM/未计费池拒绝并保留镜像，可重试。成功只取走一次镜像；失败进程恢复后仍失败，不能用 detach 重置故障。
- attach 不分配业务对象、不执行 initializer 或 handler，不预借活栈。镜像持有原堆/账户资源至接回或 Drop，目标执行前仍走目标池的准入、强制检查点与总预算。标量及带堆镜像的同步接口均绑定传入目标池，显式执行仍校验模块 VM 及堆账户。
- worker、ScalarProcess、ScalarTask、ScalarExecution、StackPool 继续非 Send/Sync。镜像是宿主独占转交原语，不携带 Pid/mailbox/pending request，不接入 worker 队列或调度执行权；不是 M:N、活栈迁移或跨 worker 消息实现。

## 替代方案与重审

整体 unsafe Send 进程或协程会搬运 Rc/RefCell/TLS 和未知 native 栈局部，拒绝。纯序列化可移交但需双份内存预留及业务图格式，当前不需要；本切片只移动已拥有的不可变对象存储。

未来接入 worker 迁移必须设计 Pid/mailbox/request 路由及唯一执行权，活 handler 迁移须另行证明全部栈帧和 runtime 数据可跨线程，不能从空闲镜像推断。

## 验证要求

编译期确认镜像 Send，原执行类型仍非 Send/Sync。真实受限 AOT 在不同线程间移动 IntList 状态，源池释放后目标池重建同账户绑定，collect 后读取/更新正常且不重跑初始化；错账户/跨 VM/无预算绑定拒绝不丢镜像，接回只能一次。覆盖未接回即 Drop、失败镜像、目标堆/栈资源拒绝、取消返回后的镜像与最终账目归零。

沿用 workspace check/test/clippy/fmt、MSRV、两种构建受限 AOT 与平台独立记录；不得宣称 M:N 或 MVP 门禁通过。

2026-11-13 在 macOS AArch64 通过 check/clippy/fmt、Rust 1.85.0 all-targets check、debug/release workspace 各 409 项测试及 TCP 单测。完整受限 AOT debug 通过；release 首次运行遇到既有 TCP frame_limit 的 ConnectionResetError，未修改该测试或放宽截止时间，重跑全套通过。无活栈镜像专项在两种构建均通过。Linux 最新增量另验，不将重跑成功写成 TCP 无偶发失败。
