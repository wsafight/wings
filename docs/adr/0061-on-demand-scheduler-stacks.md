# ADR-0061：调度 worker 按需借入计费原生栈

- 日期：2026-11-13
- 状态：已采纳（按需栈切片；macOS 功能已验证，Linux 与完整门禁另记）
- 部分替代：[ADR-0054](0054-bounded-multi-worker-scheduler.md) 的逐 worker 全 actor 容量栈映射与预留
- 扩展：[ADR-0012](0012-bounded-stack-pool.md)；固定池接口和原栈恢复契约保留

## 决定与理由

ADR-0060 消除重复结果表后，macOS AArch64 的 256 空闲 Counter、2 worker 仍预留 151569072 B，主要来自每个 worker 为所有 actor 预建 256 个 256 KiB 加 guard 的栈。空闲 actor 无活栈，不应仅因容量配置就映射并计费所有潜在执行栈。

- Scheduler worker 使用同线程按需 StackPool，最大借出数仍为 actor 容量，最多缓存两个正常返回栈。构造只预留池控制和固定缓存容器，不预留或映射潜在业务栈；已有公开固定池的构造、计费与 trim 契约不变。
- 第一次借入先向目标 process/tenant/VM 完整预留 256 KiB 可用栈及平台 guard 页，再调用已使用依赖 corosensei 的 DefaultStack 创建 guarded mapping。分配失败释放票据、不改变活跃数或消耗槽位；完整计费在映射之前，不把地址容量当免费 RSS。
- 缓存命中将原 tenant 票据净转移到目标 process，不重复增加共同祖先计费；process 拒绝时不取走缓存。执行期间独占映射和票据，不改变强制检查点、无 unwind、活栈线程固定、正常返回前不得释放的契约。
- 归还栈若缓存尚未满，票据净转回 tenant 后缓存；否则先解除映射再释放票据。trim 只回收缓存，不能影响活栈；最后活栈继续保留池所有权。使用 DefaultStack 的既有析构，不新增不可信崩溃隔离承诺。
- 新模式统计 reserved_bytes 为当前活跃加缓存映射字节、charged_bytes 为池控制加当前映射的逻辑计费；capacity 是最多同时借出数，不是已预留数量，缓存不保留业务 process。固定池统计语义不变，两种统计都不是 RSS。
- 动态准入会把部分失败从 Scheduler 构造转移到 handler 启动。未启动消息保持在原 FIFO 头，队满仍显式拒绝，其他可执行 actor 继续推进；不把暂时资源不足当业务退出，也不保证未准入工作最终能运行。stop/关闭必须清理该排队工作。
- 不增加可挂起栈迁移、不缩小单栈、不改变 ABI/语言语法或请求容量，不上调 256 actor 上限。固定 registry 扫描与公平调度、代表性多 actor/开环和原生双平台 BEAM 双目标仍需独立验收。

## 验证要求

单测验证空池零业务映射、逐目标三级拒绝无部分槽、满池、缓存复用净转移、满祖先缓存可用而冷借入失败、trim/退出归零、活栈独占/生命周期。Scheduler 验证不再按空闲 actor 数倍增栈成本、暂时启动失败保持 FIFO 及健康 actor 进展；完整受限 AOT 覆盖真实暂停/恢复、语言 spawn（父栈活跃时须另行准入 initializer 栈）、迁移、GC、监督与关闭，保持原 CPU/墙钟限制。记录同配置逻辑预留前后值，不用该数字宣称 RSS、吞吐或 MVP 通过。

macOS AArch64 相同 release Counter、2 worker、64 结果槽、mailbox 1 的 256 空闲 actor 填满逻辑预留从 151569072 B 降至 8946704 B；每 actor 增量仍为 1672 B。未改变单栈或同时挂起容量，没有 RSS/性能结论。完整数据、命令及平台边界见[验证记录](../validation-2026-11-13.md)。

## 对照与重审

只减少每 worker 固定池槽数会让已挂起栈占满后出现无法运行的 actor，并改变总活栈容量；仍预留全部地址但延迟记账会隐藏资源。选按需映射，保留真实活栈的完整成本，复用现有 guarded stack 实现。若映射/解除映射成为同业务实测瓶颈，可测量有界缓存大小或分块池，不提前增大缓存，也不取消准入、guard 或强制检查点。
