# ADR-0117：CLI 具名记录回复的流式 JSON 输出

- 日期：2026-11-18
- 状态：已采纳（MVP 命令行业务值交付）
- 扩展：[ADR-0116](0116-bounded-named-record-replies.md) 的独立记录回复，以及 [ADR-0086](0086-accounted-cli-json.md) 的固定 CLI 适配器。

## 决定与理由

CLI 对具名记录请求调用 `invoke_record`，正常返回后根据结果自带的 schema 输出 `{"reply":{字段名:字段值}}`。嵌套记录输出嵌套对象，空记录为 `{}`，Int/Bool/Unit 分别为 JSON 整数/布尔/null。RecordInt、IntList、Bytes、String 叶沿用原独立回复格式，分别为 `{"record_int":N}`、`{"int_list":[...]}`、`{"bytes":[...]}` 和 `{"string":"..."}`。不输出受管 handle、schema 索引或运行时元数据；字段名与文本由 JSON serializer 转义。

serializer 按 schema 字段顺序一次遍历独立叶值，直接写入已有受控 writer，不创建 JSON Value 树、完整回复字符串或第二份业务载荷。遍历使用固定 64 槽的显式帧数组，不通过 Rust 函数递归；该固定控制存储纳入原 adapter 控制大小回归。沿用最多 32 种记录、展开至多 64 个节点、列表合计 64 整数和 String/Bytes 合计 65536 字节的原限制；JSON 转义和字节数组表示可以使输出大于输入的 64 KiB 行限，不借此提高业务载荷或 adapter 预算。

每条成功回复换行并 flush。写入或 flush 失败返回现有 CLI 错误路径；handler 在输出前已正常提交，输出失败不回滚已提交 state。所有 JSON 参数仍只接受原 Int/Bool/Unit，嵌套输入与原堆 handle 仍在业务前拒绝。此决定不修改 ABI v21、manifest format v2、4352 B 执行控制、264 KiB adapter 预留、16 MiB CLI 总额度或任何既有时限。

## 验证

真实 AOT 覆盖全部叶类型、嵌套/空根记录、i64、Unicode 转义、64 KiB 含界、GC 后回复及下一条 state 查询；超载荷/业务失败不输出成功回复。保留原标量/单向格式、引用输入拒绝、破管/flush 错误和固定控制大小回归。执行本机全量、MSRV 和 Linux 交叉检查，保留失败证据；这些不替代原生双平台与同机同负载 BEAM 吞吐/整机内存门禁。
