# ADR-0037：不可变 IntList 与传递根

- 日期：2026-11-13
- 状态：已采纳（M0/M1 业务值切片）
- 补充：[ADR-0018](0018-bounded-record-heap.md)、[ADR-0034](0034-record-state-loops.md)、[ADR-0036](0036-bounded-inline-functions.md)

## 决定与理由

单字段记录不能验证业务引用图。先开放持久不可变 IntList，复用已有布局化 RecordHeap 的传递标记，不引入可变业务数据、引用消息或新的 allocator。

- `IntList` 可用于局部、唯一 state 和内联 fn 参数/结果，不能作为消息参数/回复；不能与 RecordInt 互换、比较或伪造。`empty_ints()` 分配空节点；`prepend_int(Int, IntList)` 分配保存整数与尾引用的节点；`head_int`/`tail_ints` 读取节点，空列表返回受管 EmptyList 失败；`is_empty_ints` 返回 Bool。参数从左到右求值，每次仅处理一个节点，不提供隐式线性遍历。
- 空节点布局为 Unit，非空节点为 Int+Ref，尾只可引用同一进程堆的有效列表。节点不可变，构造时尾必须先存在，因此语言无法构造环。GC 用原有有界 worklist 从 compiler 和持久 state 根传递标记，引用前缀/共享尾分别保活，无后台收集。逻辑列表长度不等于物理计费字节。
- ABI 升至 v7，context 和 HeapCall 签名不变，增加 ListRef 值类型、操作码与 EmptyList 状态。prepend 的 scratch output 在调用前写入尾 handle，回调先读取并验证，再分配，成功覆盖结果；失败时该 scratch 不是候选输出。其余内建忽略 scratch 旧值。旧 v6 产物必须重编译，不能装载新语言到旧 runtime。
- 根计划把 Ref/ListRef 都当引用，但 IR/语言保留精确类型；state 提交校验所需节点形状和所属堆，诊断只显示类型，不公开编码。根链不携带类型，GC 只跟随实际布局。编码仍不认证跨堆来源，不是 native 沙箱。
- 同样使用最多 1024 记录槽、64 compiler 根及宿主持久根；空节点也消耗一个槽，不作无账目 singleton。准入、分配失败收集重试、候选正常返回才提交和失败完整清理保持不变。所有新增布局/元数据先计费；完整 GC 最坏仍按固定堆上限有界，后续扩容须重审分块。

## 验证与重审

类型/IR/清单测试禁止标量伪造、RecordInt 混用及跨消息边界；精确根测试每个内建/慢路径前收集，覆盖临时前缀、共享尾、条件合流、state 回边、fn 参数/结果与持久提交。真实小堆验证 push/pop、空读取、容量失败、逐片恢复/取消/预算、失败不提交、引用隐藏及三级清理。debug/release、MSRV、双平台分别验证。

不宣称字符串、通用记录、引用消息或全部业务值已完成；不改变非移动 GC 或共享可变数据约束。新增节点种类、跨堆消息或可变字段须另立所有权/计费与验证契约，不视为 MVP 或 BEAM 性能结果。
