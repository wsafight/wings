# ADR-0079：VM 根账户与准入门闩的启动计费

- 日期：2026-11-14
- 状态：已采纳（VM 控制资源切片）
- 后续：[ADR-0081](0081-accounted-default-cli.md) 补 CLI 默认标量模式的计费根、模块和显式池；适配层及其余资源边界仍待完成。
- 后续：[ADR-0084](0084-accounted-root-module-admission.md) 要求计费根及后代接入的模块带有本 VM 计费租约。
- 后续：[ADR-0087](0087-accounted-beam-workload-roots.md) 将六个 BEAM 对照驱动接入计费根，清理回到实际启动值而非零。
- 部分替代：[ADR-0050](0050-account-node-admission.md) 的根节点/共享门闩属于账本外 bootstrap，以及计费根下仍可创建未计费子节点的安排。
- 沿用：[ADR-0045](0045-concurrent-resource-admission.md) 的 VM 级准入锁、三级净转移与末持有者归还。

## 决定与理由

子账户节点已可先预留再分配，但根节点与共享准入门闩仍不占 VM 字节额度。新增 `ResourceAccount::vm_accounted(limits) -> Result<ResourceAccount, String>`，在两次 Arc 分配之前验证根节点、门闩及两份 Arc 强弱引用计数的逻辑存储能否容纳。字节不足直接拒绝，不创建半个账户；不占 processes/messages/requests 额度。

- 根启动成本为 `size_of::<AccountNode>() + size_of::<Mutex<()>>() + 4 * size_of::<usize>()`。根 used 从该固定成本开始，所有后续业务/子元数据都在剩余额度内准入。clone、子账户或票据延长根租约时，该成本继续存在；业务清理后回到启动基线，不能伪报 used 为零。
- 启动成本直接初始化根计数，不创建指向根自身的 Reservation，避免 Arc 自引用环。根只保存是否使用计费启动的标志；子节点仍用单向父票据，不新增共享控制对象。最后一个根/后代/票据所有者释放后，根和门闩由普通 Arc 析构释放。
- 计费根下调用 `tenant`/`process` 也自动走计费子节点路径，与显式 `tenant_accounted`/`process_accounted` 一致。不能用旧构造器绕过该根的控制存储额度；非法层级、父/祖先超限与失败回滚规则不变。
- 旧 `vm` 仍供独立账本/工具模型使用，其旧子构造器保持零基线；它不满足完整 VM 资源门禁。此次将已有计费执行入口中的 CLI 堆模式与 TCP 服务改用计费根；标量 CLI 默认路径、独立工具/基准构造器及完整 VM builder 的统一仍须后续收敛，不能把新增入口写成所有公开组合都已强制闭环。
- TCP 服务在释放 adapter、worker、module 和 tenant 后检查 used 精确等于根启动基线；输出原 `resources_released: true` 时同时报告 `bootstrap_bytes` 与 `remaining_bytes`，二者须相等。该标志表示业务及派生控制预留已归还，存活根不会被描述成无内存成本。监听/帧/I/O/请求额度、CPU/墙钟及截止时间保持不变。

这是一项逻辑存储准入，仍不覆盖 allocator 自身元数据、动态装载器内部内存或全局 OOM/abort，不能提供硬内存隔离。不修改 ABI v15、语言拼写、GC、请求发布端生命周期或活栈线程约束。冻结的既有 BEAM 测量不回写，新增根成本不能据此冒充整体吞吐/内存优化。

## 验证

精确检查根/门闩逻辑成本、零和不足/精确字节边界、其他维度为零仍可启动、clone 不重复收费、计费根下旧子入口不能绕过元数据额度、父/祖先拒绝零变化、全 VM 满额度时同树净转移、跨根拒绝。用弱引用及跨线程最后持有者检查没有根自引用环；并发创建子节点与清理后回到同一启动基线。

真实 AOT 验证完整计费根下标量/引用状态执行、暂停/取消、栈和模块额度拒绝、正常和失败清理，以及一/双 worker 与未消费请求租约保留。CLI 堆模式及 TCP normal/错误/超时/断开/失败全场景执行；TCP 校验启动基线字段和派生预留回收。workspace check、debug/release 测试与完整受限 AOT、all-target clippy、fmt、MSRV、依赖/unwind 守卫和可用 Linux 功能回归继续执行。保持原资源上限与时限，不把功能证据当作原生双平台或 BEAM 双门禁通过。
