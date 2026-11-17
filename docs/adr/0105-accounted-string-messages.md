# ADR-0105：计费 String 消息参数

- 日期：2026-11-17
- 状态：已采纳（消息参数切片）
- 延续：[ADR-0103](0103-accounted-utf8-string-values.md) 的独立类型与 [ADR-0076](0076-accounted-byte-messages.md) 的复制/净转移协议。

## 决定

开放 String handler 参数与 send_self/send/request 的参数，以及宿主 `MessageValue::String(&str)`。String 与 Bytes 共用一条消息最多 65536 编码字节的上限，不分别累加额度；同一引用出现两次按两份内容计数。单独 String 掩码区分类型，不能拿 Bytes 或裸整数冒充 String。String 回复仍拒绝，后续独立接入，不把转换后传 Bytes 称原生 String 回复。

源码 ABI 升为 v18，EffectRequest 增加 string_mask，长度 80 B；context 仍 88 B，执行控制预留仍为 4352 B，不提高任何资源上限。旧产物须重编译。内部 BytePayload 使用合并 buffer 掩码及其 String 子集，发布前与目标完整 schema 检查类型/分段，拒绝掩码重叠、越界或不匹配。固定 metadata 按实际类型大小准入。

源 actor 在导出前预检独立引用标签和合计长度，三级预留暂存，再按最多 256 B 检查/复制；发布时向目标净转移，拒绝不丢账。目标另行预留受管副本，分块验证 UTF-8 并创建独立 String 槽，先前导入参数保留入口根，避免 GC 回收。宿主先检查全部参数类型/合计上限，再预留和复制，不保留借用。暂停时消息拥有副本，不能引用发送者堆。

超时、取消或丢弃请求结果不撤回已经发布的消息，结果与 publisher 旧生命周期规则不变。导出/导入失败不提交候选状态，已发生 send/spawn 不回滚；取消在分块检查点处理，不跳过资源检查换取吞吐。

## 替代方案与验证

仅提供 String 到 Bytes 转换不足以保留业务类型；直接传引用违反进程隔离；给每种类型单独 64 KiB 会提高消息上限，均不采纳。

验证混合 String/Bytes/RecordInt/IntList、空值/中文/emoji/NUL、64 KiB 合计与重复引用超限、伪造掩码/类型、三级拒绝回滚、导入 GC/取消/预算、超时/停止清理。真实 AOT 覆盖单/双 worker 及已发布副作用不撤回，沿用 CPU 20s/墙钟 30s/阶段 3s。MVP、完整账本、最新原生双平台、公平/故障与 BEAM 双门禁仍独立验收。
