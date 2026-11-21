# ADR-0141：受限子进程中的真实分配失败注入

- 日期：2026-11-19
- 状态：已采纳（分配回滚与故障验证切片）
- 延续：[ADR-0018](0018-bounded-record-heap.md)、[ADR-0080](0080-central-execution-admission.md)

## 决定

新增独立 allocation_failures 示例，只在 CPU 至多 20s、core=0 的测试 OS 子进程启用自定义 Rust 全局分配器。按第 N 次调用或精确请求大小，对有限次 alloc/alloc_zeroed/realloc 返回 null；其他分配仍走 System。不使用宿主内存压力，不修改生产 allocator，不让故障开关进入库或受管 ABI。父进程保持原 30s 墙钟限制，失败保留输入/双流/退出。

覆盖 RecordHeap 四份固定存储、ShadowRoots、真实 AOT 带名义记录布局的堆构造，核对失败点、三级账户/活跃栈与 Rust 请求存储的回滚。调用入口使用已创建的计费控制 token，专门拒绝参数副本的首次分配；失败不启动 handler，不拿走旧堆，原对象可再次启动并正常提交。真实受管 Bytes 分配第一次失败时沿既有一次 GC 重试，连续两次失败则返回受管堆耗尽；失败 actor 的旧提交和健康 actor 进展、结果清理须独立核对。另拒绝宿主类型化消息副本和候选回复导出副本，核对发布前无入队、原对象可重试，以及导出失败不提交候选。初始总计必须实际注入 18 次。

后续补充计费模块加载自身的路径结果分配：按 canonical 路径原生字节数精确命中 `module_path` 的 `try_reserve_exact`，必须返回固定分配错误、三级账户与 Rust 请求存储回基线；无注入重试后继续使用该真实 AOT 模块执行原业务。最终每个构建实际注入总数为 19 次，并分别核对业务 owner 清理后的三级账户基线和账户节点释放后的 VM 根基线。

## 范围与限制

故障仅命中已审计的 fallible 分配；不向 Arc/Box/标准库 infallible 控制分配任意返回 null，不承诺整个 VM 在系统全局 OOM/abort 时仍存活。配额拒绝、分配器失败和 OS OOM 是不同证据。记录的是 Rust 请求字节，不含 allocator 内部、libc/dyld、mmap、碎片/缓存或 RSS；清理返回基线也不是完整资源账本或性能结论。

每一确定目标必须观察到实际注入次数，否则失败；有限次数耗尽后立即恢复 System，避免错误 String/验证器自身进入无限失败。用既有 module/heap/worker 功能执行真实 AOT，不把 Rust 假入口当编译器 safepoint 验证。原 ABI v22、4352 B 控制、资源上限和 CPU/墙钟期限保持；完整 workspace/AOT、MSRV、交叉检查与原生双平台/BEAM 门禁仍独立验收。
