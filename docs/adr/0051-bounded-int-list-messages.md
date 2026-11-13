# ADR-0051：有界 IntList 消息复制

- 日期：2026-11-13
- 状态：已采纳（列表消息切片）
- 扩展：[ADR-0037](0037-immutable-int-lists.md)、[ADR-0049](0049-record-message-copies.md)

## 决定与理由

沿用不可变列表布局与独立进程堆，开放同 worker 同模块语言消息的 IntList 参数，不改类型拼写或引用回复。先验证有界序列复制，不把任意引用图或动态信封引入尚未闭环的资源路径。

- 未发布 ABI 升至 v12，EffectRequest 末尾增加 list_mask；旧 v1–v11 产物拒绝混用，已发布 0.0.2 不变。record_mask 与 list_mask 不重叠，必须逐位匹配目标消息参数类型。
- 每条消息最多 64 个参数，所有列表参数合计最多 64 个整数元素；空列表不占元素槽。源 effect callback 在源协程上读取有效列表节点，每节点先执行 runtime 预算/原子检查点，可原栈暂停或取消；源 compiler 根保活完整参数，暂停时不向 worker 暴露源堆指针。
- 固定信封保存 64 个纯整数元素，列表参数编码其连续段的起点与长度。按参数顺序排列，接收端校验无间隙、无重叠、总长度和 schema mask。信封没有源 handle、尾指针或跨堆引用；共享尾在不同参数中独立复制，不承诺别名身份。
- 超出总元素上限、源 stale/布局不符在入队前失败；已接受的 send/request 副作用不受源后续失败撤回。整条消息复制完才交 worker，不发送半条消息。超限是受管发送拒绝，不截断数据。
- 目标 handler 前从尾到头创建目标堆列表，每个空节点/cons 节点分配前检查预算与取消。每次分配后更新该参数的入口根，前面已导入的参数和当前尾在分块 GC、暂停与后续分配期间保活。失败/取消沿现有调用清理路径释放；目标堆容量不足是目标受管失败，不保证已接受消息必定成功。
- 固定载荷由 Message、ProcessEffect 和 invocation 按值持有，不新增动态信封分配；mailbox、actor 控制区与执行槽使用实际结构大小或已验证的固定预留计费。宿主/JSON/跨线程 ingress 不接受列表整数编码；仅语言私有载荷路径可导入列表。

## 替代方案与边界

动态 Vec 信封需要独立的逐级准入、跨账户转移和 fallible 增长契约，暂不引入。保留源堆/handle 会破坏消息隔离与源进程生命周期，拒绝。单次不检查的 64 节点遍历虽有上限，仍不符合 runtime 长操作检查约定，拒绝。

当前仍无引用回复、字符串、通用记录图、M:N 或完整 MVP；固定上限不是最终业务值容量承诺。未来放宽上限或引入图共享须另立 ADR，重新审计执行槽、栈帧和资源占用。

## 验证要求

覆盖空列表、负数与整数极值、共享尾、多列表混合标量/RecordInt、总元素 64/65、最高参数 mask 位、非规范段和重叠 mask。验证跨 actor、自发送、整数异步请求参数、源失败不撤回、目标满堆、GC 保活、源遍历和目标重建的单步暂停/取消及资源归还。宿主/JSON/ingress 的原 handle/段编码仍拒绝，引用回复仍拒绝。

运行 workspace check/test/clippy/fmt、MSRV 与 debug/release 受限 AOT；各平台单独记录，不放宽故障子进程上限，不用仿真结果代替 native 双平台或 BEAM 性能/内存验收。

2026-11-13 在 macOS 26.6.2 AArch64、Rust 1.99.1 通过 workspace check/test、clippy all-targets `-D warnings`、fmt、Rust 1.85.0 all-targets check、debug/release workspace 测试及完整受限 AOT，TCP 示例两测试在两种构建下均通过。AOT 包含 800 个单步 stop/Drop 边界，并确认覆盖源 64 元素遍历和目标中途重建；CPU 20 秒、墙钟 30 秒、禁 core 限制未变。Linux x86-64 隔离仿真增量通过 release 独立测试及完整受限 AOT，详见[验证记录](../validation-2026-11-13.md#abi-v12-增量6e390af)；无 native 双平台、列表吞吐或 BEAM 内存领先结论。
