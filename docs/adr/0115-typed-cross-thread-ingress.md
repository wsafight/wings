# ADR-0115：跨线程入口的有界类型化值复制

- 日期：2026-11-17
- 状态：已采纳（MVP 外部事件业务值切片）
- 部分替代：[ADR-0046](0046-bounded-cross-thread-ingress.md) 与 [ADR-0047](0047-cross-thread-ingress-requests.md) 的仅标量参数限制。

## 决定

固定单 actor 单 handler 跨线程入口新增 `IngressSender::send_values` 和 `request_values`，使用既有 MessageValue 叶类型，支持 Int/Bool/Unit、RecordInt、IntList、Bytes 与 String。同线程宿主与跨线程入口复用一份 schema/聚合限额检查及内容复制实现；不发送源堆 handle、不共享可变业务数据。原 `send`/`request` 保持标量专用，不将整数解释为引用或内部段编码。

单条参数最多 64 个，列表合计最多 64 整数，String/Bytes 合计最多 65536 B，入口槽数仍为 1..=64。类型/聚合验证先于载荷分配；字节副本先向原目标 process/tenant/VM 准入，信封与请求槽沿既有票据。入口只保存固定类型数组，不让可跨线程 sender 额外保留模块/handler/actor。宿主复制有固定上界但不进入受管执行；目标原栈逐块独立导入、精确根、GC/预算/取消复用既有路径。

发送成功前必须完成独立副本；发布成功后 WakeFailed 仍是已接收，不能重试。满队列、额度拒绝或关闭不发布，释放未发布副本及请求双端票据。timeout/cancel/Drop 不撤回消息，结果槽须等旧 publisher 实际结束；目标导入失败不执行业务、不提交状态。stop 清理入口队列，旧 sender 不指向重启实例，控制/通知租约延续到末 owner。

不更改 ABI v21、4352 B 调用预留、堆/栈/消息/请求额度或 CPU/墙钟/阶段限制。不增加 JSON 对象/引用参数、具名记录回复或通用 reactor；具名记录消息的宿主调用仍遵循清单展开叶签名。完整账本、长时公平/故障、原生双平台和 BEAM 双门禁独立验收。

## 验证

实际命令、失败证据与平台边界见[验证记录](../validation-typed-ingress-2026-11-17.md)。

固定类型/混合载荷、64 KiB/64 整数含界与超界、独立副本、三级拒绝零变化、满队列/关闭/旧 sender、请求 timeout/cancel/Drop 仍保留 publisher。真实 AOT 覆盖宿主线程发送、目标 GC/逐片导入、原提交/失败与清理；运行原全量 debug/release、MSRV/交叉检查和 TCP 回归，不扩大任何原限制。
