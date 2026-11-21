# ADR-0065：双发送者 Counter 的各自 FIFO 与合法交错

- 日期：2026-11-14
- 状态：已采纳（有界正确性切片）
- 扩展：[ADR-0063](0063-multi-actor-byte-counter.md)

## 决定与边界

新增独立正确性协议 v3，不改变 v1/v2、语言语法或受管 ABI。Wings 使用两个真实宿主线程，BEAM 使用两个真实发送进程，直接向相同业务 actor 投递请求；不得由一个代理串行代发。发送者同时释放开始闸门，各自最多 16 个在途请求，总计最多 32；每条流最多 128、总计最多 256 条。不规定跨发送者或跨 actor 的全序，不用固定交错消除竞争。

完整 Bytes 状态前 32 B 为 counter:i64、actor ID:u64、sender1 已提交序号:i64、sender2 已提交序号:i64，其余零。32 B 业务请求仍为 sequence/operation/delta/reserved，sender 与 actor 在信封携带；每发送者到每目标独立从 1 连续编号。Apply 在构造候选前检查该序号恰比已提交值大一；更新 counter 与相应序号，正常返回才提交。真实成功回复返回提交后的 counter。两端停止前核对实际状态的两组序号及最终值；Wings 仍只导出长度/32 B 前缀，不宣称完整 padding 导出。

JSON 为 `{version:3, actors, state_bytes, initial, streams:[[actor/delta...],[actor/delta...]]}`。actor 1..100，三种 state、输入 64 KiB 与输出 1 MiB；只接收对所有交错均不会溢出的有界加法集合（initial 加负 delta 总和/正 delta 总和均须在 i64 内）。失败与溢出已有 v1/v2 独立对照，此协议不伪造确定的并发失败顺序。

独立 Python 模型按每目标的两条序列做有界前缀动态规划：每步只能消费一条流的下一个请求，checked 加法及回复值必须匹配，最后与实际提交状态一致。不直接要求两端回复值完全相同；合法交错可不同。加入伪造回复/交换同目标序号/错误终值的模型拒绝回归，防止验证器只核对交换律总和。BEAM 发送者按实际回复到达顺序检查各目标 sequence，不能先排序掩盖乱序；Wings 用唯一结果 handle 与业务状态序号检查，不声称宿主提供全局完成事件流。

业务 CPU 5 秒/墙钟 10 秒/禁 core、actor 阶段共享 3 秒 deadline、AOT CPU 20 秒/墙钟 30 秒不变。Wings 2 worker、32 个结果槽、每目标 mailbox 32、堆四槽，VM/tenant 256 MiB、process 4 MiB；为避免闲置 mailbox 放大，此协议 actor 上限实现先取 100（超过时拒绝），v2 继续验证 10000 actor。两个宿主线程和有界外部输入/结果是测试驱动内存，不伪称全部受管计费。

## 验证要求

三种完整 state、同目标竞争、多个目标、零/正负 delta、非均衡/空流、跨 16 条窗口、每条流 128 条、checked 两端安全集合；非法 JSON/桥接先拒绝再装载。真实三端须显式 `--compare-beam`，默认完整 AOT 只执行 Wings/独立模型。保持原限制，完成 workspace/check/clippy/fmt/MSRV、debug/release 完整 AOT 与平台回归，不用该有界闭环功能切片替代开环、过载/超时、热点/公平性、最新原生双平台或同负载 BEAM 吞吐/整机内存门禁。
