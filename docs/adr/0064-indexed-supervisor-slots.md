# ADR-0064：监督子进程索引与固定空槽链

- 日期：2026-11-14
- 状态：已采纳（有界监督表实现）
- 部分替代：[ADR-0056](0056-multi-worker-supervision.md) 的创建/查找/停止扫描
- 验证负载：[ADR-0063](0063-multi-actor-byte-counter.md)

## 原因与决定

多 actor 正确性在 macOS 通过，但 Linux x86-64 隔离仿真的 10000×256 B 用例超过既有 3 秒 actor 阶段 deadline。监督表创建寻找第一个空槽、current_pid 和 stop 按稳定 child 身份线性搜索，使批量创建/清理存在平方级扫描；不能靠放宽时间限制或删大容量用例掩盖。

- 固定表改为每槽 child/next，加一条预分配空槽链。创建只取链头，所有可失败准入/初始化完成后才摘头和发布 child；失败保留原槽和链。成功 stop 或 poll 的最终终结归还槽，成功重启继续使用原槽。
- 私有 `SchedulerChildId` 增加监督槽索引，保留原始 SchedulerPid 作为稳定身份。current_pid/stop 先按索引定位，再完整比较身份，拒绝旧代、外来 supervisor 和空槽；重启更新当前 Pid，但不改变 child ID。不向语言或受管 ABI 公开该索引。
- 表/空槽元数据仍在构造前按实际固定容量向 tenant/VM 准入，不新增散列表、可增长队列或后台线程。消除创建/查询/停止扫描；poll 仍按固定 cursor 最多扫描容量，不声称所有监督操作 O(1)。
- 保留 once-only 终态、旧资源清理后发布、新实例重启、强度上限、失败创建不重试及状态/副作用契约。未改初始化栈、业务状态或计时检查，不因这次改动宣称性能/内存优于 BEAM。

## 验证

满表拒绝无计费变化，任意顺序 stop、多轮槽复用拒绝所有旧身份，外来 child 拒绝，创建准入失败可再次使用空槽；重启成功保留稳定身份，Never/强度上限/初始化或额度失败归还槽。保留并发失败、活栈取消、其他 actor 推进及完整清理回归。重新执行 macOS debug/release、MSRV、clippy/fmt、真实三端 Counter 与 Linux 全部 release harness/受限 AOT，原 CPU/墙钟/3 秒阶段限制不变。隔离仿真不替代最新原生双平台与 BEAM 双门禁。
