# ADR-0134：可选 worker 线程 CPU 区间

- 日期：2026-11-18
- 状态：已采纳（诊断切片；实际验证另记）
- 延续：[ADR-0102](0102-opt-in-worker-lock-statistics.md)、[ADR-0133](0133-open-loop-worker-lock-diagnostics.md)

## 原因与决定

0133 的两对同业务长测已经核对锁累计，但持锁/等待墙钟包含 OS 停顿，不能据此定位 CPU 开销。保留原业务和开关，在 `SchedulerConfig.lock_statistics=true` 时，为每 worker 的既有计费观测存储增加一个原子状态，并在 worker 函数入口和退出清理后读取两次本线程 CPU 时钟。默认路径不读取时钟，不增加每次认领/恢复的 CPU 时钟调用。

macOS/Linux 使用 `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`；区间只累计该 worker 线程的用户态和内核态 CPU，包含 worker 函数内的初始化、执行、统计和清理，不含线程创建前启动、OS 线程退出、独立 deadline timer、业务发生器或其他线程。纳秒是返回单位，不承诺纳秒精度或采样分布。它不是采样期 CPU、整进程 CPU、CPU 硬配额或锁内 CPU。线程时钟范围可核对 [Linux 手册](https://man7.org/linux/man-pages/man3/clock_gettime.3.html)；本机 macOS 的接口可用性由 SDK 声明、原生定向测试和实际长测分别核对，不以未取得的在线源码作证据。

区间以两个读时钟调用为界：CPU guard 先于 worker 局部变量创建，晚于它们和退出 guard 清理；函数入参最终释放及线程封套不在区间内，不宣称覆盖整个 OS 线程寿命。

查询结果明确区分 Pending、Unavailable 和 Nanoseconds；只有退出时才能发布终态。原生错误、非法/溢出时间和倒退全部记 Unavailable，不能伪造 0，也不改变业务结果或重试。使用单个 acquire/release 原子发布，读取中间状态不能组合出伪终态；本地计时 guard 不允许跨线程移动。最后 handle 保留现有统计及原账户租约，新增存储随同观测区在分配前准入，默认不分配。

独立 `--observe-locks` 长测额外输出版本 1 的 `worker_cpu_statistics`，原锁统计和 v6 事件不变，明确 worker 函数范围及 join 状态。冻结驱动只接受两个完整 worker 的实际可用结果；Pending/Unavailable 保留为失效实验，不把缺样本写成零。旧锁诊断报告仍可按原格式重放，新 CPU 数据不能回填旧运行。

## 验证与回退

验证原生时钟、边界/失败状态发布、默认关闭、join 后跨线程查询和末 handle 退账；在真实一/双 worker AOT 核对正常状态/失败/取消仍一致。独立长测仍用原 80000/s、30s/60s、两对反序和原限制，比较扰动且保留所有迟发/拒绝/超时；不当作五对性能门禁。MSRV、两平台交叉检查和本机执行分别记录，不能用交叉检查替代原生 Linux。

ABI v22、4352 B 控制预留、执行/取消/让出、容量和资源上限不变；删除 CPU 观测即可回退，不恢复此前退化的通知/空表/迁移候选。完整资源账本、启动根因、公平/过载/服务 I/O、原生双平台以及 BEAM 吞吐/整机内存双门禁仍独立验收。
