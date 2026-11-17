# ADR-0116：具名记录回复的有界独立复制

- 日期：2026-11-17
- 状态：已采纳（MVP 业务值切片）
- 扩展：[ADR-0096](0096-copied-record-replies.md)、[ADR-0097](0097-accounted-int-list-replies.md)、[ADR-0098](0098-accounted-byte-replies.md) 与 [ADR-0106](0106-accounted-string-replies.md) 的叶值回复路径。

## 决定

开放具名不可变记录作为 handler reply。目标在 handler 正常返回后按 manifest 的名义 schema 检查记录根，并复制叶字段；不跨堆传引用、不共享可变值。嵌套记录按字段顺序深度优先展开，记录节点与叶值总数最多 64，列表叶合计最多 64 个整数，String/Bytes 叶合计最多 65536 字节。超限或 schema/布局不匹配时拒绝回复，目标保留旧提交状态并按原失败恢复路径处理。

复制载荷含固定叶槽、列表数据、字节数据及脱离 module/actor/heap 的 schema 副本。所有可增长存储在分配前按原 process/tenant/VM 额度准入；结果克隆共享同一不可变租约，最后一个持有者释放时归还。回复方的导出副本与请求方导入的业务值分别计费。语言 request 仅向声明同名且字段递归一致的 completion schema 导入；完成事件按叶参数重建嵌套记录并经精确根保活。

TimedOut、Cancelled、Failed 的 completion 仍带与声明记录形状相同的默认空值（标量为零，列表/字节/String 为空），业务必须先检查 status。它们不携带真实回复副本。已发布消息和请求结果不因取消而撤回；导入失败不改变请求终态，不提交 handler 候选状态。

受管 `EffectRequest`、`ScalarContext` 和 `ScalarOutput` 布局不变，ABI 保持 v21，执行控制预留仍为 4352 B。Manifest 增加完成签名的名义记录元数据并升至 format v2；容量、CPU/墙钟/阶段限制均不变。此切片不开放任意 JSON 对象、动态记录派发或未界定结构值。

## 验证

实际执行记录见[具名记录回复验证](../validation-named-record-replies-2026-11-18.md)。

验证要求覆盖展开上限含界/越界、深层与空记录、各叶值类型、三层账户拒绝零变化、独立复制及根 schema 生命周期；真实 AOT 覆盖 collect/让出、无效布局/额度失败、worker 与 Scheduler 一/多 worker completion、timeout/cancel/failure/stop 竞争、宿主结果保留和完整清理。所有全量检查使用原固定限制；通过本地功能回归不等于双平台 BEAM 性能/整机内存门禁。
