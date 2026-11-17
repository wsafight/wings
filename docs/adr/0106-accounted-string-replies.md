# ADR-0106：计费 String 回复与完成事件

- 日期：2026-11-17
- 状态：已采纳（字符串交互切片）
- 延续：[ADR-0105](0105-accounted-string-messages.md) 的独立 String 参数与 [ADR-0098](0098-accounted-byte-replies.md) 的结果生命周期。

## 决定

开放 `-> String`、匹配第三参数为 String 的系统完成事件与宿主 `StringReply`。`RequestOutcome::ReplyString`、`ExecutionStatus::CompleteString` 和 `invoke_string` 与 Bytes 保持独立类型；不能以整数 handle、BytesReply 或 JSON 字节数组冒充 String。CLI 输出 `{"reply":{"string":"文字"}}`，仍不开放 JSON 引用参数。

源码 ABI 升至 v19，原 v18 的 80 B EffectRequest/88 B context 和 4352 B 执行控制预留不变；旧产物重编译，消息总上限、堆/栈/请求/队列额度不提高。出口 String 引用与候选 state 同时保活，至多 256 B 检查预算并导出独立计费副本；结果控制块也先三级准入，失败不提交候选 state。

StringReply 只保留结果数据与计费租约，不保留 actor/模块/堆/栈。其内部可以复用不可变字节存储，但构造时分块校验 UTF-8，并由独立类型封装保证只读 str 有效；BytesReply 构造不接受 String 掩码。clone 共享结果存储，最后拥有者才释放；不把共享结果 buffer 当成跨 actor 共享可变业务值。

系统完成事件使用 String 子掩码导入目标独立 String，非成功终态为有效空字符串，调用者须先检查 status。超时、取消、发起方退出与目标迟到完成沿原双端生命周期，不能提前释放 publisher 或撤回已发布请求；满完成队列保留结果直到可投递或 owner 退出。先检查目标回复与 callback 类型完全一致，不能跨 Bytes/String 配对。

## 验证与不做事项

验证空/中文/emoji/NUL/64 KiB、String/Bytes 错配、出口 GC 根、逐块预算/取消/Drop、三级存储与控制拒绝回滚、源退出后结果存活、末 handle 清理及成功/失败/超时/取消完成事件。真实 AOT 覆盖 direct、单 worker、一/双 worker Scheduler、ingress/监督与 CLI 输出，沿 CPU 20s/墙钟 30s/core 0/阶段 3s；不以转换成 Bytes 的回包代替 String。

通用记录、完整分配/启动/装载账本、长时公平/过载/故障、最新原生双平台及同负载 BEAM 吞吐/整机内存双门禁仍独立验收，不因字符串交互完成宣布 MVP 完成。
