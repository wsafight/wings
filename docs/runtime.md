# 运行时

[ADR-0081](adr/0081-accounted-default-cli.md) 让 CLI 默认标量模式与堆模式都使用计费根/子账户、计费模块和单槽零缓存池；16 MiB 三级逻辑字节上限不因模式变化。初始化/handler 控制由核心准入，错误继续写 stderr 并退出非零。JSON/标准 I/O 适配层与宿主控制对象仍未完整计费，独立工具入口保留原边界。

[ADR-0080](adr/0080-central-execution-admission.md) 将原 4352 字节控制容量统一放到 initializer/Invocation 核心预留，覆盖直接调用、owned task 及单/多 worker；活栈挂起时保留，正常返回/受管失败/正常取消清理后归还。worker 外层重复票据移除。标量 `instantiate_in_pool` 保存显式池，后续同步调用不退回默认未计费池。VM 已满且没有控制余量时拒绝；栈票据的净转移不能替代控制分配准入。旧未计费工具池及完整账本边界仍保留。

[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 增加 `ResourceAccount::vm_accounted`：两次 Arc 分配前检查根、共享准入门闩与引用计数的逻辑字节，used 从启动成本开始；存活根在业务清理后回到该基线。此根下 `tenant`/`process` 自动计费控制节点，不能借旧入口绕过；旧 `vm` 工具模型保持原零基线。CLI 堆模式与 TCP 服务使用计费根，TCP 报告启动/剩余字节相等才标记业务预留已释放；没有完成所有入口、allocator/装载器内部成本或完整资源账本。

[ADR-0078](adr/0078-request-publisher-lifetimes.md) 修正内部请求提前归还额度：ScalarWorker 宿主结果被取走、语言完成事件入队或发起方退出后，仍排队/执行的目标消息继续占请求槽和原账户 requests 票据。两端都结束才递增槽代次并归还；已消费 ID/已入队事件的能力不可取消。事件路由和消息预留可先移出/清理，目标消息完成、失败或 stop 后结束发布端。该规则与 ingress/Scheduler 宿主结果双端租约一致，具体容量及身份仍分别管理；ABI v15 和取消不撤回消息的语义不变。

[ADR-0076](adr/0076-accounted-byte-messages.md) 增加 Bytes 的计费消息副本：单条消息合计至多 65536 字节，发送方暂存先准入，发布时转至目标账户，导入时目标副本另行预留；每至多 256 字节检查预算/取消/让出。队列保留原生命周期，超时不释放仍排队的字节；部分导入参数由入口根保活，失败/stop/取消完整清理。消息 mask 和账目细节见 ADR，原语法及标量回复边界不变。

[ADR-0074](adr/0074-bounded-concurrent-recovery.md) 回归现有超时/取消只决定宿主结果、不撤回已接受消息的契约：仍存活的 publisher 保留请求额度，stop/取消竞争选择一个终态，失败保留旧提交 Bytes，监督重启不重放旧队列。用一/双 worker 真实 AOT 和固定屏障回复表测试验证，不新增消息撤回或事务。

[ADR-0073](adr/0073-reusable-initializer-stack-pool.md) 为宿主批量创建增加显式初始化池借用：只接受同 tenant 计费池，同步绑定目标 process 并转移完整栈额度，返回后归还；满池、错误账户、额度或 initializer 失败按原路径回滚。Scheduler/supervisor 不持有该池，后续 handler 与自动重启路径不变，缓存由调用者 drop/trim，不把缓存计费与 RSS 混同。

[ADR-0072](adr/0072-scheduler-yield-observation.md) 的 `SchedulerConfig::yield_samples` 默认 0，可设每 worker 1..=65536 条固定 FIFO；启动前 tenant/VM 准入失败回滚。`take_yield_sample(worker)` 一次消费副本，`yield_statistics(worker)` 返回请求/合并/消费/清除/确认和保留/取出/丢弃计数；活动查询不保证跨锁快照，停机后核对守恒。样本只含 Pid/时间，不保留业务对象；最后 handle 释放才归还诊断租约。

[ADR-0071](adr/0071-observed-atomic-yield.md) 的 `PreemptionToken::new_observed(account)` 预先计费 4096 B，固定保留首次未消费请求和最近真实让出；克隆/调用租约释放后归还。重复请求合并，disarm 清除不生成样本，步骤/effect/取消/预算返回不伪造原子让出；默认 token 无时钟/诊断锁。它不是完整事件日志或逐 actor 的 Scheduler 指标。

[ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 增加同 worker CPU/突发心跳业务回归，沿用原强制检查点、总预算和原子取消。外部心跳超时不释放实际 pending、不撤回 state 提交；受限测试收齐 CPU/心跳后核对独立 state 及最终归零，不据此声称长时公平性或完整资源账本通过。

[ADR-0069](adr/0069-streamed-open-loop-measurement.md) 的长测只延长独立测试宿主的实验时段，不放宽原 smoke 或 runtime 配额。仍最多 256 实际 pending，外部超时不撤回远端提交，最终未收齐实际结果即判实验无效；冻结副本和原始事件不等于完整资源账本或 OS 隔离。

[ADR-0068](adr/0068-bounded-open-loop-counter.md) 仅新增测试宿主的计划时刻开环：外部 100ms deadline 不取消远端业务，超时 pending 保留到实际结果，拒绝不重试。未改变 runtime 请求/状态提交契约；长时公平性、超时竞争与完整资源账本仍待验收。

[ADR-0067](adr/0067-beam-bounded-admission.md) 的 BEAM 测试 gate 对已接受尚未开始的业务消息计数，满时显式拒绝；开始只允许绑定 actor 归还，关闭后不再接收/开始。占槽与 `!` 间若发生退出，该请求仍算已接受，宿主依据 DOWN 完成 pending，不自动重试；旧 gate 不指向新实例。这是可信对照驱动，不是 VM 内置或公共 SDK。

[ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 允许 Scheduler mailbox 配置 1..256，0/257/极大值在创建资源前拒绝；同模块 spawn/监督重启保留已配置容量。满队列沿原 `mailbox_full` 显式失败，不改变 state/队列或票据；当前执行 handler 不算排队数，starting 暂保留原容量。满队列仍能 stop，排队请求选择唯一终态并释放，ScalarWorker 上限不变。

[ADR-0065](adr/0065-two-sender-counter-fifo.md) 对同发送者到同目标 FIFO 增加双发送者正确性对照，不保证跨发送者全序。每发送者独立 16 条闭环窗口，总共最多 32 请求；业务状态验证序号连续并与计数器一起提交。现有请求的锁内准入/入队仍为线性化点，测试不提供新的全局完成事件流，也不将安全加法用例称为并发故障/超时验收。

[ADR-0064](adr/0064-indexed-supervisor-slots.md) 保留监督 child 的原始 Pid 身份并添加私有槽索引；查询/停止完整核对身份，复用槽不接受旧 child，重启不改变稳定 ID。创建成功才占用空槽，创建失败保持可用；主动 stop、禁用/超限/失败重启归还槽，成功重启不归还。独立终态/清理顺序与有界 poll 不变。

[ADR-0063](adr/0063-multi-actor-byte-counter.md) 的 Scheduler `byte_state_snapshot(pid)` 只在无 owner 时返回已提交 Bytes 长度/32 B 前缀副本，非 Bytes 返回 None、旧 Pid/关闭拒绝；初始化和 handler 返回更新缓存，失败退出复制旧提交值。副本可在 actor/堆/模块销毁后保留，不持资源租约。普通 SchedulerSnapshot 不自动暴露前缀；副本可能含业务数据，只供可信宿主显式读取，不是完整导出或持久快照。

[ADR-0062](adr/0062-sparse-ready-scheduler.md) 将 actor 容量扩至 16384，并以每 worker 固定就绪 FIFO 和本地活栈 FIFO 派发，不遍历空闲 actor/执行槽。每 actor 最多一个就绪成员，消息发布与完成投递按唯一 owner 决定入队，退役先摘链再复用代次；失败启动恢复原消息头并重排队尾。每轮至多认领一个新 handler 和恢复一个活栈片，语言 pending 维护仍最多扫描 256 槽，其他容量/提交/副作用契约不变。

[ADR-0061](adr/0061-on-demand-scheduler-stacks.md) 为 Scheduler worker 采用按需栈池，最大活栈数仍由 actor 容量限制，冷栈在映射前按完整容量和 guard 三级准入；缓存命中只净转移账户。启动拒绝不弹走队头、不提交 state、不退出 actor，其他可准入 actor 继续运行；拒绝可持续至资源释放或 stop/关闭，未承诺饿死工作最终准入。正常返回才可缓存最多两个栈，超额映射释放，缓存只向 tenant 计费，trim 不碰活栈。

[ADR-0060](adr/0060-shared-scheduler-reply-table.md) 将 `SchedulerConfig.requests` 的宿主结果容量解释为全实例 Int/Bool/Unit 共用的总槽数，语言 pending 另有同大小表，不合并这两类容量；普通固定入口 ingress 不变。每次宿主 request 先按原目标 process 计费，实例表满返回 `ingress_requests_full`，队列/准入失败不留半槽或票据。超时/取消不撤回消息，caller/publisher 都结束才复用槽；退出释放业务资源，未消费结果仅延长原票据/共享控制表租约，不保留 actor 或模块。

[ADR-0059](adr/0059-byte-state-counter-equivalence.md) 为单 worker 增加固定按值 `ByteStateSnapshot`（长度/最多 32 B 前缀），不返回 handle 或保留堆/账户。`byte_state_snapshot(pid)` 在 active invocation 时拒绝，非 Bytes 返回 None；失败 ExitSnapshot 在清理前复制已提交 Bytes，退出后仍可持有副本。前缀可含业务数据，仅可信宿主显式读取，不是完整导出或持久快照。固定 Counter 测试先提交整批最多 32 个请求再 tick，按 Delivered 顺序消费；失败终结该批队尾且不重放，业务契约不变。

[ADR-0058](adr/0058-byte-i64-big-endian-access.md) 的八字节读取固定有界、不分配；替换仍沿 Bytes 原账户、精确源根、每块最多 256 字节和取消清理契约，需同时容纳旧/新对象。先验证完整范围，再预留/复制，不从负数或极端 offset 形成越界访问。ABI v14 复用原 scratch/context，未开放原 handle 消息或原生沙箱。

[ADR-0057](adr/0057-accounted-immutable-bytes.md) 将 Bytes 作为精确堆中的不可变叶节点：每个对象占一个槽，固定所有权表计入堆容量，独立 buffer 在分配前由原进程三级预留。构造/复制每块最多 256 字节且检查预算/取消，未发布 builder 在暂停时仍计费、失败/取消时释放；不可跨堆发布。预留/槽不足分别只 GC 重试一次，compiler 源根和宿主持久根保留旧对象，复制替换需要同时容纳旧/新 buffer。清扫或堆退出释放 buffer 与票据，不扫描二进制内容为引用；heap_stats 包括已发布数据，账户 used 另包含未发布数据。单次 allocator 最多 64 KiB，不提供硬实时保证；Bytes 复制消息见 ADR-0076，引用回复、字符串和完整业务值仍未完成。

[ADR-0056](adr/0056-multi-worker-supervision.md) 的 SchedulerSupervisor 通过固定子表与每实例唯一终态票据实现宿主 one-for-one 恢复。poll 一次至多消费一个退出，受管失败且滚动窗口仍准入时创建全新 state/堆和 Pid，旧请求已终结、旧消息不重放。并发失败不能互相覆盖终态；stop/关闭不重启，初始化/资源拒绝或强度超限不自动重试。host handle 的结果租约仍可延长旧账户，但票据不持有业务资源。需显式 poll，不提供语言 supervision/link/monitor。

[ADR-0055](adr/0055-multi-worker-exit-diagnostics.md) 的 SchedulerHandle::last_exit 返回跨 worker 最近失败的按值副本。记录旧 Pid、owner 编号、handler、ABI 故障和执行/预算/GC 快照；已提交引用 state 不暴露 handle，清理前账目不表示资源未归还。正常成功、主动 stop、关闭和未开始的准入失败不覆盖。关闭后仍可读取，不持有旧业务资源；固定单槽可能覆盖，不能替代可靠监督事件或故障历史。

[ADR-0054](adr/0054-bounded-multi-worker-scheduler.md) 的 Scheduler 使用固定线程与共享有界 registry，Pid 不依赖当前 worker；活 handler 始终由唯一 owner 恢复，正常返回后的镜像按 worker 编号轮转。send/request 的接受顺序由 registry 锁确定，同 sender FIFO，跨线程不承诺调用先后全序。stop 活目标只请求取消，owner 正常返回后退休代次；Drop 关闭入口、取消并 join 后才释放池。宿主结果可延长原账户租约但不能继续执行，语言完成事件提前计费且队满保留。配置边界与 start 前准入失败恢复队首见 ADR，未接入旧 Supervisor。

宿主可按 [ADR-0053](adr/0053-detached-process-images.md) 在无 ScalarExecution 借用时消费 ScalarProcess 为独占镜像，释放原池绑定并移动到另一线程。attach 只接受同 process 账户的新池，校验失败保留镜像、成功仅一次，不执行初始化；旧堆 state/代次/根/失败标志不变。镜像 Drop 直接释放所持资源，不存在挂起活栈可取消。worker 的 Pid/mailbox/request 未随之移动，原有单线程唯一执行权和活栈约束不变。

只读模块及 handler ID 可按 [ADR-0052](adr/0052-cross-thread-module-leases.md) 跨线程 clone；每个 worker 必须在所属线程独立构造，模块 VM 身份仍逐次校验。代码/计费租约到最后持有者才释放，各 actor 的状态、堆、栈池、mailbox 和执行权不共享或迁移；四线程宿主示例不改变单 worker 调度契约。

IntList 消息按 [ADR-0051](adr/0051-bounded-int-list-messages.md) 在源协程逐节点复制到固定 64 整数信封，所有列表合计超限在发布前受管拒绝。目标按参数顺序从尾重建，每个空/cons 节点前检查预算与取消，当前尾和已导入参数由入口根保活；失败与 stop/Drop 沿原调用清理，未发生跨进程 handle 共享。共享尾在不同参数独立复制，空列表也在目标分配节点。宿主/JSON/ingress 不接受内部段编码，引用回复尚未开放。

RecordInt 语言消息按 [ADR-0049](adr/0049-record-message-copies.md) 用固定信封复制字段，目标消息开始后才在自己的计费堆构造引用。导入逐项扣 runtime 步数，分配/GC 可让出；入口根最多 64 项，完成/失败/取消清空。队列满拒绝在发布前，已发布后目标堆满是目标受管失败，不回滚源副作用，也不暴露源 handle。

真实 I/O 验证示例 `tcp_counter` 只绑定临时 IPv4 loopback 端口，单适配线程、最多八连接/每连接 64 帧、固定 32 B 收发，绝对读写 deadline 不随部分传输刷新；请求超时不重试，断连不回滚 state。线程、listener 和连接逻辑预算先准入，Drop 停止并 join，详见 [ADR-0048](adr/0048-bounded-loopback-service.md)；kernel/allocator 的全部内部开销仍非精确计量。

宿主可用 `open_request_ingress` 配置固定标量回复入口；`IngressSender::request` 返回唯一结果 handle 与已接收通知状态。结果 `try_take` 或外部线程 `wait` 只终结一次；超时/取消/丢弃不撤回消息，结果槽在调用者结束且消息完成后才回收。请求预算、代次、失败/关闭与双端租约见 [ADR-0047](adr/0047-cross-thread-ingress-requests.md)；不等于语言同步 call 或完整 I/O。

宿主 `ScalarWorker::open_ingress(pid, handler, capacity, max_steps)` 需先 `enable_wait`，每 actor 仅一个 1..=64 槽的标量单向入口；可跨线程的 sender 不持 actor/代码租约。`send` 成功表示已接收，`IngressWake::WakeFailed` 也已接收、不能重试，宿主须继续 tick/有限 wait 或终止。每 tick 至多搬运一条至有空位的 mailbox，保留消息票据；stop/失败/Drop 关闭并清理，旧 sender 不指向复用进程槽。共享 ring/通知预算到最后 handle 才归还，详见 [ADR-0046](adr/0046-bounded-cross-thread-ingress.md)；不承诺跨发送者全序或 exactly-once。

代码租约与模块 VM 级准入按 [ADR-0044](adr/0044-native-module-leases.md) 实现：clone/HandlerId/实例共享一次预留，跨 VM 计费执行拒绝；最后一个活栈先正常取消返回，再交还 Library 和预算。旧可信非计费工具入口保留，不将该兼容路径称为完整三级准入。

受管 GC 按 [ADR-0043](adr/0043-chunked-runtime-gc.md) 分为清旧标记、根、传递遍历与清扫，每块最多 16 个节点/根并扣一个 runtime 工作单位。原栈暂停保留 phase/cursor，不重做；取消/预算失败正常返回，清扫中止也保留活根，下一轮先清旧标记。heap_stats 使用实时计数，无全堆诊断扫描；同 actor 收集期间无其他入口执行，不是并发 GC。

worker 的 `enable_wait` / `wait` 只在无就绪入口时进行有界 OS 等待，返回后重查 deadline/完成事件与 ingress，不执行业务。FD 的固定控制预留由 worker 和 ingress notifier 共享租约，最后 owner 销毁后归还；等待语义见 [ADR-0041](adr/0041-worker-deadline-parking.md)，租约扩展见 ADR-0046。

[ADR-0040](adr/0040-language-request-events.md) 将请求结果保留到完成事件实际入队；队满不丢结果，发起方退出清理预留而不撤回对端消息。当前仍由 tick 扫描 deadline，没有自动 OS 事件泵。

[ADR-0039](adr/0039-local-process-capabilities.md) 允许同 worker 同模块创建与投递；新实例独立账户和状态，父失败不撤回已创建子项。固定 actor 能力号不复用，退出失效，无增长能力表或隐式监督关系。

[ADR-0038](adr/0038-self-send-effects.md) 的自发送经协程按值交出、worker 同线程立即准入入队，下一片确认后继续原调用；不重入 actor，不保存挂起 context 指针。独立 CLI 无此能力，调用受管失败；退出按现有规则清理已排队消息，不作为副作用回滚。

IntList 按 [ADR-0037](adr/0037-immutable-int-lists.md) 使用同进程计费节点和不可变共享尾；内建每次只操作一个节点，空 head/tail 受管失败。持久列表根传递保活尾部，退出不保留业务引用。

[ADR-0042](adr/0042-managed-function-frames.md) 的私有源码 fn 保留真实调用帧；参数按序求值一次，精确根逐帧链接，记录引用仍归当前堆。取消/失败正常逐层返回并解链，候选不提交；同步/初始化/挂起执行使用同一计费栈池，不额外分配调用栈。

[ADR-0035](adr/0035-bounded-exit-diagnostics.md) 在清理前复制单槽最近退出诊断，保留失败 handler、结构化 ABI 故障、预算/GC 与旧标量提交；不保留旧堆/栈/账户，supervisor 重启不会覆盖。仅报告最近一次受管失败，不提供无界历史或语言 monitor。

RecordInt 条件合流按 [ADR-0033](adr/0033-record-conditional-roots.md) 在检查前发布选中引用；唯一 state 的引用回边按 [ADR-0034](adr/0034-record-state-loops.md) 分别保活 header/候选/出口，不发布未执行体局部。旧提交根独立保留至正常返回，RecordInt/IntList 消息另按 ADR-0049/0051 复制，仍无任意引用 phi。

宿主诊断按 [ADR-0032](adr/0032-safe-point-diagnostics.md) 汇总单 worker 的排队、当前入口、最近安全点预算/GC 统计和进程账户；查询不执行、收集或暴露记录 handle。挂起数据不是实时采样，完整跨线程与故障历史诊断仍待实现。

有界记录状态切片遵循 [ADR-0027](adr/0027-persistent-record-state.md)：已提交状态作为独立宿主根保活至 handler 正常返回，候选由 compiler 根保活，状态与回复校验后才替换根；失败不提前释放旧状态。堆、根及执行栈仍属于同一 process 账户。

监督带堆进程用显式 `ScalarSupervisor::spawn_with_heap`，记录槽数随子项配置保留；重建先释放旧实例，再在相同限额下创建全新计费堆并重新执行 initializer，不恢复状态或重放消息。失败移除子项且回滚预留，普通 spawn 仍无堆。见 [ADR-0028](adr/0028-supervised-record-heaps.md)。

条件表达式仅执行被选中的一臂，预算按实际路径含分支/合流标记扣减；每个已执行操作和最终提交前仍检查取消/让出。未选臂不分配、不触发错误，失败不提交状态。根计划可暂时多保活外层引用但不能扫描未执行臂的引用。见 [ADR-0029](adr/0029-scalar-conditional-expressions.md)。

标量比较从左到右求值两侧一次，比较本身计一操作，不产生溢出或隐式类型转换；失败顺序沿用操作数执行顺序，见 [ADR-0030](adr/0030-scalar-comparisons.md)。

标量 state while 在每轮重新求条件，以候选状态回边，handler 正常结束前不提交。header/条件/回边均强制检查，空循环也受取消、时间片和总预算约束；恢复不重放迭代。循环体局部不逃逸，外层临时根按循环范围保活，见 [ADR-0031](adr/0031-scalar-state-loops.md)。

[文档目录](../docs.md) · [项目概览](../README.md)

一个 VM 实例对应一个 OS 进程，内部微进程由 runtime 调度，共享该地址空间。

语言通过统一 `on` 定义消息入口。有回复类型的入口处理请求，其余处理单向消息；`-> Unit` 仍要确认回复。状态赋值只替换当前 handler 的候选状态，正常结束后提交，再投递自动回复；失败不提交候选状态，但不撤回先前已经发生的外部副作用。具体规则见[语言设计](language.md)。当前[标量 AOT 原型](architecture.md#标量-aot-原型)只验证直接入口调用、候选状态和回复，下面的完整微进程调度、资源、消息与监督契约仍待实现。

## 微进程模型

### 进程控制块（PCB）

进程的业务状态和执行状态分别保存。下面是字段草案，尚未测量结构体尺寸：

```rust
struct Process {
    pid: Pid,
    status: ProcStatus,
    state: StateStorage,
    mailbox: Mailbox,
    execution: Option<ExecutionContext>,
    relations: RelationSet,
    pending_requests: RequestTable,
    resources: ResourceAccount,
    code: CodeLease,
}
```

- `ProcStatus` 区分 Runnable、Running、Preempted、Idle、Hibernated、Updating 和 Exiting。只有一个调度线程能持有进程的执行权。
- `StateStorage` 保存运行态 arena 或冬眠 blob，二者不同时作为可写状态存在。序列化迁移期间的临时双份数据也计入预算。
- `ExecutionContext` 只在 handler 执行或被抢占挂起时存在，保存原生栈、shadow stack 和寄存器上下文。handler 返回后释放执行上下文；等待消息不占用独立原生栈。
- `Pid` 含槽位与 generation；复用槽位前递增代次，发送、monitor 和回调都校验完整标识。位宽、代次耗尽和未来节点标识在协议冻结时确定，不预先承诺固定的 64-bit 分配。

### 内存账本与分配模型

未发布标量实例按 `ScalarModule` 共享只读清单、initializer 和入口表，状态与失败标记仍独立。共享模块不是共享可变业务数据。`HandlerId` 由模块按名称预解析并绑定身份，重复调用可直接索引入口；外部名称分派只是适配层，不将此接口当作语言内消息协议或 mailbox。模块数据的共享收益必须与固定模块基线和实例增量分开测量。

百万进程的内存成本包括 PCB 和它关联的全部资源。首版在大块映射中划分逻辑独立的 arena：runtime 从共享分配池为各进程分配专属、不移动的堆块，受管代码只能访问自己进程的堆块。这种逻辑隔离不要求每进程一个 OS 映射，也不是硬件保护页隔离。

原生栈使用有容量上限的可复用栈池，每个栈槽有 guard page。编译器在使用栈空间前检查显式栈限额，超限产生受管错误；首版不做透明原生栈增长。栈池容量和被抢占进程保留的栈都纳入准入，guard page 只是漏检时的诊断防线。

未发布标量原型已实现 `StackPool::new(槽数, 最大缓存数)`：固定 256 KiB 可用栈，最多 2048 槽，真实页大小对齐且另计 guard page。池按整块地址预留，借出才激活；槽数与缓存数在分配前检查。默认每线程 64 槽、缓存 2 槽；显式池同线程共享，最后一个活栈归还前不会 unmap。成功、受管失败和正常取消返回后立即归还；挂起槽位不能被复用。超过缓存限额时重映射为匿名 `PROT_NONE` 以丢弃页，`trim` 主动清理缓存。重置失败保留槽位、累计失败数，不伪报回收；缓存可暂时超出目标但仍受总槽数限制。`stats` 报告容量、活跃数、缓存数、地址预留和保护页，不是 RSS。编译器显式栈限额仍未实现，栈池也尚未接入下述三级资源账户，不能用池容量限制替代完整准入。

```text
执行中/被抢占 = PCB + arena 已分配容量 + 原生栈 + shadow stack + 外部关联成本
普通空闲      = PCB + arena 已分配容量 + 外部关联成本
冬眠          = PCB stub + 实际 state blob + 外部关联成本
外部关联成本  = mailbox + links/monitors + timers + pending requests + 共享数据引用
```

| 统计口径 | 必须报告的项目 |
|---|---|
| 逻辑使用量 | 活对象、消息 payload、状态 blob 和关系表等有效数据 |
| 用户态分配量 | 堆块和队列容量、栈槽、对齐、碎片、空闲池，以及共享元数据摊销 |
| 虚拟地址空间 | 预留映射、栈槽和保护区，不当作 RSS |
| 驻留与物理占用 | 两平台报告 RSS；Linux 另报 PSS，macOS 另报可获得的 physical footprint、压缩内存与 swap，不混用口径 |
| 内核与外部资源 | Linux VMA、macOS VM region、可获得的页表数据、socket/FD 和内核缓冲区；独立列出，不混入用户态数据 |

M1 先测字段布局和最小状态成本，再用不同 state 大小、mailbox 长度与活跃比例测整机斜率，并与 BEAM 对照；不预设单进程的固定总字节数。百万连接的结果还必须包含连接层和内核缓冲区。冬眠只减少可释放的内存，不会让大 state 或 mailbox 消失。

栈池的三级接入另见 [ADR-0019](adr/0019-accounted-stack-pools.md)：`new_accounted` 在任何向量/mmap 前按 tenant 预留整池容量和元数据，`for_process` 限定同租户进程；借出槽位净转移到 process，完成/正常取消后转回 tenant。缓存和 guard 不漏计，`trim` 不释放容量承诺，最后一个活栈/池 owner 才 unmap 并归还额度。宿主未计费 `new`/TLS 路径仍保留，调度集成必须显式使用计费池；参数、token 与 task 头等仍待接入，不能称完整资源隔离。

### 独立记录堆与精确根原型

`RecordHeap::new` 接受 process 账户、最多 1024 个对象槽及 128 个布局，构造前预留全部存储容量与控制结构字节；`ShadowRoots::new` 另预留最多 64 个根槽。票据与存储同寿命，空闲槽不释放容量；计费是所请求的用户态存储，不是 allocator 元数据或 RSS。当前固定八字段槽会为小记录保留未使用字段，不是最终紧凑 arena 布局或内存领先证据。

`register_layout` 描述 Int/Bool/Unit/Ref 字段，重复布局复用 ID；`allocate` 校验字段数、类型、布局身份和引用后才占槽，错误不改变堆，容量满只报错不自动 GC。外部 handle 包含堆身份、槽号和代次，内部引用压成槽号/代次，读取与根发布检查 stale/跨堆 handle。记录构造后不可修改。

`RootFrame` 按作用域维护显式根，嵌套帧只操作自己的槽；帧 Drop 清除本帧根，不回收业务对象。收集只使用传入的这一张 `ShadowRoots`，调用者必须先汇总全部活跃根；其他根表和宿主保存的未发布 handle 不自动保活。`collect` 先验证根，错误时不改变堆；随后按布局标记引用并回收不可达槽，活对象不移动，回收后旧代次失效。完整工作上限受固定槽数、字段数和根数限定，不承诺停顿目标。

[ADR-0025](adr/0025-aot-record-roots.md) 增加 AOT 临时记录路径，后由 [ADR-0027](adr/0027-persistent-record-state.md) 扩展持久状态：compiler 按最后使用位置发布 Ref（包含成功出口的候选状态），堆回调在收集前遍历 C 布局 shadow frame 链并验证全部 handle。至多 64 帧、64 个 compiler 根，另有一个宿主已提交状态根和最多 64 个入口参数根；验证失败不回收。单 Int 字段分配失败时先回收一次再重试，仍无空间则受管失败；显式 Collect 立即回收。固定容量堆及 129 槽汇总根表在构造前计费，GC 中不增长。实例通过 `instantiate_with_heap` opt-in，调用时独占堆所有权移入协程，正常返回归还，取消/Drop 随执行结束清理；支持 RecordInt 与有界 IntList 按值复制消息，仍不支持引用回复或后台收集。

[ADR-0026](adr/0026-language-temporary-records.md) 将临时记录接入源码。带堆实例保存 process 绑定池，同步调用不能借用默认未计费栈；显式 start/task 的其他账户或未计费池在执行前拒绝，不终止已有实例。[ADR-0080](adr/0080-central-execution-admission.md) 将保存显式池的行为扩至标量初始化，计费调用/初始化都由核心预留控制容量。worker `spawn_with_heap` 在初始化前准入堆，失败回滚半创建 actor，默认 spawn 仍无堆；宿主消息/request 只接受标量，语言 RecordInt/IntList 消息按 ADR-0049/0051 复制，退出/stop 先取消活栈再释放堆和账目。CLI `--heap-records` 使用 1..=1024 槽、单槽计费栈池以及固定 16 MiB 的 VM/tenant/process 字节上限；未覆盖 allocator 内部开销、完整模块与所有运行时分配，不宣称完整内存隔离。

独立原语不自动接入业务根；当前 AOT 路径已覆盖 RecordInt/IntList 局部、状态、消息入口及 safepoint 根，仍缺引用回复和通用值布局，不是完整业务 GC。原语边界见 [ADR-0018](adr/0018-bounded-record-heap.md)，集成契约见 ADR-0025–0027。

### 隔离机制：语言级封闭 + 段隔离堆

微进程共享地址空间，无法隔离任意原生内存破坏。首版由 runtime 将规范内的受管业务错误转为微进程退出；编译器、runtime 和可信原生扩展出错，仍可能使整个 VM 进程终止。首版不对抗恶意本地机器码或系统级攻击。

| 执行路径 | 边界与限制 |
|---|---|
| `wings-lang` 受管代码 | 没有裸指针、共享可变引用或任意 `extern`；受编译器插桩、内存检查和资源配额约束 |
| 同进程 Rust/native 内建 | 属于可信计算基，需要审计、预算和受控接口；不能继承受管代码的故障隔离承诺 |
| 不可信原生扩展（接口待定） | 后续独立立项的 OS **子进程池 + IPC**；崩溃影响同一 worker 内的任务，不自动等同于每任务隔离 |

- `ProcPtr` 是进程内的堆块标识和偏移，不是可导出的 VM 原生内存地址。编译器和可信访问例程校验堆块归属、对象边界与整数溢出；消息中不能携带它。
- 业务算术异常、显式栈限额和本地配额超限走受管错误路径。堆不足时先尝试该进程 GC，仍不足则以 `heap_overflow` 退出。
- 要把局部资源耗尽限制在微进程内，必须在分配前完成预算准入，并预留错误处理空间。系统 OOM、allocator abort、编译器错误和 runtime 内存破坏仍会影响整个 VM 进程，外部服务管理器必须能重启 VM。
- guard page 只能捕获命中保护区的访问，拦不住跳过保护区的越界。未知 `SIGSEGV` 按 VM 进程故障处理，此时内存完整性无法保证，不能捕获后继续执行。
- 同进程只接受受控工具链生成并校验版本与来源的代码，不加载用户提交的任意 dylib。插件池交付前拒绝不可信原生代码，不回退到线程池。

子进程池还需要独立的内存/CPU 限额、凭证、文件系统和网络权限策略。OS 进程隔离了地址空间与崩溃范围，但完整的安全沙箱还需要这些限制。需要防范不可信代码时，优先选择成熟的 WASM 沙箱或经过加固的 OS 隔离。

当前静态标量编译帧准入见 [ADR-0017](adr/0017-scalar-frame-admission.md)：业务帧超过 64 KiB 在构建期拒绝，宿主可降低该值。[ADR-0024](adr/0024-scalar-stack-boundary.md) 增加 ABI v4 薄入口，在分配业务帧前检查实际剩余栈，并保留 64 KiB 慢路径空间；不足正常返回受管错误，不扣步或写输出。同步调用、initializer 与挂起调用均使用池内栈，worker 的初始化也先做进程额度准入，失败回滚 spawn。薄入口自身 prologue 与固定慢路径仍依赖可信代码的有界前提，尚不支持递归或完整动态调用深度。固定栈池容量与活栈转移的三级准入见 [ADR-0019](adr/0019-accounted-stack-pools.md)。

### 冬眠：只收缩已返回 handler 的状态

首版把等待下一条消息定义为**handler 已返回后的 Idle 状态**，不是函数内部的 `receive`。被抢占的 handler 仍有活栈，不能冬眠；Idle 已经释放原生栈，冬眠的额外收益仅来自压紧 state 和释放 arena 容量。

1. handler 返回并提交新 state，runtime 释放执行上下文。
2. mailbox 为空、无进行中的状态迁移，且空闲时长超过阈值后，尝试获取冬眠状态的独占权。
3. 在额度内把 state 编码到版本化 blob。成功后原子发布 Hibernated 状态，再归还 arena；编码失败则保留原状态，不强制冬眠。
4. 消息到达时继续进入 mailbox。若编码过程中有新消息，完成后重新检查队列并安排一次唤醒，不能丢掉通知或重复运行进程。
5. 唤醒时先预留解码预算，重建 state，再处理 mailbox。预算不足走资源失败路径，不能绕过 VM 实例总上限。

冬眠默认关闭，按进程类型配置最短空闲时间和唤醒冷却期，避免频繁收缩与重建。序列化、反序列化和临时双份内存都有成本；计时器、pending request、links/monitors 和 mailbox 在冬眠期间仍须保持有效并继续计费。

BEAM 的 [`erlang:hibernate/3`](https://www.erlang.org/doc/apps/erts/erlang.html#hibernate/3) 已支持丢弃调用栈、GC 和堆收缩。Wings 尝试通过受限 handler 模型降低状态表示和分配成本，冬眠本身不是新能力。收益需同时对照 BEAM 冬眠态和普通 Idle 态实测。

## 调度器：M:N + safepoint 轮询抢占

未发布源码先提供 [ADR-0020](adr/0020-single-worker-mailbox.md) 的宿主 `ScalarWorker`：固定槽位、同 worker generation Pid、有限单向 mailbox 与手动 `tick` 轮转。`spawn` 前计进程数/控制容量，消息入队前计条数与保守控制容量，活栈使用同租户计费池。`send` 当前只接受无回复 handler，参数按模块 ID/schema 验证，满时返回 `mailbox_full`；同一宿主发送者保持 FIFO。

每次 `tick` 至多开始/恢复一个入口的一片，挂起时保留原栈、候选不提交；成功才提交并归还消息/执行预留。受管失败返回退出事件并清理该实例和队列，其他实例可继续。开始前额度不足不丢队首；外部收到错误后可继续驱动其他 actor。`stop` 不经过普通队列，挂起时先正常取消退出，满队列也可清理。宿主可读已提交标量 state 和账目用于诊断，不能取得可变实例。

宿主异步请求按 [ADR-0021](adr/0021-bounded-requests.md) 接入固定 pending 表，回收契约由 [ADR-0078](adr/0078-request-publisher-lifetimes.md) 修正。`request` 只接受回复入口（包括 Unit），消息与 requests 额度全部满足才入队；`take_reply` 非阻塞，结果端和目标消息端都结束才回收槽位及 requests 额度。结果取走后 ID 立即不可再次消费/取消，消息仍排队/执行时继续背压。每次 tick/take/cancel 及 handler 返回后按单调时钟检查 deadline，超时、取消、回复或目标退出只选择一个终结结果。超时/取消不撤回已接受消息，迟到回复必须匹配完整 generation ID，不能覆盖终态或新代次。目标退出仍保留已完成未取走结果的计费，直到取走或 worker 销毁。

`enable_timer` 可选启动一个计费控制线程，`tick_timed` 在非零单调 deadline 到达时原子请求让出，仍由下一个强制 safepoint 挂起，不重置总预算。arm/disarm 与计时请求在同一控制锁串行，撤销后不把旧 deadline 泄漏到普通 tick；没有 deadline 时不周期轮询，Drop 唤醒并 join。该线程不持有 actor、账户或活栈，不提供迁移，容量及验证限制见 [ADR-0022](adr/0022-worker-deadline-preemption.md)。默认 worker 不创建线程，当前每 worker 一个的形式不作为 M:N 的既定方案。

该宿主切片后来由 ADR-0038/0039/0040 接入语言自发送、同模块创建/发送和整数异步请求；deadline 由 tick、查询或显式 wait 推进，无自动 I/O reactor。worker 默认不带堆，spawn_with_heap 显式配置计费堆；不是完整 M0/M1，也不是下述 M:N 实现。参数/token/control 沿用 ADR-0076 的保守 4352 字节容量，由 ADR-0080 在执行核心统一准入，模块另按 ADR-0044 预留；不含系统 allocator/动态装载器内部元数据，完整内存账本仍待补齐。

### 分层

- 单调度线程先验证语义，再扩展为 M:N。本地队列优先、跨线程工作窃取；线程数遵守 VM 进程的 CPU 配额，不把物理核数当作始终可用的计算量。
- handler 可以在编译器插入的 safepoint 被切走，保留执行上下文后继续运行。进程迁移必须转交唯一执行权、更新 worker 绑定，并保证 runtime 代码不依赖原线程 TLS 或跨 safepoint 持锁。
- native 内建不能阻塞调度线程。网络和文件等外部操作通过受限异步接口完成后投递事件，阻塞原生库只能放入有容量限制的工作池；工作池不提供原生故障隔离。

### 抢占协议

未发布标量源码区分总预算与时间片：每条 IR 指令、提交与受管 GC 工作块前先检查共享取消标志、总预算，再检查让出标志/时间片。GC 的每块消耗一步，空位扫描分块只检查不额外扣步。`CancellationToken` 可跨线程克隆和取消，但不能重置；`PreemptionToken` 可跨线程请求让出，慢路径在暂停前清除已观察的请求，暂停后的新请求仍在后续检查触发。重复让出请求可以合并，不是事件计数器。取消和总预算耗尽终止实例且不写输出；时间片耗尽只暂停，不补充总预算、不重放 handler。慢路径返回后重新检查取消，保证挂起期间的取消生效。

`start_controlled` 返回原线程使用的独占执行 handle，`resume` 接受非零时间片，结果是挂起或完成；完成后再次恢复返回错误。挂起状态有活栈，不能处理同实例其他消息或冬眠。Drop 未开始的 handle 也将实例标为失败；Drop 挂起 handle 则用内部取消恢复，沿生成代码正常返回路径收尾，随后释放栈，不 unwind。输入校验或栈分配失败发生在创建执行前，不终止库实例。

`ScalarTask` 提供 owned 执行槽：按值拥有实例及至多一个未完成调用，`start` 使用模块绑定的 `HandlerId`、显式栈池和控制令牌，忙时拒绝；槽位可在同线程内移动但不能 Send/Sync，只是后续进程表的所有权接口，不是调度器，见 [ADR-0015](adr/0015-owned-scalar-tasks.md)。

同步 `invoke_controlled` 与 CLI 使用同一池内执行路径，以最大步数时间片运行至正常返回；CLI 的 `--max-steps` 设置每次 handler 的总上限，初始化独立使用默认 1,000,000 步。宿主 worker 与可选原子计时让出另有有限切片，尚不等于下面的 M:N 调度。ABI、固定栈成本与提交边界见[架构说明](architecture.md#标量-aot-原型)。

首版使用**计时线程设置原子标志 + 受管代码轮询**，不依赖信号。下面是可编译的检查逻辑，实际检查点由后端生成：

```rust
use std::sync::atomic::{AtomicBool, Ordering};

pub fn preemption_requested(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Acquire)
}
```

1. 每个 worker 的控制块在调度线程存活期间地址稳定。调度器切入进程前初始化时间片和标志，独立计时线程在预算到期时用原子操作请求让出；迟到请求至多导致下一时间片提前结束，不能引用已释放的 PCB。
2. 编译器在回边、递归与其他调用点、分配点，以及过长直线代码内插入检查。检查视为不可删除、不可跨长循环外提的可观察操作，优化完成后验证控制流覆盖。
3. 检查点前把活跃 GC 根发布到 shadow stack。慢路径使用后端已知的调用约定，正确处理所有 live values，包括 caller-saved 和浮点/SIMD 寄存器；不能从任意指令直接跳到只保存 callee-saved 的切换桩。
4. 进入切换桩时已满足受管调用约定，再保存原生栈指针和必要机器上下文。恢复时重新绑定当前 worker，并恢复寄存器与根链。上下文切换耗时单独测量，不预先承诺 `<100ns`。
5. GC、序列化、消息复制、模式匹配和退出清理也必须分块让出或有明确输入上限。初版完整 GC 只在限定小堆测试范围内给出延迟目标，不能据此推广到任意堆大小。

Linux 通过 `epoll` + `eventfd` 管理 I/O 等待与 worker 唤醒，macOS 使用 `kqueue` 与经验证的 `EVFILT_USER` 等通知路径。通知只能唤醒正在等待的 worker，不能中断计算线程。两个平台都由计时线程设置原子标志，受管代码在 safepoint 让出。后续实测确有需要时，再评估信号方案，并单独设计目标线程、信号屏蔽、备用信号栈，以及与 VM 其他信号处理逻辑的共存协议。这不是 Go 式异步抢占。

延迟测试限定在受支持的受管代码、有限输入、native 内建预算和既定机器配置下，测量请求发出到进程让出的 p50/p99/p999 与观测最大值。结果包含计时线程和 OS 调度抖动，普通 Linux 与 macOS 都没有无条件的墙钟时间硬上界。同时报告超额执行的受管工作量，区分 runtime 公平性与 OS 调度延迟。

### 空闲 worker 的 OS 等待原语

未发布源码的 `WorkerParker::new` 建立独占等待端，`notifier` 返回可克隆、Send/Sync 的 `WorkerNotifier`。`wait(Duration)` 返回通知提示或超时，零超时轮询；等待端可移交线程但不能共享并发等待。Linux eventfd 计数与 macOS 用户事件都可合并，`true` 不对应某个消息或消息数。

集成时先发布队列内容，再 `notify`；等待端先检查队列，无工作才等待，每次返回都重查队列和终止标志。提前通知保留提示，不用通知次数维护业务计数。描述符由共享所有权维持，任一 handle 先释放不会使另一个使用已关闭或复用的 FD。EINTR 后只等待原截止时间的剩余部分，无法表示的超时明确拒绝。底层协议见 [ADR-0016](adr/0016-worker-wakeup.md)；worker/通知逻辑预算由 ADR-0041/0046 接入，可选计时线程见 ADR-0022，有限真实 I/O 见 ADR-0048，仍不构成完整 reactor 或物理内存隔离。

### 对比

| 项目 | BEAM reduction | Tokio 协作式 | Wings safepoint 轮询 |
|---|---|---|---|
| 公平性来源 | runtime 的 reduction 预算，具体操作和 native 路径需分别分析 | 任务主动让出；部分 runtime 资源提供协作预算 | 编译器强制检查 + runtime 长操作预算 |
| 正常路径成本 | 有预算检查与记账成本 | 有任务调度和相关资源预算成本 | 原子读取、分支、根维护及慢路径成本，待测 |
| 纯计算死循环 | 受管代码可调度；不能概括任意 NIF | 不调用让出机制的循环可能占住 worker | 受支持代码的回边必须插桩，M2 验证 |

## 消息、资源计费与监督

### 资源准入

未发布源码在 [ADR-0014](adr/0014-resource-reservations.md) 的三级资源原语上，按 [ADR-0045](adr/0045-concurrent-resource-admission.md) 接入跨线程串行提交；尚未覆盖所有运行时分配，不代表完整资源隔离：

- `ResourceAccount` 按 VM → tenant → process 创建，分别配置 `bytes`、`processes`、`messages`、`requests` 四维上限。`tenant_accounted`/`process_accounted` 在 Arc 节点分配前向父路径预留节点及强弱引用计数的逻辑存储，不占子业务额度，不预留进程数；最后一个账户/票据/入口/结果所有者释放才归还。worker 一律创建计费进程账户，见 [ADR-0050](adr/0050-account-node-admission.md)。[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 的 `vm_accounted` 先检查根/门闩启动成本，并强制此树下旧子入口也计费；CLI 堆模式和 TCP 采用该根，清理回到存活根基线。旧 `vm` 及其旧子构造器仍供零基线工具模型，不满足完整 VM 资源门禁。
- `reserve` 先检查当前账户及全部祖先的额度与整数溢出，全部成功才记账；失败各级用量不变。不可克隆的 `Reservation` 在 Drop 时归还当前所有者及祖先的额度，调用者必须让票据与真实资源同寿命。
- `transfer` 只接受同一 VM 内的目标，共同祖先不重复计费；先验证目标独有路径，成功后才转移用量和票据所有权。失败保留原所有权和全部账目；跨 VM 转移拒绝，零预留合法。
- 账户使用 `Arc`，同 VM 的 reserve/transfer/Drop/used 共用一个门闩；四维计数只在门闩内读写，used 是单账户一致快照，不是跨多次查询的整体快照。锁内不分配、等待业务或调用协程；其他 VM 不共锁。账户与唯一票据可跨线程，不代表 actor/活栈可以迁移。堆、根表、worker 活栈/mailbox/请求表、计费子账户、计费根/门闩及 opt-in 模块预算已接入；旧构造路径、执行入口统一及 allocator/动态装载器内部开销仍未完整覆盖。账面字节数不是 RSS 或 OS 内存隔离。

单个 arena 的堆上限拦不住 mailbox 洪泛和无限 spawn。所有可增长资源在分配前都要经过**微进程、租户、VM 实例**三级额度检查。跨级预留失败时回滚，资源所有权转移时连同已预留额度一起转移，中间缓冲区也必须计费。

| 资源 | 计费与超限处理 |
|---|---|
| heap、栈、shadow stack | 按已分配容量计费；堆先尝试本地 GC，仍不足返回受管资源错误 |
| mailbox 和消息编解码 | 同时限制条数、字节数、单消息大小和解码结构深度；发送前预留传输与目标展开预算 |
| 微进程数量 | spawn 前预留 PCB 和最低执行资源，超限返回 `resource_exhausted`，不能无限排队 |
| links、monitors、timers、pending requests | 注册前计费；预留终结通知记录，失败不产生半注册关系 |
| 共享只读 blob | VM 实例的物理分配记一次；持有者按引用元数据和逻辑持有字节记额度，避免转发后逃逸配额 |
| FD、外部 I/O、原生工作池 | 限制句柄数、并发、缓冲区和等待队列；取消与退出触发有预算的清理 |

VM 实例总上限之外，为控制记录、退出清理和错误报告保留余量。这部分空间无法保证系统 OOM 时仍能恢复；不可恢复的 VM 进程故障交给外部服务管理器处理。

### 投递与背压

1. `send` 先按 schema 计算或有界估算编码大小和接收展开成本，执行三级准入后才分配 envelope。编码期间的临时缓冲也计费；失败释放预留额度。
2. 发送方只向接收方 inbox 发布完整、不可变的 envelope，不修改接收方 arena。接收方持有唯一执行权后才解码；GC、退出和冬眠因此不用与外部堆写并发。
3. 发布前再次校验完整 Pid 和接收状态，定义与进程退出的线性化顺序。成功只表示消息已被接收队列接受，不保证已处理；已接受后接收方退出，也不提供持久投递或 exactly-once。
4. `send` 遇到额度不足立即返回 `mailbox_full` 或 `resource_exhausted`，**不隐式阻塞发送方**。需要背压时使用显式 credit/重试协议，等待不占住 worker，也不在多个 actor 间形成隐式环形锁等待。
5. 同一发送者到同一接收者的普通消息保持发送顺序，不承诺不同发送者间的全局顺序。首版按消息标签分派，不提供任意 selective receive。

起步使用带版本的 schema 和固定的受控 codec，可以评估 [postcard](https://docs.rs/postcard/)。反序列化库不能代替资源校验，还需限制字节数、元素数量和结构深度。大消息复制与解码要分块或限制大小；`ImmutableRef` 的零拷贝只读共享在计费和生命周期测试通过后再开放。

### 退出信号与监督

未发布宿主库按 [ADR-0023](adr/0023-bounded-host-supervision.md) 提供 `ScalarSupervisor`：独占单 worker，按 `ChildId` 跟踪子项，失败后可用同一模块重新初始化，产生新 Pid。旧队列丢弃、旧请求正常终结，不重放、不保存失败状态；其他子项不受影响。`OnFailure` 使用固定历史限制滚动窗口内的成功重启数，业务成功不清零；强度耗尽或重建失败即停止跟踪并返回原退出原因及重启状态。主动 stop/Drop 不重启，满队列或挂起也可正常取消清理。子项表和历史先向 tenant/VM 准入，子进程资源仍用原三级账户。

此切片没有语言级监督、link/monitor、退避或持久化恢复；不承诺 OTP 兼容，也不把宿主恢复演示称为完整 M4。以下关系表、广播和清理预算仍需分别落地。

- **信号不等于普通消息**：终止、取消和监督通知使用预留控制记录与调度优先级，不能被已满的业务 mailbox 挡住。注册关系时预留终结通知空间，终结事件不静默丢弃；大规模 fan-out 分批处理并计费。
- **先写语义测试再称兼容**：区分单向 monitor 与双向 link，定义 `trap_exit`、普通退出与不可捕获终止。monitor 的 `DOWN`、demonitor、请求超时和退出竞争必须只完成一次。
- **对照 OTP 的范围有限**：M4 对 normal/异常退出、`kill`、link 传播、monitor 注册/解除竞争，以及 `one_for_one`、`rest_for_one`、`one_for_all` 监督重启策略做 oracle 测试。系统信号顺序、控制优先级和有限 mailbox 与 OTP 的差异单独列出，不承诺全部语义兼容。
- **清理不执行任意用户析构**：GC 管理对象可批量回收，但共享 blob 引用、关系表、消息队列、FD 和计时器仍需逐项处理。先从调度与寻址中移除进程，再由有预算的清理任务释放资源；被清理任务持有的内存和句柄在释放前继续计费。
- **不承诺死亡 O(1)**：回收成本随堆块、关系和资源数量变化。分别测量“不可再运行”“监督者收到通知”和“资源释放完成”，重启还受重启强度、退避和额度限制。

## 热更新

首版静态加载，热更新放在 M5。进程停在 safepoint 时仍保留旧执行帧，因此后续只在 **handler 返回后的 Idle 边界**迁移业务状态，不重写活跃返回地址。

1. 校验新产物的来源、目标平台、受管 ABI 版本、schema 版本和功能位，再加载新代码与元数据；加载失败不改变旧进程。
2. 为目标进程设置 Updating 请求，等当前 handler 返回。死循环或超时的 handler 不能靠任意点暂停完成迁移，更新操作应失败或按显式策略终止并重建该进程。
3. 暂停该进程的新 handler 分派；mailbox 仍接收经过额度准入的消息，满时返回错误。因此升级期间仍有队列上限，调用方可能收到发送失败。
4. 在预算内编码旧 state，由新版本迁移函数生成临时新 state。迁移函数禁止外部副作用，并有执行预算；旧状态保留至提交，迁移失败或超限则丢弃临时状态，继续旧版本。
5. 校验积压消息、pending requests、timer payload 和共享数据的 schema 兼容性；新版本必须仍能解读它们，或通过显式、可回滚的迁移处理。只迁移 state 不足以保证升级可用。
6. 原子提交新 state 与代码入口，再恢复分派。这是每进程提交，不承诺跨进程集群事务；部分进程迁移失败时允许新旧版本并存，并要求协议兼容。

**代码存活由 `CodeLease` 管理**：VTable、运行/被抢占的栈帧、shadow stack 元数据、回调、进行中的迁移和诊断读取都持有代码版本引用。只有全部引用归零且通过调度器静止期确认，才具备回收条件。M5 默认保留旧模块直到 VM 实例重启，限制保留版本数；达到上限改为滚动重启，`dlclose` 另设验收门，不默认启用。

**状态编码和函数 ABI 是两份契约**。动态加载边界采用版本化 C-compatible 入口表，规定表尺寸、缓冲区指针/长度、分配释放方、错误码和禁止跨边界 unwind；不传 Rust `String`、`Vec`、trait object 或未固定布局的结构体。首版要求 runtime 与产物由同一锁定工具链构建；版本校验不通过就拒绝加载，不能用 state 序列化代替调用约定校验。

BEAM 的[代码替换](https://www.erlang.org/doc/system/code_loading.html)允许旧代码继续执行，通过全限定调用进入当前版本，不要求普遍改写栈上返回地址。Wings 选择在 Idle 边界迁移，是为了缩小首版实现范围；AOT 本身并没有限定必须暂停换码。

## 相关文档

- [语言设计](language.md)规定 handler 返回、异步请求和副作用的时机。
- [架构设计](architecture.md)说明编译器、受管 ABI、GC 与平台范围。
- [评测与验收](evaluation.md)覆盖资源守恒、调度公平性和故障注入。
- [风险与里程碑](roadmap.md)区分首版范围和后续可选功能。
- [激进优化](optimizations.md)说明紧凑布局、消息快路径、资源预留和调度优化的实验条件。
