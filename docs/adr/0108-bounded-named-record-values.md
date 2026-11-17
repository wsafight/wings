# ADR-0108：有界具名不可变记录值

- 日期：2026-11-17
- 状态：已采纳（本地/状态/函数业务值切片）
- 延续：[ADR-0018](0018-bounded-record-heap.md)、[ADR-0042](0042-managed-function-frames.md) 与 [ADR-0103](0103-accounted-utf8-string-values.md)。
- 后继：[ADR-0113](0113-bounded-named-record-message-expansion.md) 开放有界具名记录消息参数；本记录保留原本地切片的范围与理由。

## 决定

process 内增加 `record Profile { count: Int, label: String, enabled: Bool }`，通过 `Profile(1, "name", true)` 按声明顺序构造，以 `value.count` 读取。`record` 只在 process 声明位置识别，保留旧变量/字段名用法。记录与字段不可变，state 只能整体替换；同字段形状的不同记录仍是不同类型。不改变既有类型大小写或待审语法提案。

每个 process 最多 32 种记录，每种最多 8 个具名字段；字段可为 Int/Bool/Unit/RecordInt/IntList/Bytes/String 或更早声明的记录，禁止 Pid/RequestId、递归及前向记录。显式限制形成有界无环类型图，不限制正常堆中不可变对象共享。记录允许局部、state、分支/循环和非递归 fn 参数/结果；本轮不开放具名记录消息/回复、JSON 或跨进程原 handle。

源码 ABI 升至 v20，新增名义 RecordRef 与清单记录 schema，旧产物重编译；context/EffectRequest 布局和 4352 B 控制预留不变。IR/清单独立验证 schema、类型索引与构造/读取，不能仅依赖前端。每个实例的布局表先计费再分配，记录复用原最多 8 字段槽和精确 Ref 跟踪；每条构造有强制检查点，分配扫描/GC 沿原分块预算和取消。引用字段在构造/访问时校验实际类型、堆身份与代次，候选 state 验证名义布局后才提交。

schema 是只读代码元数据，实例独立拥有业务对象与已提交根。GC 沿字段 Ref 跟踪嵌套记录/列表/文字/字节，不以记录 shape 相同放行错误名义类型。失败保留旧提交，不回撤此前消息副作用。

## 验证与边界

验证空/8 字段/32 schema、重复名字、错误构造数量/字段类型、错名义类型、递归/前向/能力字段拒绝；LSP 共用解析/类型查询。IR/清单拒绝伪造索引/类型及外部记录签名，runtime 检查陈旧/错布局；Rust 堆对象 API 保留原异堆身份拒绝，受信任 ABI 的局部编码不携带跨堆身份且不得传入外部入口。真实 AOT 覆盖嵌套引用、跨函数/分支/循环根、GC/分块恢复、旧提交/超额/取消清理、单/双 worker/监督及 CLI 标量输出。

沿原额度、CPU/墙钟和阶段限制，不增加堆槽上限。具名记录传输、完整资源账本、公平/过载/故障、最新原生双平台与 BEAM 双门禁仍分别验收，不因本切片宣布 MVP 完成。

本机实际结果和首次 TCP 启动失败见[验证记录](../validation-named-records-2026-11-17.md)。
