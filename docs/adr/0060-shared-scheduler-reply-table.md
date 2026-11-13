# ADR-0060：调度实例共享宿主请求结果表

- 日期：2026-11-13
- 状态：已采纳（重复控制元数据消除切片；macOS 功能已验证，Linux 与完整门禁另记）
- 部分替代：[ADR-0054](0054-bounded-multi-worker-scheduler.md) 的逐 actor 三类型结果表
- 扩展：[ADR-0047](0047-cross-thread-ingress-requests.md)

## 决定与理由

多 actor 负载前的容量探测发现：每 actor 无论是否有宿主请求，都创建 Int/Bool/Unit 各一份、各 `config.requests` 槽的结果表。该重复容量随 actor 数增长，还会让最后一个结果 handle 保留整张 actor 类型表。先消除已量化的重复控制数据，不以调小配置或取消检查换取数字。

- 每个 Scheduler 只预建一份、`config.requests` 槽的宿主 ReplyTable，固定控制容量向 tenant 计费；Actor 不再保留三个 Arc/表。语言请求表仍独立、容量也为 `config.requests`，两者不是共用同一槽。普通 ingress 继续每固定入口一份表，不改变其容量契约。
- 宿主 Int/Bool/Unit 请求共用该实例上限，不再每 actor/类型各有一份额度。`IngressRequest::reply_type` 按请求值保存，槽和 publisher 不需要存类型；成功输出仍由原 handler schema 校验，不增加引用回复。
- 每次 start 先向目标 process 的原三级路径预留 requests 票据，再在共享表选槽；只允许同控制账户或直属子 process，跨账户树拒绝。表满、队满、schema/额度失败不入队、不留半槽；当前请求账户保留到 caller/publisher 都结束，timeout/cancel/Drop 不提前归还在途容量。
- 结果 handle 只持共享结果表，不持 Shared/actor/模块；Scheduler 关闭清理 publisher，已完成/失败结果可在关闭后消费。表控制租约保留至最后结果 handle，原目标账户只由其在途请求票据保留。正常终态、代次耗尽和旧 publisher 拒绝规则不变。
- 不改 ABI、语言语法、worker 唯一执行权、活栈线程固定或返回后迁移。栈池仍按原 actor 容量逐 worker 预留，固定 registry 扫描与最多 256 actor 尚未扩展，不隐瞒其主要成本。

## 测量与验证

新增 `scheduler_density` 受限子进程探测，以相同 release Counter、2 worker、64 请求容量和 mailbox 1，在 16/64/256 个空闲标量 actor 记录空表/填满/退出的逻辑预留。旧实现 macOS AArch64 的每 actor 增量为 19288 B，256 actor 共 4937728 B；这个数字仅为逻辑计费，不是 RSS/物理内存或 BEAM 结果。保持相同配置、退出后归零并记录修改后结果，不把栈预留消失算入优化。

修改后每 actor 增量为 1672 B，256 actor 共 428032 B；同配置填满时总逻辑预留从 156079048 B 降为 151569072 B，逐 worker 栈预留仍是主要成本。完整受限 AOT 验证每 actor 空闲控制增量不超过 4096 B、退出回到空表、最终归零；此上限只防重复元数据回归，不是 actor 总内存承诺。完整数据与验证环境记入[验证记录](../validation-2026-11-13.md)。

测试多 actor/混合回复共用上限、跨 actor 无空槽拒绝且账目不变、消费后复用、各目标 process/tenant/VM 额度、取消/超时双端租约、关闭后结果保留、旧 generation 及并发生产者。运行 workspace/check/clippy/fmt/MSRV、debug/release 完整受限 AOT、真实 Counter 对照和 Linux 回归；不放宽 CPU/墙钟限制。

## 对照与重审

惰性创建逐 actor 表会保留第一次请求后的重复容量，且要在请求热路径分配和回滚；只合并三种类型仍随 actor 数成倍增长。选固定实例表作为明确的总上限，将来若共享锁/扫描成为吞吐瓶颈，可在同协议测量后分片，不把全局锁当性能最终方案。大规模 actor/开环、公平性与双平台 BEAM 吞吐/整机内存门禁仍待完成。
