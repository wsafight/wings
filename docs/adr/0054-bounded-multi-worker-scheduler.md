# ADR-0054：有界多 worker 调度与 handler 边界迁移

- 日期：2026-11-13
- 状态：已采纳（有界 M:N 集成切片；macOS 功能已验证，Linux 与完整门禁另记）
- 扩展：[ADR-0020](0020-single-worker-mailbox.md)、[ADR-0040](0040-language-request-events.md)、[ADR-0053](0053-detached-process-images.md)

## 决定与理由

在独立 ScalarWorker 之外增加 Scheduler，以固定原生 worker 池运行更多微进程。复用 ScalarTask 的强制检查点与独占镜像，不对协程、Rc 栈池或未知 native 栈局部实现 unsafe Send。

- 固定最多 16 个 worker、256 个 actor 和 256 个语言 pending request，每 actor mailbox 最多 64。共享 registry 在一个 Mutex 下维护 generation Pid、固定 mailbox、可运行镜像与唯一 owner；没有在该锁内执行 native initializer/handler、恢复协程或等待线程。扫描、过期与完成投递均有容量上界，暂以有锁固定表为正确性基线，不声称高性能。
- 新 SchedulerPid 属于 scheduler 域，不与旧单 worker Pid 混用。它不含可变 worker 地址；投递始终查同一个 registry 和代次。队列 FIFO 在 registry 锁下确定；跨发送线程不承诺调用先后全序。语言 Pid 能力按 scheduler/module 校验，跨 worker send/self/spawn/request/cancel 使用同一 registry。
- worker 原子认领可运行镜像与一条消息，发布 owner 后才出锁 attach/start。活 handler 及 GC/副作用暂停都固定由同一个 owner 恢复；其他 worker 不取得该镜像或执行权。每个 worker 对整个固定表轮转，固定 IR/runtime 时间片与独立 deadline 原子请求让出。首次执行由任一 worker 认领，后续在正常 handler 返回、detach 并归还 registry 后按 worker 编号轮转；迁移只发生于无活栈边界，旧 state/堆/账户保持不变。这是可复现的迁移正确性基线，不是负载自适应 work stealing，指定 worker 繁忙时不由空闲 worker 接管。
- worker 本地计费栈池按最大 actor 数预留，按原 process 账户绑定；多池预留成本完整保留，不假称零成本。共享控制表、mailbox、语言请求/完成信封、宿主结果表、线程栈/控制及计时线程均先预留再构造。初始化在锁外使用调用线程本地池，仍受已有总预算；并非可任意中断的初始化服务。
- 宿主 handle 可跨线程发送标量消息/请求，使用有界结果表与唯一 IngressRequest。语言私有信封另外允许 RecordInt/IntList 复制载荷，沿 ABI v12 schema/入口根导入；外部不能伪造引用。语言请求只支持现有 Int 回复和三参数完成事件，完成信封提前预留，队满保留待投递；超时/取消不撤回目标消息，迟到回复不替换终态。
- start 前额度/栈准入失败不消费队首或状态，归还镜像供后续重试；其他 actor 继续运行。开始后受管失败终止目标，失败/stop 使已有请求选择失败或既有终态。stop 对活 handler 设置取消原子标志，不把活栈移出原线程；宿主 stop 返回已请求停止，最终退出由 owner 清理。无活栈目标可直接移出 registry 清理。
- Scheduler Drop 先关闭投递、请求全部 owner 取消、唤醒 worker，再 join；所有本地 task 正常取消返回后才释放线程/池预算，最后清空 registry。worker 非正常退出也关闭共享入口并取消其余 owner，不继续让队列依赖已退出线程。外部结果 handle 可延长结果表和原账户租约，不能继续执行或投递。slot 代次耗尽退休，不重定向旧 Pid。

## 边界与重审

已有 ScalarWorker/Supervisor API 保留，不把新 scheduler 的 Pid 导入旧监督器；多 worker 监督接入仍需独立实现。没有活栈迁移、跨租户移动堆、无锁队列、热更新或分布式。固定扫描及每 worker 池预留是待测基线，必须在相同业务 BEAM 吞吐/整机内存门禁中暴露成本；本切片通过不等于 M0/MVP 完成。

## 验证要求

固定表测试验证认领互斥、代次、FIFO、跨模块/域拒绝、mask/schema、请求终态与资源回滚。真实受限 AOT 运行 actor 数大于 worker 数，验证独立状态、单步暂停不重放、跨 worker RecordInt/IntList 消息和异步请求、返回后迁移、旧 Pid 稳定及原账户根/额度。覆盖队满/三级拒绝、start 失败重试、繁忙 actor 公平推进、stop/Drop 所有暂停边界、初始化/线程启动失败、超时/取消/迟到回复、持有结果至 scheduler 销毁后的最终归零。

运行 workspace check/test/clippy/fmt、MSRV、debug/release 完整受限 AOT；两个平台分别记录原生/仿真边界，不放宽 CPU/墙钟限制，不用功能证据替代 BEAM 性能与内存结论。
