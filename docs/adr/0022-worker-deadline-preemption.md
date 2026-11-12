# ADR-0022：单 worker 的可选 deadline 抢占

- 日期：2026-11-12
- 状态：已采纳（标量 worker 计时切片）
- 补充：[单 worker](0020-single-worker-mailbox.md)、[活栈恢复](0011-scalar-stackful-resume.md)

## 决定

- `ScalarWorker::enable_timer` 可选创建一个常驻控制线程，默认构造不创建线程。线程只持有原子让出令牌、Mutex/Condvar 控制状态，不访问 actor、资源账户或协程；worker/活栈仍只在原线程使用。
- `tick_timed(slice_steps, duration)` 要求已启用 timer、非零时间片及非零且可表示的单调时钟时长。进入 tick 前发布一次 deadline，返回（含启动资源失败）后撤销。截止时只原子请求让出，由生成代码下一个强制 safepoint 挂起；不取消 handler、不补充总预算，不承诺硬实时或 p99 已达标。
- arm/disarm 和 timer 写标志在同一控制锁下串行。新 arm 清除上次标志；arm 时已到期立即设置让出标志，不等控制线程获调度。disarm 清除尚未消费的标志；timer 不把上一轮到期写入下一轮。普通 `tick` 保持步数切片，不依赖 timer。
- 没有 deadline 时线程在 Condvar 等待，不按周期空转。Drop 设置 stop、唤醒并 join，再释放控制结构及线程容量票据；挂起调用仍由既有正常取消路径清理，不能跨 ABI unwind。
- 创建前向 tenant/VM 预留 256 KiB 请求线程栈、两个 OS 页的保守保留及控制容量。分配/线程创建失败回滚；这不是 OS 线程实现、TLS、内核和 allocator 全部物理开销的精确计量。每 worker 一个线程只是当前可选验证方式，M:N 时的共享计时服务须另评估，不把它当作默认扩展方案。

## 验证与边界

有界子进程验证未启用、重复启用、零片/零时长/不可表示 deadline 的拒绝；计时标志在强制检查点让出、原栈恢复和成功提交；计时暂停不重置总预算；额度拒绝零变化；空闲 timer Drop 不等待长 deadline，Drop 挂起 actor 仍归零。debug/release 与两个首要平台分别验证。

当前直线标量程序通常在 deadline 前正常结束。极短 deadline 的行为回归可走 arm 时已到期的路径，只证明 deadline 检查和强制让出，不证明控制线程在 handler 执行中获调度的延迟。不能由此推导真实服务公平性；没有循环、GC、长 native 或 M:N 验收。请求 timeout 仍是 tick/take 的轮询终结，与执行 deadline 的原子抢占不同；没有自动 I/O reactor 或外部请求通知。
