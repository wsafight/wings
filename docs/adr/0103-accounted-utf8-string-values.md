# ADR-0103：计费 UTF-8 字符串值

后续：[ADR-0104](0104-bounded-string-literals.md) 增加字面量；[ADR-0105](0105-accounted-string-messages.md) 扩展 String 消息参数，替代本文当时仅本地值的消息边界，回复限制保持。

后续：[ADR-0111](0111-bounded-string-content-equality.md) 开放分块内容 `==`/`!=`，替代本文暂不比较的限制；不增加排序或规范化。

- 日期：2026-11-17
- 状态：已采纳（本地/状态/函数切片）
- 延续：[ADR-0057](0057-accounted-immutable-bytes.md) 的不可变计费缓冲、分块复制与精确根约束。

## 背景与决定

MVP 的业务值仍缺字符串。Bytes 允许任意字节，不能仅改名为字符串。本轮增加独立 `String` / `StringRef` 类型和堆标签，内容必须为 UTF-8，编码长度仍为 0..=65536 字节。非移动 GC 沿用精确根与固定槽位；字符串与 Bytes 均不含子引用。

开放 `string_from_bytes(Bytes) -> String`、`string_bytes(String) -> Bytes`、`string_len(String) -> Int` 和 `concat_strings(String, String) -> String`。转换复制内容而非别名；长度按 Unicode scalar value 计数，不是字节、字素簇或显示宽度。不做隐式转换、规范化或区域相关比较。非法编码和拼接超限正常返回受管错误，旧提交状态不变。

每次复制先按原 process/tenant/VM 预留目标字节，每至多 256 字节检查取消/让出并扣 runtime 预算；UTF-8 校验与计数也分块，块末不完整字符至多回退三字节留待下一块。源引用和候选状态通过原 shadow stack 保活；失败丢弃暂存和票据，容量耗尽沿原分块 GC 重试。执行控制预留和原资源上限不变。

源码 ABI 升至 v16，context/EffectRequest 布局不变，旧产物须重编译。新增字符串错误状态；清单、IR 和语义检查器均拒绝 String 消息及回复，不能借整数或 Bytes 消息绕过。支持字符串局部、state、分支/循环合流和有界非递归函数；暂不提供字面量、消息/回复、通用记录、索引/切片或内容比较，不改已有大小写规则或待审语法方案。

## 替代方案与边界

直接复用 Bytes 类型无法保证 UTF-8，不采纳。共享同一个 buffer 需要新增所有权与转移契约，本轮选择已验证的独占复制。一次开放所有字符串语法和传输会扩大验证范围，后续独立推进；本切片不等于 MVP 完成。

## 验证与重审

检查空值、ASCII/中文/四字节字符、跨块边界、非法/截断/过长 UTF-8、64 KiB 边界、源类型/陈旧引用、分块预算/取消和三级额度回滚。真实 AOT 验证状态、跨帧根、GC/让出、失败保留旧提交与最终清理。原生双平台及 BEAM 同负载吞吐/整机内存仍独立验收。未来开放字符串消息/回复、字面量或共享 buffer 时重审本决定。
