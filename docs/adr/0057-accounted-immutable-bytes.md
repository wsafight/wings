# ADR-0057：计费不可变 Bytes 与分块构造

- 日期：2026-11-13
- 状态：已采纳（业务值集成切片）
- 扩展：[ADR-0018](0018-bounded-record-heap.md)、[ADR-0043](0043-chunked-runtime-gc.md)

## 决定与理由

固定 Int 字段和列表不足以表示实际协议载荷及 256 B/4 KiB/64 KiB 状态。增加独立不可变 Bytes 对象，而不是用整数列表模拟字节或把宿主未计费 buffer 冒充业务 state。Bytes 不是字符串，不隐式解释 UTF-8，也不改变待评审的类型大小写。

- ABI 升至 v13，增加 BytesRef、四个 HeapCall 操作及 InvalidBytes 状态；context/EffectRequest 布局不变，旧产物必须重编译。类型 Bytes 可用于唯一 state、局部和有界 fn 参数/结果，根链、条件合流、state while 和持久提交沿用精确引用规则。当前拒绝消息参数与回复，不向宿主公开 handle。
- fill_bytes(length, value) 构造 0..=65536 字节、各字节为 0..=255；bytes_len 返回长度；byte_at(bytes, index) 读取整数；replace_byte(bytes, index, value) 复制后替换一个字节。非法长度、字节或负数/越界索引正常返回 InvalidBytes，不写候选输出、不修改旧对象；零长对象合法但没有合法索引。
- 每个 Bytes 占一个原堆槽，buffer 独立分配且不可变。固定 buffer 所有权表先计入堆控制容量，数据字节向原 process/tenant/VM 三级预留后 fallible 分配；未发布 builder、GC 重试及挂起/取消期间也持有数据票据，以堆身份拒绝跨堆发布，不借新目标账户逃逸额度。零长对象仍占槽与控制容量。数据预留或槽不足先从精确根收集，分别只重试一次；已提交对象不会为新副本让出额度。
- 构造/复制每块最多 256 字节，在每块前扣 runtime 工作单位并检查预算/取消；分配槽扫描和 GC 继续使用原有 16 节点工作块。Rust 不跨 yield 持有 heap 的独占借用；源 Bytes 在 compiler 根中存活。未发布 builder 在失败/取消时析构归还字节，目标只在全部填充及合法槽准入后发布。
- GC 将 Bytes 视为无出边叶节点；保活来自 compiler/宿主持久根或记录 Ref 字段。清扫及堆 Drop 释放 buffer 和字节票据，generation 重用/耗尽与原堆一致。get/布局检查拒绝把 Bytes 当记录或列表，不把任意二进制内容当引用扫描。heap_stats.charged_bytes 包括已发布 buffer，未发布 buffer 仅反映在账户 used 中。
- HeapCall 的 argument 指向 compiler 生成的固定三整数 scratch，回调先复制参数再覆盖 output；此为可信 native ABI，不验证任意外部指针，也不是原生沙箱。scratch 和活跃源引用按调用帧原预算计费，名称/宿主 JSON 边界不变。

## 验证与边界

测试空/最大长度、非法字节/索引、共享旧对象、布局混用拒绝、传递根、GC 回收与代次、三级拒绝回滚和取消时未发布 builder 清理。真实小堆 AOT 验证临时/持久 state、fn/条件/回边根、分块暂停不重放、失败不提交、跨 worker 返回迁移与监督新堆。执行 workspace/debug/release/MSRV、受限 AOT 和平台回归，不放宽子进程 CPU/墙钟限制。

`--check-byte-roots` 用独立回调堆在每个 compiler 检查点/heap 回调前收集，验证源对象、跨 fn 根、分支/循环合流、候选出口及失败解链，不依赖运行时堆实现作为 oracle。数据分配最多 64 KiB，但 allocator 和页错误不提供硬实时保证。

## 对照与重审

继续使用 IntList 会把固定字节状态变成大量节点，不能视为同协议状态；直接保存宿主 Vec 则绕过业务根和额度。可考虑内联小字节、共享页或大对象池，但当前先保留可核算的固定所有权表及独立 buffer，不把候选优化当默认实现。若该表明显增加非 Bytes 进程成本，或消息/字符串需要更大对象及切片，需记录实际吞吐与整机内存结果后另立 ADR，不移除取消检查或改成共享可变数据。

后续字符串必须单独定义 UTF-8、长度/切片/边界和消息规则；通用记录、Bytes 消息/回复与大对象池不在本切片冒称完成。逻辑字节、计费字节和 RSS 是不同指标，尚无同业务 BEAM 性能/内存结论，M0/MVP 门禁不变。
