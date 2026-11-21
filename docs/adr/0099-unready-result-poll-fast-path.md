# ADR-0099：未就绪宿主结果轮询快路径

- 日期：2026-11-16
- 状态：已采纳（局部锁竞争优化；性能须配对测量）
- 部分替代：[ADR-0047](0047-cross-thread-ingress-requests.md) 与 [ADR-0060](0060-shared-scheduler-reply-table.md) 对未就绪结果的全表加锁轮询。

## 背景

mixed-4k 五对探索性长测中，Wings 每轮 p99 高于 BEAM，且轮间波动明显。源码检查发现 `IngressRequest::try_take` 即使结果未就绪、请求期限未到，也要锁共享 ReplyTable；开环驱动会持续轮询实际 pending，与 worker 发布结果竞争。一次额外原限制 30s/60s 运行和三秒 macOS sample 保留在验证记录，不把采样中 worker 等待或 driver busy-yield 当作所有尾延迟的确定根因。

## 决定

- 结果表启动前预留固定 `AtomicBool` 数组及控制元数据，再 fallible 分配；每个结果槽一个标志，不引入每请求 Arc、通道或线程。按实际固定容量计费，不提高原资源上限。
- 新请求在持结果表锁时将本槽标志重置为 false，再返回带原始单调 deadline 副本的唯一结果 handle。旧 publisher 仍先校验 generation，不能设置新请求标志。
- 发布成功/失败或显式取消在原锁内确定唯一终态，随后 Release 写 true；Condvar 通知仍在解锁后。真实结果、caller/publisher、票据和槽代次仍只在原锁内读写。
- `try_take` 先拒绝已消费 handle；Acquire 读到 false 且当前时间早于 handle deadline 时直接返回 None，不取表锁。其余情况走原锁内路径，重查并消费真实终态。即使标志为 false，期限到达也必须进入慢路径选择 TimedOut。
- 就绪标志不代表可在锁外读取业务结果，不代替 publisher 生命周期；超时、取消、Drop 不撤回已发布消息，仍等 caller 与 publisher 都结束后复用槽位。`wait` 保留原 Condvar 与期限规则。

ABI v15、4352 字节执行控制预留、语言语法、safepoint、总预算、quantum、队列/请求容量和原测试时限均不变。语言 RequestId 表不使用此快路径。

## 验证与回退

确定性测试在持有结果表锁时调用未就绪 `try_take`，必须返回 None；验证已消费拒绝、成功/取消可见、64 槽反向并发发布、各槽隔离、复用先清标志、旧 generation、零/正 deadline 与迟到 publisher 容量保留，以及三级字节准入少一/精确边界和最终清理。原并发发布/取消/Drop/Condvar 竞争回归必须保持。

执行 debug/release workspace harness、完整受限 AOT、check/clippy/fmt/MSRV 和真实 BEAM smoke；优化前后固定同一业务、资源与 v6 的 30s 预热/60s 采样/五对交替重复。完整结果保留，不挑单次最好值。CPU 硬配额、原生 Linux 和双目标仍独立验收，局部结果不能宣布 MVP 或领先 BEAM。

回退时只撤销 readiness 数组、handle deadline 副本和提前返回分支，恢复原全表加锁路径；不改变任何业务协议或放宽验收。若需锁外消费结果、分片结果表或改变 slot 生命周期，应重新决策。

实际运行、失败与前后数据见[优化验证记录](../validation-optimization-2026-11-16.md)。
