# ADR-0072：Scheduler 的有界让出样本与计数

- 日期：2026-11-14
- 状态：已采纳（调度观测切片，完整公平性门禁未通过）
- 扩展：[ADR-0071](0071-observed-atomic-yield.md)、[ADR-0070](0070-bounded-fairness-heartbeats.md)

## 决定

`SchedulerConfig::yield_samples` 默认为 0，关闭观测；显式 1..=65536 时，每个 worker 在启动前创建独立计费 token 和固定 FIFO。容量按每 worker 配置，tenant/VM 在分配前准入，失败回滚所有已创建控制对象；process 不承担实例级诊断容量。默认路径不分配 observer/FIFO、不读诊断时钟、不取诊断锁，不改变 ABI v14、强制检查点、取消/预算和既有调度顺序。

启用后，只在 worker 获得 coroutine 控制权、确认原子请求后发布样本，附完整 SchedulerPid、worker 索引和该 worker 单调序号。序号按所有已确认让出计数，即使 FIFO 满而丢样也继续；FIFO 满拒绝新样本并计 `dropped`，不覆盖旧值、不阻塞等待宿主。宿主以 `take_yield_sample(worker)` 取一次，以 `yield_statistics(worker)` 取副本；关闭模式返回 None，无效 worker 显式报错。handle 保留固定诊断容量及账户租约直到最后释放，但样本不持有 actor/模块/业务堆或栈。

token 增加固定计数：全部 request 调用、合并请求、消费、clear/disarm 丢弃和真实确认；返回当前 pending 及 `overflowed`。请求计数满足 `requests = coalesced + consumed + cleared + pending`；消费与确认间可有一个尚未回到 worker 的请求。整数溢出置标志并饱和，不静默回绕；带溢出的实验不能宣称完整。步骤片/effect、取消/预算、正常完成不伪造样本；timer arm 清除旧请求、timer disarm 丢弃迟到请求照常执行并记账。

FIFO 与 token 分别短锁，无锁跨受管执行/切栈；宿主活动查询不是跨锁原子快照，只有停止业务并等 worker 无 owner、无 pending 后才能核对完整守恒：`confirmed = retained + taken + dropped`、`consumed = confirmed`。查询/取样不重置 token 计数、FIFO 序号或覆盖计数。计时线程只持控制对象；实际样本归当前唯一 worker owner，不把跨 worker 活栈迁移或全局时间全序写成已有能力。

## 验证

单测覆盖容量 0/1/上界/越界、worker 索引、默认关闭、固定 FIFO 顺序/满丢弃/循环复用/序号缺口、observer 计数和饱和标志、tenant/VM 启动准入失败回滚、两个 worker 的 timer 请求/真实切回与取消清理、handle 留存及最终归零。真实 v7 负载增加独立 `--observe` 运行，保留原 101×256 B、百万 checked 加法、100 actor 同刻突发、mailbox/pending、总预算/片预算/quantum，不用提高 quantum 或关闭步骤片来制造样本。

短测 timer 可能一次也不触发；零样本明确为无数据，不报零延迟、不要求偶然 OS 交错。宿主逐条取样并保留请求/合并/清除/丢样计数；原 CPU 5 秒/墙钟 10 秒/禁 core、总阶段 3 秒、完整 AOT 20/30 秒不变。此处不改 BEAM 业务，不将启用诊断与默认模式的时间差当排名；长时公平性、开销、丢样、OS CPU 硬配额、原生双平台与 BEAM 吞吐/整机内存仍须独立验收。

另以原 Bytes `Hold` 产物跑 `scheduler_yield` AOT 边界回归：两个 worker、1ms quantum、无限步骤片、每 worker 一槽 FIFO，确保真实 compiler 检查点确实消费 timer 请求，验证满丢样、归属及取消退出。这是刻意触发边界的诊断测试，沿用完整 AOT 的 20/30 秒和内部总 3 秒限制；其参数不混入 v7 原业务测量，不把它的延迟当公平性数据。
