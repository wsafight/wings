# ADR-0062：稀疏就绪队列与大容量 actor 表

- 日期：2026-11-14
- 状态：已采纳（稀疏派发切片；macOS 功能已验证，Linux 与完整门禁另记）
- 部分替代：[ADR-0054](0054-bounded-multi-worker-scheduler.md) 的全 registry 轮转扫描与 256 actor 上限
- 扩展：[ADR-0061](0061-on-demand-scheduler-stacks.md) 的按需计费栈
- 后续：[ADR-0066](0066-bounded-scheduler-mailbox-capacity.md) 将 mailbox 参数上限扩至 256，保留本决策的就绪/活栈/空槽队列。
- 等待竞争修正：[ADR-0093](0093-scheduler-ready-before-park.md) 在停车锁内重查本 worker 队头，保留已认领启动失败的退避。

## 决定与理由

按需栈消除空闲栈预留后，worker 仍对每个 actor 槽加锁、扫描语言 pending 表，并扫描自己全部空执行槽。稀疏业务的每次派发成本随空闲 actor 数增长，直接妨碍既定 1000/10000 actor 负载。先消除全 actor 派发扫描，再开放相应容量，不只上调常量。

- actor 表上限升至 16384；worker 仍为 1..=16，宿主结果与语言 pending 各为 1..=256，mailbox 仍为 1..=64。本切片不扩大请求表或放宽时间/资源限制。
- 启动前在 tenant 准入 actor 槽、每 worker 队头/尾和固定本地执行容器；就绪链借用槽内双向链接，不在 send/request/完成/重试路径分配额外队列节点。每个 actor 至多在一个 ready queue，退役先移除成员，再推进 generation；旧代不留下占满容量的无效 token。
- 空闲到有消息时加入目标 worker 的 FIFO 就绪尾。首次派发按 actor slot 分布，正常返回后仍分配给 `(last_worker + 1) % workers`，不改变已有可复现边界迁移契约，不宣称自适应 work stealing。活栈只在 owner worker 的固定本地 FIFO 轮转，不扫描空执行槽，也不跨线程迁移。
- worker 每轮至多认领一个新 handler 并恢复一个已有活栈时间片；新工作不能不断插到活栈前，已挂起工作也不能阻止其他就绪 actor 被认领。语言 pending 维护仍扫描独立的最多 256 槽，不再对每个空闲 actor 重复扫描；不是无锁或 deadline 最优队列。
- 启动资源拒绝恢复原 FIFO 队头并重新排入就绪尾，其他 actor 可推进；无实际执行进展时有界等待后重试。结果发布、语言消息/完成入队、正常返回后剩余消息都保持队列成员不重复，stop/退出/关闭完整清理。
- 创建 actor 用预建空槽链 O(1) 取得候选，初始化仍在锁外且失败返回原空槽；不扫描所有存活 actor 找空位。耗尽 generation 的槽永久退出空槽链。语言 capability 查找及退出时 pending 清理仍有界扫描，不以本切片宣称全部操作常量时间。
- 按需栈最大借出数随 actor 上限扩展，但每次仍完整三级准入、固定 guard/单栈和最多两个缓存。公开预映射栈池仍上限 2048，不能误把大容量模式用于一次预映射所有业务栈。

## 验证要求

纯队列测试覆盖 FIFO、重复发布、移除头/中/尾、满容量、代次复用和双 worker 归属；运行时测试保留唯一 owner、并发发送、队满、启动失败/重试、暂停/取消、跨 worker 消息和完成、迁移/监督全部回归。受限真实 AOT 运行 1000/10000 空闲 actor 与有限活跃子集，核对状态独立、FIFO、队表满、旧 Pid 拒绝及最终清理，CPU/墙钟限制不变。完整 workspace/check/clippy/fmt/MSRV、debug/release AOT 与 Linux 独立验证另记。

该探测仍是功能与逻辑预留证据，不替代完整 Bytes 同业务状态、开环到达、热点/公平性、RSS、尾延迟或 BEAM 双目标门禁。完整 MVP 仍按原路线图验收。

macOS debug/release 的 1000/10000 actor 探测及完整 AOT 已通过，具体环境、逻辑计费、既有 TCP 偶发失败和剩余门禁见[2026-11-14 验证记录](../validation-2026-11-14.md)。

## 对照与重审

位图可跳过部分空闲槽，但仍需按容量扫描，且不直接提供 FIFO；无界工作队列会引入重复 token 和未计费增长。先用锁内固定 intrusive FIFO 保留容量与唯一执行权。后续只有同业务测量表明锁、语言 pending 扫描或固定迁移为瓶颈，才分别评估分片、deadline 结构与窃取，不提前移除检查或改变活栈边界。
