# ADR-0050：子账户控制节点的先准入与末持有者计费

- 日期：2026-11-13
- 状态：已采纳（资源元数据切片）
- 扩展：[ADR-0014](0014-resource-reservations.md)、[ADR-0045](0045-concurrent-resource-admission.md)

## 决定与理由

当前账本可以限制业务分配，却不限制创建账户控制节点。worker 创建进程账户、退出后仍由 ingress 或票据持有的账户，都必须有明确预算和释放时机。先覆盖这些节点，不把逻辑字节等同 allocator 实际内存。

- 增加显式 `tenant_accounted` 和 `process_accounted`；仅允许 VM → tenant → process。子节点在申请 Arc 存储前向父路径预留 `size_of::<AccountNode>() + 2 * size_of::<usize>()`，覆盖节点及 Arc 强弱引用计数的逻辑存储。父路径超限/溢出直接拒绝，不分配节点、不产生部分计数。
- 节点元数据由父级持有并计入父级及全部祖先，不占子账户业务额度，不消耗 processes/messages/requests。子节点用不可转移的内部元数据票据保存父路径，不另存重复的父 Arc；未计费子节点使用零额票据。票据只引用父，不引用子，避免自身循环。最后一个子账户所有者释放时归还，而不是 actor 退出时提前归还。
- worker 一律用 `process_accounted` 创建 actor 账户；spawn、初始化或后续准入失败完整回滚。挂起栈、ingress sender、未完成结果/票据可以延长账户租约，必须随最后所有者归还。无需在 worker 锁外扫描引用计数来猜测释放时机。
- 原 `vm`、`tenant`、`process` 保留为兼容的未计费工具原语，明确不满足完整 VM 资源门禁；VM 根及共享门闩仍是外部 bootstrap。真实服务示例显式选择计费 tenant，CLI 堆模式使用计费 tenant/process；其他独立原语可继续用旧接口测试净资源算法。不宣称所有公开组合都已强制计费。
- 账户与票据仍自动 Send/Sync；元数据准入复用同 VM 门闩。节点分配发生在预留返回后，不在门闩内调用 allocator。Rust allocator 自身的元数据、全局 OOM/abort、动态装载器内存和根 bootstrap 不在此切片内，不能据此宣称硬内存隔离或完整 M1。

## 替代方案与重审

直接修改所有现有构造器会改变独立账本模型和工具的零基线；本次先为执行路径提供显式计费入口，不伪装旧接口已闭环。把子节点票据记在子账户会形成自引用或需要额外销毁协议；归父级所有能用普通 Arc/Drop 保持单向层级。后续正式 VM builder 收敛入口时，须决定是否移除未计费构造器，并为根/门闩、分配器和其他控制分配单独建立预算。

## 验证

验证 tenant/process 的精确限额、父/祖先拒绝、失败无变化、不占子业务额度、clone/票据延迟释放、父 handle 提前释放、跨线程最后归还和并发节点争抢。worker 回归覆盖 spawn 配额/初始化失败、退出/stop/Drop、ingress 最后发送端与请求结果租约，不把节点归还时间缩短为 actor 生命周期。

执行 workspace check/test/clippy/fmt、MSRV、debug/release workspace 测试与受限 AOT 全回归。两平台独立记录；此切片不改变语言、ABI、GC 或活栈线程约束，也不完成 M:N 或同业务 BEAM 门禁。

2026-11-13 在 macOS AArch64 通过上述检查、Rust 1.85.0 all-targets、debug/release workspace 测试和受限 AOT 全回归；TCP 示例单测通过，TCP 专项 debug/release 各重复三次通过。BEAM oracle v1 的 11 个 checked/FIFO/exit 用例与 23 个非法输入亦通过，仅为既有 oracle 正确性，不是 Wings 等价性能结果。Linux x86-64 尚未验证此切片。
