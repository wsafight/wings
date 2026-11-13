# ADR-0071：可选计费原子让出观测

- 日期：2026-11-14
- 状态：已采纳（有界观测切片，不是公平性门禁）
- 扩展：[ADR-0070](0070-bounded-fairness-heartbeats.md)

## 背景与决定

心跳端到端时间和 CPU 请求 pending 区间都不能识别真实原子让出。增加宿主侧可选 `PreemptionToken::new_observed`：在分配 observer 和 token 前经指定账户三级准入，固定空间只保留尚未消费的首次请求时间和最近一次已确认的让出，不创建事件队列。默认 token 不启用 observer，不读取时钟或增加 mutex 热路径。

观察模式下，请求线程在同一短锁内记录 `Instant` 后发布原子 flag；重复请求保留首个未消费时间，仍按原有 flag 合并。受管 slow path 消费 flag，将对应请求时间带回私有 checkpoint；只有协程真实让回 worker 栈后才记录第二个 `Instant`。两者之差包括 safepoint 等待、OS 调度、slow path 和切栈开销，是从请求发布前到 worker 取回控制权后的保守观测，不是硬实时保证或精确机器指令时间。计时线程仍只访问控制对象，不访问 actor、堆或活栈。

按步骤片主动让出、effect 让出不伪造原子请求样本；取消/预算优先返回、正常完成或 timer disarm 清除未消费请求也不生成样本。`observation()` 返回副本，`pending_since` 表示尚未消费，`last_yield` 保留最近一个确认；消费后到 worker 返回前可能二者皆空。它不是完整事件日志，重复/覆盖/未消费请求必须由实验驱动另行记账，不能把缺失样本算零延迟，也不能给多个并行执行者共用同一 token 后宣称逐 actor 归因。

新增观测不改变受管 ABI v14、编译器 safepoint、总预算、取消顺序、状态提交或活栈 owner。诊断 mutex 不跨受管执行或 coroutine suspend；锁中不分配，poison 恢复不 panic/unwind。observer 固定预留由全部 token/调用持有者释放后归还。现有固定调用控制预留必须继续覆盖增加的私有 checkpoint/控制对象。

## 验证与边界

单测覆盖默认路径无观测、首请求合并、真实 coroutine yield 后确认、步骤/effect 不误归因、清除/取消/预算不捏造样本、跨线程请求及 process/tenant/VM 准入失败回滚与最终归零。真实 AOT 百万次 checked 加法验证预请求让出及远端线程请求，完整算完并检查后续 state；未命中执行窗口的远端请求单列，不强求某种 OS 交错。

业务仍受原 CPU 5 秒/墙钟 10 秒/禁 core、总阶段 3 秒限制，完整 AOT 原 CPU 20 秒/墙钟 30 秒不变。此处不把观测接为默认调度负担，不据短时结果宣布候选 p99<5ms、90 秒公平性、OS CPU 配额、原生双平台或 BEAM 吞吐/整机内存双目标通过；之后长测须独立冻结输入、明确 observer 开销、样本缺失/覆盖和平台条件。
