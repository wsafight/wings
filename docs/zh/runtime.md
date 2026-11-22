# 运行时

[English](../runtime.md) | **简体中文** · [返回概览](../../README.zh-CN.md) · [架构设计](architecture.md) · [语言设计](language.md) · [评测与验收](evaluation.md)

`wings-vm` 负责原生模块装载、协程栈管理、进程内精确 GC、能力寻址、多 worker 调度、定时器与监控、三级资源准入及外部服务适配。

## 微进程与能力寻址

- **轻量空闲态**：Actor 仅在执行 handler 或在异步 Effect 检查点短暂让出时借用原生协程栈；handler 返回后立即归还执行栈，空闲等待消息期间不占用独立原生栈。
- **O(1) `slot + generation` 寻址**：`WorkerPid` 与 `SchedulerPid` 均编码槽位下标与递增代次（`generation`）。进程终止释放槽位后代次递增，旧 `pid` 的投递、请求或监控校验自动以过期身份拒绝，避免 ABA 误投递与全表线性扫描。
- **唯一执行权**：任一时刻至多一个调度线程持有某 Actor 的执行权；跨消息与跨请求的值传递均在目标堆中深拷贝重建独立副本，微进程之间不共享可变堆指针。

## 协程栈池与精确分块 GC

- **有界可复用栈池（`StackPool` / `WorkerFibers`）**：
  - 每个槽位具备固定可用原生栈（256 KiB）与硬件保护页（guard page），按租户预留总容量并在借出执行时净转移至目标进程账户。
  - 正常返回、受管错误或取消均正常回收协程帧供同 worker 后续调用直接复用，避免每次消息触发 `mmap`/`mprotect` 系统调用；超出缓存上限的空闲栈重映射为 `PROT_NONE` 归还物理页。
- **直接栈快路径与活栈挂起**：无异步让出风险的纯同步短路径可直接执行，需要跨检查点抢占/Effect 挂起的调用运行在池化协程栈上，两者共享相同的步数预算、栈界检查与状态提交语义。
- **进程内分块 GC（`aot_heap`）**：非移动精确 mark-sweep，每处理 16 个根或堆节点轮询步数预算、取消令牌与调度片让出请求；字节缓冲池支持同进程回收复用以降低系统分配器压力。

## 无锁快速邮箱与 M:N 调度器

- **单发送者 FIFO 与显式背压**：每个 Actor 拥有固定容量邮箱，满槽立即返回显式 `mailbox_full` 错误，不静默丢消息，也不无限扩张内存。
- **无锁投递与批量消费快路径**：`Scheduler` 为活跃槽位维护代次绑定的快速邮箱通道，常见跨线程投递与同 Actor 连续积压消息批处理（有界 batch）无需反复争抢全局调度锁。
- **抢占式公平调度**：
  - 编译代码在分支/循环/调用点扣减 `slice_remaining` 步数片，同时后台计时线程按时间片（quantum）原子置位 `yield_request`。
  - 长计算或死循环 handler 到达安全点后自动挂起活栈并排回就绪队列尾部，同 worker 上的其他健康 Actor 继续获得推进机会。
- **平台唤醒与定时**：空闲 worker 通过平台原生事件机制（macOS `kqueue`、Linux `epoll` + `eventfd`）阻塞等待新消息或最早到期时间（请求超时与 `send_after` 定时器），避免空转轮询。

## 故障隔离、监控与有界监督

- **受管故障隔离**：算术溢出、除零、切片越界、步数耗尽、堆配额耗尽或栈帧超限均捕获为 `ExecutionFault`，回滚本次未提交的候选状态，记录 `last_exit` 诊断快照，不影响同 VM 其他 Actor。
- **语言级 `monitor` 与宿主级 `Supervisor`**：
  - Actor 可通过 `monitor(child_pid, callback)` 订阅目标退出事件，在语言层编写有状态的按需重建逻辑（见 [`examples/session-service.wings`](../../examples/session-service.wings)）。
  - 宿主 API 同时提供 `ScalarSupervisor` 与 `SchedulerSupervisor`，支持带滚动时间窗重启强度上限的 `one-for-one` 自动重启。

## 三级资源准入与外部服务入口

- **三级四维账本（`Resources`）**：围绕 `bytes`、`processes`、`messages`、`requests` 四个维度建立 `VM -> Tenant -> Process` 树形账户。容量预留在分配前原子扣减，任一层级超限立即回滚，跨层级转移只更新非共享祖先路径，句柄释放（`Drop`）自动退账。
- **运行入口**：
  - `wings-vm run --trusted <artifact-dir>`：单 Actor 原生标准输入/输出 JSON Lines 执行器。
  - `wings-vm serve --trusted <dir> [--workers N] [--tcp ADDR | --http ADDR]`：支持单模块或多 Actor 应用包（`bundle.json`），内置有界连接与请求体限制，支持结构化 JSON 参数自动展开为具名 `struct` / `str` / `bytes` / `[int]` 消息并流式序列化具名记录回复。
