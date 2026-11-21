# ADR-0063：多 actor 固定字节 Counter 正确性

- 日期：2026-11-14
- 状态：已采纳（有界正确性切片）
- 扩展：[ADR-0059](0059-byte-state-counter-equivalence.md)、[ADR-0062](0062-sparse-ready-scheduler.md)

## 决定与理由

标量稀疏探测不代表完整业务状态。接入真实 10000×256 B、1000×4 KiB、100×64 KiB 不可变 Bytes Counter，每个 actor 的状态含独立 ID，空闲 actor 同样保留完整状态；用真实 Wings Scheduler、BEAM actor 和独立 Python checked 模型核对，不用列表或标量替代。

- 新正确性协议 v2 使用单外部发送者、最多 256 请求、总在途窗口 32；允许请求定位 actor，顺序只约束同发送者到同 actor，不要求跨 actor 回复全序。每个请求逻辑仍为 32 B，路由 actor ID 为宿主信封，不冒充原业务字节。
- actor 数 1..=10000，状态大小仍为 256/4096/65536 B，逻辑状态总量至多 16 MiB。字节布局是 counter:i64 BE、actor ID:u64 BE、其余零；checked i64、加法溢出优先、注入失败和正常返回后提交不变。失败 actor 的已投递队尾全部终结，不重放；其他 actor 继续执行。
- Wings 复用 SchedulerSupervisor 的 Never 与独立终态票据，避免多目标同时失败覆盖最近失败缓存。固定结果表 32、堆四槽、2 worker；每 actor mailbox 取输入中任一 32 条窗口对该目标的最大请求数，至少 1、最多 32，不给空闲 actor 全部预留 32 槽。此处用于闭环 FIFO 验证，不冒充性能协议的 mailbox 1。VM/tenant 字节额度 256 MiB、process 4 MiB；不改变请求/调度 ABI 或语言语法。测试宿主最多存 10000 个固定配置/结果槽及 256 条输入请求，不产生无限发送者。
- Scheduler actor 缓存已提交 Bytes 的长度与最多 32 B 前缀副本。初始化完成及 handler 正常/失败返回后从旧提交根复制，不读取候选或挂起栈；byte_state_snapshot 在 owner 存在时拒绝，非 Bytes 返回 None。退出诊断复制同一旧提交副本，不持堆/账户/代码租约。普通快照仍不自动暴露字节前缀；只供可信宿主显式读取，可能含业务数据，不是完整导出/脱敏。
- BEAM 在收集时先检查每目标实际到达顺序及 monitor 终态，再将跨 actor 回复按输入序号规范化输出。不能先排序掩盖同 actor 乱序。Wings 对唯一结果 handle、每目标值序列及独立终态旧提交副本核对；不声称有全局完成事件流。
- Wings 只核对实际提交状态的长度、counter、actor ID 与前 32 B 中的零填充，不独立导出/证明余下 padding；完整状态由已回归的 Bytes 构造/复制原语保留。BEAM 停止活 actor 时检查完整零填充。`states_verified` 表示各端上述有限检查通过，不表示 Wings 导出了完整状态或两端分配布局相同。
- 输入上限 64 KiB；二进制桥接先完整验证头/数量/路由/操作/reserved/精确长度再装载可信模块。v2 输出最多 1 MiB，包含所有 actor 最终值/ID/长度确认和有限回复；v1 的 64 KiB 输出、5 秒 CPU/10 秒墙钟和 3 秒 actor 阶段限制不变。v2 同样限制业务子进程 CPU 5 秒/墙钟 10 秒/禁 core、共享 actor 阶段 deadline 3 秒，编译沿原 AOT 限制。

## 验证要求

最小/代表容量的创建、空闲状态、满字节状态、跨窗口 FIFO、checked 两端、多目标同时失败、故障队尾终结、独立旧状态及全部资源归零。三端核对必须显式 --compare-beam；默认完整受限 AOT 仅跑 Wings/Python。workspace/check/clippy/fmt/MSRV、debug/release AOT、两个首要平台另行记录，不以仿真代替原生门禁。

这仍是有界闭环正确性，不是开环负载、两个真实发送者、mailbox 100 热点、公平性或性能采样。吞吐/整机内存、预热/采样/重复/尾延迟、原生最新双平台与完整 MVP 门槛不变。
