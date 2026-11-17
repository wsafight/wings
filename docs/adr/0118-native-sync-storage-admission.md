# ADR-0118：原生同步后备存储先准入与发布前初始化

- 日期：2026-11-18
- 状态：已采纳（MVP 资源账本修正）
- 部分替代：[ADR-0079](0079-accounted-vm-bootstrap.md) 的根启动字节公式；延续 [ADR-0045](0045-concurrent-resource-admission.md) 的锁内无分配约束。

## 证据与决定

本机 Rust 1.99.1 macOS AArch64 的标准 Mutex/Condvar 在首次使用时通过 OnceBox 分别分配 64 B pthread mutex 和 48 B pthread condvar。独立单线程分配计数器显示：原根创建时存活 168 B、used 仍为 168 B，首次 used 查询新增一次分配，存活变为 232 B。根启动公式漏掉了原生 mutex 后备存储；监督退出票据原 64 B 控制余量也无法同时容纳 16 B Arc 头和 64 B mutex。

保留标准库同步实现，新增内部构造辅助函数。在分配前已有票据的构造路径中创建 Mutex/Condvar；macOS 的新 mutex 立即在尚未发布时加锁/解锁，新 condvar 立即对空等待集通知一次，完成原生后备对象初始化。之后才将外层结构通过 Arc、worker 或 sender 发布，避免首次共享使用才分配以及并发首次使用的临时竞争分配。Linux 的 futex 实现沿用内联存储和原构造行为，不增加原生后备字节。

根启动准入增加 `size_of::<libc::pthread_mutex_t>()` 的 macOS 后备字节；本机基线从 168 B 变为 232 B，不提高根额度。退出票据控制预留取原 64 B 与 `2 * size_of::<usize>() + mutex 后备字节` 的较大值，本机从 64 B 增至 80 B。结果表、ingress、Scheduler、timer、观测 FIFO 和原子让出 observer 的原固定控制余量显式包含各自原生同步字节，沿用足够的既有预留。每份后备对象随原同步对象和末 owner 一起销毁，不产生新共享租约或线程。

该辅助函数覆盖 VM 源码直接创建的同步对象，未替换标准库内部 channel/thread 的实现，也不把 allocator 元数据、动态装载器或 OS 内核成本称为已精确计费。ABI v21、manifest v2、4352 B 执行控制、原 actor/mailbox/request/堆/栈容量与所有 CPU/墙钟限制不变。历史冻结结果及其 168 B 启动回执不回写。

## 验证

独立全局分配计数器先保留原缺口失败，再验证根创建后的全部请求存储与启动 used 相符，首次 used/reserve 不再分配，满额根仍能执行零资源准入，释放后回到原分配基线。核对本机标准原语的后备请求大小，超过预留或发布后新增分配均失败；allocator 自身元数据不纳入该探针。退出票据验证 tenant/VM 不足、精确边界、克隆末 owner 与清理。既有多 worker、观察、ingress、取消/超时/恢复、全量和真实 AOT 回归保持原限；最新原生双平台及 BEAM 吞吐/整机内存双门禁继续独立验收。

实际修复前失败、Rust 1.99.1/1.85 本机分配探针、debug/release 全量与真实 BEAM 短测结果见[验证记录](../validation-native-sync-accounting-2026-11-18.md)。
