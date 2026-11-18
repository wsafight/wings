# ADR-0130：只读核对测量作业的 cgroup CPU 配额

- 日期：2026-11-18
- 状态：已采纳（测量控制切片；原生 Linux 验证待完成）
- 补充：[ADR-0069](0069-streamed-open-loop-measurement.md) 的 CPU 条件记录；原业务、阶段、重复和安全限制保持。

## 选择与范围

单 worker/单 BEAM scheduler 和 CPU 累计时间 rlimit 都不能替代并发 CPU 带宽上限。本轮只为计划开环与 CPU/心跳长测提供可选的 Linux cgroup v2 条件核对，不自行创建、配置或迁移 cgroup，不写系统控制文件。

操作者须预先把整个测量作业放入独占 leaf domain cgroup，启动时仅含测量 driver；其子进程继承该组。新增 `--cpu-quota-us`/`--cpu-period-us` 必须成对提供，与该组 `cpu.max` 精确相符且关闭额外 burst。范围明确包含 Python driver、Wings/BEAM 和普查/probe helper，不能把整作业配额称为单独 VM 配额。两个 runtime 顺序沿用同一组，原五对交替顺序不变。

从原生 Linux 的 `/proc/self/cgroup` 和 cgroup2 mountinfo 解析实际路径；保留目录身份、自己位置、所有可见祖先配额、cpu.stat、进程/线程身份及采样单调区间。拒绝无限预算、混用控制组、threaded/有子组、冻结、非 SCHED_OTHER、缺失统计、累计值回退和配置变化。每次控制文件/成员/祖先遍历有固定上限；遇到退出竞争不伪造完整样本，按不完整实验失败保留。

所有系统访问只读。macOS、混合/不可见层级、无 cpu controller 或未预置正确组时，在创建实验目录/启动测量前失败；未提供参数保持原探索性模式。读取条件后到下一采样之间仍假定可信宿主不移动任务、不修改控制组；这不是抗管理员篡改的证明，也不代替主机隔离、监测开销、内存、过载和双平台 BEAM 门禁。

参考 Linux 内核 [cgroup v2 文档](https://docs.kernel.org/admin-guide/cgroup-v2.html) 的 cpu.max、cpu.max.burst、cpu.stat、层级与成员语义。这里只记录实际控制与观测，不用 quota/period 推导实际可用 CPU 时间，也不把累计 CPU 秒数当带宽控制。

## 验证

本机 macOS 只验证无参兼容、显式参数早拒绝，以及使用临时普通文件的格式/变化模型；模型不调用内核 cgroup 控制，不算原生 Linux 证据。原生 Linux 后续须在独占受限 OS 作业核对真实 fork/exec 继承、线程策略、节流统计、配置变化拒绝和逐阶段日志，再分别重跑原代表性负载与 BEAM。没有这些数据不移除性能报告的缺失门禁。
