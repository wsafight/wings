# ADR-0127：按 Unicode scalar value 索引和切片 String

- 日期：2026-11-18
- 状态：已采纳（定向 Debug/Release AOT 验证通过；完整 AOT 套件未通过）
- 延续：[ADR-0103](0103-accounted-utf8-string-values.md) 的 UTF-8、独立计费与分块执行约束。

## 背景

String 已支持构造、转换、字符数、拼接、相等比较、状态、消息与回复，但语言尚不能读取单个字符或提取子串。这是常见文本处理的基本能力，不能用 Bytes 索引代替，因为 UTF-8 字节偏移不等于字符位置。

## 决定

新增 `string_at(value: String, index: Int) -> Int`，按 Unicode scalar value 返回其 Unicode code point；新增 `string_slice(value: String, start: Int, length: Int) -> String`，以 scalar value 计数并返回独立计费的 UTF-8 副本。两者均支持完整 UTF-8 内容，不做 grapheme-cluster 分段、规范化或隐式 Bytes 转换。

`string_at` 要求 `0 <= index < string_len(value)`。切片要求 `start >= 0`、`length >= 0`，且 `start + length <= string_len(value)`；空切片允许位于字符串末尾。负数、加法溢出或越界以独立 `InvalidStringIndex` 受管失败，不提交候选状态。字符串 UTF-8 的内部不变量仍单独验证。

扫描和复制每至多 256 编码字节执行预算/取消/让出检查。切片目标先按 process/tenant/VM 预留；失败清理未发布 buffer 和额度，容量不足沿既有分块 GC 重试。源 String 依赖 compiler 精确根保活，结果只在完整复制和正常 handler 返回后提交或导出。

HeapCall 增加索引/切片操作码，非法索引状态码为 16，受管 ABI 升至 v22；旧产物须重编译。context、EffectRequest、4352 B 执行控制、字符串 65536 B 上限及原有消息/状态语义不变。

## 替代方案

返回单字符 String 会额外分配并掩盖字符的整数表示；本决策返回 Unicode scalar value。按 UTF-8 字节切片会暴露无效编码边界；按 grapheme cluster 切片则需要 Unicode 分段版本与更高成本，不属于最小运行时契约。

## 验证与边界

验证 ASCII、多字节字符、256 B 块边界、空切片、尾部切片、64 KiB、负数/越界、预算暂停、取消、GC、旧状态保留和额度清理。索引/切片实现本身不代表完整业务值账本、原生双平台、公平性或 BEAM 吞吐/整机内存门禁已通过。
