# ADR-0133：同业务开环长测的 worker 锁诊断

- 日期：2026-11-18
- 状态：已采纳（诊断入口；实际运行结果另记）
- 延续：[ADR-0102](0102-opt-in-worker-lock-statistics.md) 的默认关闭锁观测；复用 [ADR-0069](0069-streamed-open-loop-measurement.md) 的业务/事件/阶段。

## 决定

将已有四类 worker 状态锁统计接入 `beam_open_loop --measure ... --observe-locks`。原无参数路径保持关闭；新参数只允许独立长测，不能隐式改变普通 smoke 或 BEAM。原 actor/state/mailbox/pending、CPU/墙钟/业务期限、40 B 事件、30s 预热/60s 采样和资源上限保持；开启后 Scheduler 原有固定统计存储仍先向 tenant/VM 计费。

全部请求结束并核对状态、停止 actor 后，先关闭/join worker，再从仍持有计费控制租约的 handle 读取每 worker 的 claim/start/snapshot/finish 累计。输出版本 1 的独立 `worker_lock_statistics` 元数据，声明 `whole_scheduler_lifetime`、`wall_time_not_cpu` 与已 join。它包括初始化、预热、采样和收尾，不能按六十秒采样区间解释。关闭模式不输出此对象，原 v6 业务和三条控制事件不变。

每个 phase 保留获取次数、获取前等待/锁内墙钟的总和及最大值、溢出标记。停止后独立校验无饱和、最大值不大于总和、完整 worker 身份，且全部 start/finish 次数须等于 Configure 数加最终已提交增量。业务仍必须通过原逐条事件重放与最终状态核对，不能以统计自洽替代业务正确。

独立诊断驱动只运行既有 hot-queue 80000/s，两对开关反序（off/on、on/off），每次保持原 30s/60s。执行冻结的 runner、业务产物、Python 驱动/重放器及原生内存 probe；前后校验 SHA，保留原始事件、统计、错误和 invalid manifest。普通 AOT 不启动此长测。独立模型和参数拒绝进入短回归。

## 判读与回退

开启统计增加时钟与原子更新开销，两对对照用于观察扰动，不是五对性能门禁。锁内墙钟可包含 OS 停顿，不能称为独占 CPU；这里没有 timer mutex、Condvar 睡眠或宿主/请求表锁数据。不能用这些累计值解释全部发生器迟发、推导最大吞吐，或据此恢复已回退的计时通知/空表候选。只有补齐 CPU/时序和相同业务消融后，才选择具体优化。

本切片不改变 VM 实现、语言或 ABI v22，不减少取消/让出检查，也不提高 4352 B 执行控制预留。删除诊断 CLI/驱动即可回退，原 v6 对照与既有锁统计 API 保持。完整账本、启动根因、过载/服务 I/O、原生双平台及 BEAM 吞吐/整机内存门禁仍独立验收。

实际运行与验证状态见[验证记录](../validation-open-loop-locks-2026-11-18.md)。
