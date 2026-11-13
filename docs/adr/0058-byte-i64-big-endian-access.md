# ADR-0058：Bytes 的有界 i64 big-endian 读写

- 日期：2026-11-13
- 状态：已采纳（固定业务状态编码切片）
- 扩展：[ADR-0057](0057-accounted-immutable-bytes.md)

## 决定与理由

BEAM 协议 v1 的完整字节 state 以 signed i64 big-endian 保存计数器；语言只有单字节读写会迫使业务逐字节重建状态，且缺少位运算时无法正确表示 i64 两端。增加两项固定宽度内建，不引入隐式转换、可变 buffer 或新语法。

- `read_i64_be(value: Bytes, offset: Int) -> Int` 按二进制补码读取连续八字节，支持未对齐 offset 及完整 i64 范围，不分配。
- `replace_i64_be(value: Bytes, offset: Int, number: Int) -> Bytes` 沿原计费/根契约复制完整旧对象，再替换八字节，原对象保持不变。只发布全部构造完成的新值。
- offset 必须非负，且八字节都位于对象内；负数、加法溢出或不足八字节正常返回 InvalidBytes，不写候选输出或分配副本。不会跨业务对象读取，不允许按引用解码。
- 复制每块最多 256 字节，沿原预算/取消检查和单次 GC 重试；写八字节本身固定有界。读操作沿 compiler 强制检查点；不宣称 allocator 或页错误有硬实时保证。
- ABI 升至 v14，HeapCall 增加操作 12/13，复用三整数 scratch，context/EffectRequest、错误码及普通宿主标量协议不变。旧产物必须重编译；Bytes 消息/回复和字符串仍不开放。类型大小写仍待用户评审，不在本切片改变。

## 对照与验证

不使用宿主直接修改堆或不计费状态缓存，也不以八次整对象单字节复制冒充一次更新。通用切片、位运算和编码库以后独立设计，本切片只开放固定宽度协议所需的最小能力。

单测核对完整 i64 两端、正负数、未对齐/末尾合法 offset、不足长度/极端 offset、源不变、GC 保活、低预算与取消归还。独立 compiler 根模型和真实 debug/release AOT 覆盖嵌套调用、状态替换及失败不提交；按 workspace、MSRV 和平台门禁验证，维持受限子进程上限。固定字节编码可用于下一步 Wings/BEAM 正确性对照，但不等于该对照已实现或性能目标通过。
