# 架构设计

[ADR-0131](adr/0131-equivalent-beam-cancellation-recovery.md) 在独立 OTP 对照端使用持续有限请求适配器和按 child 的监督分支，验证与 Wings 相同的取消/恢复业务。所有适配/监督进程计入 BEAM 作业；没有引入 VM 运行依赖或改变受管 ABI。

[ADR-0124](adr/0124-bounded-scheduler-slice-observation.md) 在默认关闭的诊断模式记录每次 Scheduler 恢复调用的墙钟区间；它与原子请求到切回的观测分开，不改变受管 ABI、执行预算或调度顺序。时间边界和未覆盖范围见 [runtime](runtime.md) 与[验证记录](validation-scheduler-slices-2026-11-18.md)。

[ADR-0123](adr/0123-status-only-resource-rejection.md) 将资源准入事务的内部失败表示为 ResourceLevel，公开接口继续包装原文本；记录回复直接映射为受管状态码。门闩、逐级检查、回滚与票据共享原实现，不改变 ABI/执行预留或增加第二套准入算法。

[ADR-0122](adr/0122-bounded-handler-lookup-diagnostics.md) 将产物既有名称校验公开供 VM 查询与 CLI 解码复用，避免宿主未知名在错误构造时按外部长度分配。合法名称集合和原清单/ABI 不变；查询前固定来源上限不代替完整错误存储计费。

[ADR-0121](adr/0121-persistent-cancellation-recovery.md) 在原 AOT 恢复 fixture 中保留 Scheduler/supervisor 并逐轮复用固定槽，观测超时/取消、stop 竞争、全新重启和强度上限；默认短回归与显式长测分离。没有增加执行协议、ABI、容量或重试语义，账目推导缓存不替代完整内存普查。

[ADR-0120](adr/0120-persistent-concurrent-failure-soak.md) 的 v10 诊断在同一 VM/模块/Scheduler/supervisor 内重复失败生命周期，共用 v9 业务核心及 oracle，按完整 Pid/child 代次拒绝旧入口。每轮清理后仅允许原两 worker 各最多两个栈缓存，退出整个环境再核对根基线；不改变执行 ABI、调度、计费或监督契约。

[ADR-0118](adr/0118-native-sync-storage-admission.md) 在 macOS 的根启动和共享控制公式中包含标准 Mutex/Condvar 的原生后备存储。内部构造在已有准入之后、共享发布之前完成惰性初始化，避免首次共享使用或并发初始化额外分配；保留标准库实现。Linux 内联 futex 不加后备字节，ABI v21、manifest v2 与原执行控制预留不变。

[ADR-0117](adr/0117-streamed-cli-record-replies.md) 为 CLI 增加具名记录回复的 JSON 字段对象输出；仅遍历 `RecordReply` 的独立 schema/叶值，固定帧数组沿原 adapter 控制预留，不分配 JSON 树或完整输出副本，不改变受管 ABI、manifest、原业务载荷或 JSON 参数范围。

[ADR-0116](adr/0116-bounded-named-record-replies.md) 允许具名记录作为 AOT 回复并把名义 schema 副本绑定到独立 `RecordReply` 租约；leaf payload 限于 64 节点、64 个列表元素和合计 65536 B。请求完成值用清单保存的名义类型核对并在接收方独立重建，manifest format v2，ABI v21、ABI 布局与 4352 B 执行预留不变；不开放记录消息原引用或 JSON 对象参数。

[ADR-0115](adr/0115-typed-cross-thread-ingress.md) 将固定跨线程 ingress 的参数扩至既有类型化叶值复制。sender 只持固定 schema、ring、账户/通知/结果租约，不含代码/actor owner；同线程宿主和 ingress 共用类型及合计限额检查，目标沿既有受管导入。ABI/4352 B 不变，不开放跨堆原 handle 或通用 reactor。

[ADR-0114](adr/0114-scalar-process-account-binding.md) 使所有从 process 计费池初始化的实例保存原账户身份，纯标量不再只检查模块 VM。调用在控制/栈准入和 token 创建前校验；可发送的脱栈镜像搬运账户租约而非栈池，attach 校验后才消费镜像。同账户跨线程重绑不重跑 initializer，ABI v21 与 4352 B 控制上界不变。

[ADR-0111](adr/0111-bounded-string-content-equality.md) 的源码 ABI v21 增加 String 内容相等堆操作和专用 Bool IR，不让通用整数 Compare 接受引用；codegen 发布两个最后使用输入的精确根，callback 分块比较并可让出。context 88 B、EffectRequest 80 B/4352 B 控制预留不变，旧产物重编译。

[ADR-0109](adr/0109-accounted-wakeup-endpoint-leases.md) 将 OS 唤醒 FD 和等待票据合并为单个 Arc 端点；计费构造先准入再创建 kqueue/eventfd/epoll，worker 与 ingress 只共享端点。Linux poller 先关闭，最后 notifier 关闭端点后才归还原 4096 B，不改 ABI v20、执行预留或原时限，也不把逻辑控制容量等同 kernel/RSS。

[ADR-0108](adr/0108-bounded-named-record-values.md) 增加名义 `RecordRef(index)`、产物 schema 和构造/字段读取 IR，源码 ABI v20，旧产物须重编译。IR 与清单独立验证有界无环类型图，实例按 schema 创建计费的独立布局；业务对象复用原槽和精确引用扫描，context/EffectRequest 布局与 4352 B 控制预留不变。[ADR-0113](adr/0113-bounded-named-record-message-expansion.md) 允许源码记录消息参数有界展开为既有叶值，目标薄入口按 schema 重建；不传原 handle、不共享业务堆。IR/清单外部参数仍只接受叶类型，记录回复和名义完成类型由 ADR-0116 扩展，当前 ABI v22。

[ADR-0107](adr/0107-accounted-control-token-leases.md) 将可共享取消/让出原子的独立生命周期绑定资源票据，新增显式计费构造及常驻控制接入；Default 工具路径和既有调用控制边界保留。ABI v19、强制检查、4352 B 执行预留及原资源上限不变，仍非完整 allocator/装载账本或 MVP 验收。

[ADR-0106](adr/0106-accounted-string-replies.md) 开放 String 回复/匹配完成入口，源码 ABI v19，布局与 4352 B 控制预留不变。出口引用与候选 state 同时保活，分块导出并验证 UTF-8 后构造独立结果类型，结果只保留计费存储租约，目标完成参数另行复制为受管 String；不能用 BytesReply 冒充文字。

[ADR-0105](adr/0105-accounted-string-messages.md) 将源码 ABI 升至 v18：EffectRequest 增加独立 string_mask、长度 80 B，context 88 B/控制预留 4352 B 不变。String/Bytes 参数共用合计 64 KiB buffer，目标完整 schema 检查类型后分块独立导入；当时 String 回复和完成值仍拒绝，后由 ADR-0106 扩展；消息所有权与账目见决策。

[ADR-0104](adr/0104-bounded-string-literals.md) 将源码 ABI 升至 v17，新增 HEAP_STRING_LITERAL，context/EffectRequest 布局不变。编译器将字面量编码放入原生只读数据段，传固定 scratch 的地址/长度，runtime 分块复制为独立计费 String；原 Invocation/模块租约使常量在活栈暂停期间持续有效。IR 独立检查单值 64 KiB 和合计 1 MiB，不生成大栈数组或逐字节 IR。

[ADR-0103](adr/0103-accounted-utf8-string-values.md) 将未发布源码 ABI 升至 v16：新增 `StringRef`、四项字符串 heap operation 与 InvalidString 状态，context 88 字节/EffectRequest 72 字节布局不变，旧产物须重编译。String 使用独立叶节点标签与不可变计费 buffer，不与 Bytes 共用类型或裸 handle；根发布、跨帧/候选状态保活、GC 和 handler 返回后迁移沿用既有模型。清单/IR/语义检查三层拒绝字符串消息与回复；本轮仅开放本地、state、非递归 fn 和分块内建，不增加控制预算、资源额度或待审语法变更。

[ADR-0102](adr/0102-opt-in-worker-lock-statistics.md) 增加显式启用的 worker 四类共享锁区间统计，固定存储先准入、释放原锁后原子发布；默认关闭，不改执行 ABI 或锁的获取顺序。统计覆盖与一致性边界见决策，不是完整 profiler 或 CPU 时间。

[ADR-0101](adr/0101-empty-language-request-maintenance.md) 的空语言请求表维护快路径在 hot-queue 五对中未建立稳定收益，已回退计数与提前返回；当前沿原 Scheduler 维护扫描、双端回收和宿主结果表，不改 ABI/预算/容量。

[ADR-0100](adr/0100-coalesced-deadline-timer-notifications.md) 的计时通知合并实验已因 hot-queue 五对无稳定收益回退。当前仍在原锁内 arm/disarm 并无条件通知，stop 唤醒/join；不采用候选的实际等待状态字段，不改 quantum、safepoint 或 ABI。

[ADR-0099](adr/0099-unready-result-poll-fast-path.md) 为宿主结果表预付费固定每槽原子就绪标志。未就绪且未到原请求期限的 `try_take` 不锁全表，其余情况仍在原锁内确定和消费唯一终态；不在锁外读取结果，不改 ABI 或发布者生命周期。

[ADR-0098](adr/0098-accounted-byte-replies.md) 将 Bytes 回复接入原协程出口根和至多 256 字节的分块导出，复制/结果控制准入全部成功后才提交状态。结果拥有独立存储及原目标账户票据；完成信封仅借不可变结果租约，在发起方通过分块导入生成独立业务值，不在 Scheduler 锁内整块复制。沿 ABI v15 和原 4352 字节控制预留，不代表完整资源账本或 MVP 验收。

[ADR-0097](adr/0097-accounted-int-list-replies.md) 接入有界 IntList 回复：原协程为候选/回复建立出口根并逐节点检查，复制为独立计费结果后才提交。继续 ABI v15，原生代码、schema、结果拥有者与完成导入的输入边界分别验证。结果只持不可变内容及预留，不保留 actor、模块、堆或执行栈。

[ADR-0096](adr/0096-copied-record-replies.md) 接入 RecordInt 内容回复：目标返回引用先验证/读取固定字段，再提交候选；pending/宿主只持独立 i64 内容，语言完成入口在发起方堆重建记录并发布入口根。沿 ABI v15，具体平台证据见[验证记录](validation-2026-11-14.md)，Bytes 回复由 ADR-0098 扩展。

[ADR-0095](adr/0095-typed-scalar-request-events.md) 扩展语言请求与系统完成签名的标量类型检查。ABI v15 的布局和编码保持，旧 Int 产物仍兼容；旧 runtime 会拒绝新增 Bool/Unit 完成签名。前端、IR、Manifest 和 runtime 各检查自己的输入边界，不以共享检查器代替执行入口防线。

[ADR-0094](adr/0094-typed-host-message-values.md) 扩展直接宿主消息的类型化复制入口。复用现有 record/list/bytes 载荷、入口根和受管导入，不暴露堆引用整数，不改变 ABI v15 或源码拼写；宿主准备阶段持目标账户租约，在 registry 锁外复制，发布时重新验证 Pid/generation。接口与生命周期集中在[运行时](runtime.md)。

[ADR-0093](adr/0093-scheduler-ready-before-park.md) 在实际 parking 的 registry 锁内重查本 worker 队头，避免空认领后已发布工作仍等待下一次通知。启动准入失败仍按原一毫秒退避，不改固定队列归属、活栈 owner 或迁移协议。

[ADR-0092](adr/0092-concurrent-failure-counter.md) 用独立对照驱动检查双发送者的失败前缀，复用既有候选提交、逐 child 监督终态与有限回复表，不改执行引擎、ABI 或语法。对照协议集中在 [v9](beam-workloads.md#v9双发送者的并发失败前缀)，不扩张为超时/恢复或性能结论。

[ADR-0091](adr/0091-manifest-string-preflight.md) 为 Manifest 文件读取增加不构建对象树的字符串预检，预检暂存在原类型解码前销毁；原 serde 表示、语义检查、64 KiB 输入和 2 MiB 模块控制预留不变。不用 CLI 末端截断代替来源分配约束。

[ADR-0090](adr/0090-bounded-cli-reporting.md) 的 VM 级控制输出先预留 8 KiB，再复制 stderr 描述符；帮助按需复制 stdout，直接写静态内容。固定 4 KiB 诊断缓冲限制格式化输出，控制对象保留至最后诊断结束；来源错误 String 不因此自动计费。

[ADR-0089](adr/0089-bounded-cli-startup-arguments.md) 的启动读取只借用原生 argv，先由计费 process 预留 20 KiB，再按固定数量/总字节边界扫描并复制目录。macOS 使用 libc 参数入口，Linux glibc init-array 只保存指针；正常 Rust 运行时和模块装载顺序保留。

[ADR-0088](adr/0088-owned-cli-stdio.md) 让 CLI 业务 I/O 在 initializer 前从同一 process 预留 12 KiB，再复制 stdin/stdout 描述符并创建固定标准库缓冲；副本和缓冲均在预留释放前销毁。

[ADR-0087](adr/0087-accounted-beam-workload-roots.md) 让业务对照驱动使用与 CLI/TCP 相同的计费根入口；根/门闩从原资源额度支付，不回退到工具模型，不将有界驱动的其他宿主分配写成完整账本。

[ADR-0086](adr/0086-accounted-cli-json.md) 的 CLI 适配器先预留 264 KiB，再创建复用行缓冲和取消标志；serde visitor 仅生成固定消息名和至多 64 个标量，输出直接序列化固定结构。来源错误构造、可信启动存储和完整宿主账本仍未闭环。

[ADR-0085](adr/0085-module-path-admission.md) 将计费加载拆为路径解析前的原控制预留与元数据检查后的库页预留，同一模块租约持有两部分，保留原总额和末持有者释放顺序。

[ADR-0084](adr/0084-accounted-root-module-admission.md) 补齐反向组合检查：计费根及后代只接收本 VM 的计费模块，不自动补租约；旧工具树保持原用途。

[ADR-0083](adr/0083-accounted-module-execution.md) 将计费模块的 VM 绑定延伸到所有初始化/执行/镜像接回入口：池必须绑定本 VM 的 process，默认 TLS 工具池在创建前即拒绝。未计费模块的独立工具路径保留；带堆实例要求原账户，纯标量创建后的账户选择由 ADR-0114 收紧，不增加隐式根或账户。

[ADR-0082](adr/0082-bounded-tcp-segments.md) 修正有限 TCP 示例的 socket 边界：macOS 接受后先限制 TCP_MAXSEG，再重设原 16 KiB 收发缓冲；读写后持续核对原 64 KiB 上限，配置失败与缓冲拒绝分别计数。没有改变执行核心、语言或 ABI v15，也不将配置前内核容量或用户态预留写成 OS 硬限制。

[ADR-0081](adr/0081-accounted-default-cli.md) 将默认标量 CLI 与堆 CLI 合并为同一计费构造路径，模块先准入再解析/装载，实例保留同 process 的单槽池。两种模式均使用原堆模式 16 MiB 三级逻辑字节上限；不把它解释为 JSON/标准 I/O、allocator 或动态装载器全部开销的闭环。

[ADR-0080](adr/0080-central-execution-admission.md) 由执行核心在控制/参数/协程和借栈前统一预留原 4352 字节，initializer 本地持有、Invocation 持有至正常退出清理；单/多 worker 删除外层重复票据。显式标量池现在与堆模式一样保存在实例中，detach 仍释放本线程池、attach 重新绑定，不引入活栈跨线程移动。失败不丢队首或载荷，ABI v15 与资源上限不变。

[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 在根与共享准入门闩的 Arc 分配前检查逻辑启动成本，直接初始化根计数，避免保存自引用 Reservation。计费根下所有子构造器都先预留元数据；最后后代/票据释放前根成本持续存在，业务清理只能回到启动基线。CLI 堆模式与 TCP 已采用该入口，完整 VM builder 和其余分配仍待收敛；不修改 ABI v15 或线程执行权。

[ADR-0078](adr/0078-request-publisher-lifetimes.md) 在原固定请求表中分别记录结果端与消息发布端结束，不为每个请求新增堆对象。完成路由移出即释放其代码/事件租约，requests 票据保留到两端结束；退出账户可因仍接受的目标消息继续存活，槽代次只在完整回收时推进。单/多 worker 使用相同生命周期，不改变 ABI v15、活栈唯一执行权或消息准入上限。

[ADR-0076](adr/0076-accounted-byte-messages.md) 将受管 ABI 升至 v15：EffectRequest 为 72 字节，独立 bytes_mask 与记录/列表 mask 互斥；context 的 88 字节布局不变。Bytes 消息通过计费动态载荷复制，不传源 heap handle，接收端独立堆导入与入口根沿现有唯一执行权运行。单 worker/Scheduler 执行控制区统一预留 4352 字节，原资源上限不提高。

[ADR-0072](adr/0072-scheduler-yield-observation.md) 给 Scheduler 增加每 worker 独立、可选预先计费的 token/FIFO，只有真实切回宿主后才附 Pid/worker 发布。默认关闭；诊断锁不跨受管代码，计时线程不访问 actor，受管 ABI v14、强制检查点和执行 owner 不变。固定 FIFO 满丢新样本并计数，不新增无界事件队列。

[ADR-0071](adr/0071-observed-atomic-yield.md) 只扩展宿主控制对象和私有 checkpoint：可选 observer 经账户准入、固定空间、真实协程返回后确认，不改变 ABI v14 或 compiler 检查点。默认不读取时钟或获得诊断锁；启用时诊断锁不跨受管执行/切栈，计时线程仍不访问 actor。

[ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 用六层声明顺序的受管函数执行百万次 checked +1，同 worker 下与 100 个真实 Bytes 心跳 actor 竞争。只增加测试程序，不改语言/ABI；观察 CPU pending 不等于实际运行或让出时间戳，单 worker 数量不代替 OS 单核配额。

[ADR-0069](adr/0069-streamed-open-loop-measurement.md) 为测试宿主增加独立长时测量入口，以固定缓冲事件文件替代随时长增长的常驻样本；无 ABI/语言改动，不排除驱动、gate、采样及写入成本。原 smoke 资源边界不变，完整性能/内存门禁仍待验证。

[ADR-0068](adr/0068-bounded-open-loop-counter.md) 将现有 Bytes Counter、Scheduler 与 BEAM CAS gate 接入短时开环验证；不改 ABI/语言，驱动和采样成本不排除在未来整机测量外。长时性能和内存双目标仍未验证。

[ADR-0067](adr/0067-beam-bounded-admission.md) 在可信 BEAM 测试端增加固定 Pid/容量与 signed atomics 计数，发送前 CAS 占槽、actor 开始归还，关闭设终态；不增加代理、无界等待者或自动重试。其成本须纳入后续 BEAM 整机测量。独立 v4 对照沿现有 Wings Scheduler，不改变 ABI v14、业务提交、资源或语法契约。

[ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 将 Scheduler 的 mailbox 参数允许范围扩至 1..256，不更改默认或现有调用方容量。固定环缓冲按实际槽数构造前三级准入；全局宿主结果/语言 pending 仍各最多 256。认领未 start 的消息继续保留一个容量位置以便失败恢复队首，成功开始后不占排队数；没有无界等待者/自动重试。

[ADR-0065](adr/0065-two-sender-counter-fifo.md) 的双发送者宿主示例让两个真实线程直接经 SchedulerHandle 投递，BEAM 用两个真实进程；没有代发串行代理。业务 Bytes 保存每目标各发送者的已提交序号，独立模型验证合法交错。此为测试协议，不改变 ABI v14、调度/资源实现或语言语法。

[ADR-0064](adr/0064-indexed-supervisor-slots.md) 用固定空槽链和带完整身份校验的 child 槽索引，消除监督创建/查询/停止的容量扫描。可失败准入/初始化完成后才摘链，成功重启保留稳定身份和监督槽；最终终结归还槽。构造前按新固定元数据三级路径计费，不新增动态哈希表；poll 仍按容量扫描。

[ADR-0063](adr/0063-multi-actor-byte-counter.md) 将已提交 Bytes 的定长副本诊断扩至 Scheduler。初始化及 handler 返回后在 detached image 上复制长度/最多 32 B 前缀，锁内只发布副本；owner 存在时拒绝读取，不访问挂起栈/候选。普通快照不暴露前缀，显式接口仅供可信宿主；退出副本不保留资源租约。测试宿主接入多 actor Counter，不改变 ABI v14 或语言语法。

[ADR-0062](adr/0062-sparse-ready-scheduler.md) 用槽内 intrusive FIFO 和固定 worker 本地活栈 FIFO 代替全 actor 派发扫描；空槽链 O(1) 分配、退役移除旧就绪成员，成员/代次/唯一 owner 在同 registry 锁下更新。actor 上限 16384，宿主结果和语言 pending 仍各最多 256；后者有界扫描保留。首次派发按 slot 分布，返回后固定编号迁移不变，不宣称自适应窃取、无锁或性能门禁完成。

[ADR-0061](adr/0061-on-demand-scheduler-stacks.md) 规定 Scheduler worker 的业务栈按需创建：池构造只准入控制和至多两个缓存槽，冷借入在映射前向原 process/tenant/VM 预留完整栈加 guard；复用缓存净转移票据，归还时有界缓存或先解除映射再归还额度。活跃/挂起栈保持原 worker 独占至正常返回，默认固定池不变。不再按空闲 actor 上限预映射所有栈，但不承诺冷借入一定成功，也不将逻辑预留变化称为物理内存结果。

[ADR-0060](adr/0060-shared-scheduler-reply-table.md) 让 Scheduler 在 tenant 启动准入时创建唯一宿主 ReplyTable，固定容量 `config.requests`，不再逐 actor 创建三类型表。Int/Bool/Unit 与 RecordInt 内容回复共用槽、handle 按请求保留类型，schema 校验不变；语言 pending 表独立。每槽的请求票据向目标 process 的原三级路径预留，表只接受原账户或直属子账户。结果不持 Shared/actor/模块，关闭后可消费终态，表控制计费直到最后结果 handle 释放。ABI/语法与线程执行权不变，完整业务计费和大规模调度仍待完成。

[ADR-0059](adr/0059-byte-state-counter-equivalence.md) 增加测试宿主 `beam_counter`，以现有 AOT/Bytes/单 worker 执行固定字节 Counter，不扩展受管 ABI 或服务协议。`ByteStateSnapshot` 只复制已提交 Bytes 的长度/最多 32 B 前缀，无引用或资源租约；宿主读取只在无活跃调用时允许，失败诊断在 actor 清理前复制旧提交根，不读取候选或悬挂栈。多 worker 扩展见 ADR-0063，外部诊断授权/脱敏另行设计。

[ADR-0056](adr/0056-multi-worker-supervision.md) 用独立固定终态票据接入 SchedulerSupervisor，不依赖可覆盖的最近失败缓存。actor 发布端位于最后释放字段，旧镜像/mailbox/控制票据先清理，终态才可消费；票据及重启历史在 tenant 准入。poll 在锁外初始化新实例，沿用已有有限滚动窗口，不增加后台线程或业务 ABI。

[ADR-0055](adr/0055-multi-worker-exit-diagnostics.md) 在 Scheduler 的共享计费控制区保存一个按值失败快照。写入与 Pid 退休共用 registry 锁，跨线程读取不持有模块/堆/账户；固定名称缓冲、不解析错误文本。该缓存可覆盖，不能直接作为不丢终态的监督协议。

[ADR-0054](adr/0054-bounded-multi-worker-scheduler.md) 将独占镜像接入固定多 worker Scheduler：共享有界 registry 管理稳定 Pid、mailbox 和唯一 owner；本地 ScalarTask 保留所有活栈，只有正常返回后的镜像可迁移到下一 worker。native init/resume 不在 registry 锁内执行，没有 unsafe Send。共享扫描、按编号轮转和每 worker 全容量栈池是正确性基线，非 work stealing 或性能结论；已有 Supervisor 仍只服务单 worker。

[ADR-0053](adr/0053-detached-process-images.md) 将原线程栈池绑定从 AotHeap 移到 ScalarProcess，堆仅保留线程安全的原账户；DetachedProcess 独占持有无活栈模块/state/堆，自动 Send，无 unsafe trait。目标 attach 验证 VM 与原 process 账户后才取走镜像，失败可重试。同步执行绑定新池，业务堆及精确根不复制、额度不转移；调度集成见 ADR-0054，仍不支持活栈迁移。

[ADR-0052](adr/0052-cross-thread-module-leases.md) 使模块代码与预算租约同属一个 Arc<ModuleData>，由同一末持有者先释放 Library 再归还预算，避免分离 Arc 的并发析构窗口。ScalarModule/HandlerId 自动 Send/Sync，不增加 unsafe trait 实现。不同线程创建独立 worker 并共享只读代码，最终卸载可发生于最后持有者线程；可信模块不得依赖装载线程 TLS 或可变 native 全局。actor、栈池与挂起执行仍不能跨线程转交，不等于 M:N。

[ADR-0051](adr/0051-bounded-int-list-messages.md) 升至 ABI v12，EffectRequest 增加 u64 list_mask，56→64 字节，context 仍为 88 字节。语言消息用固定 64 整数序列及规范段描述复制 IntList，逐节点扣 runtime 预算；源协程持根遍历、目标独立堆重建并更新入口根，不共享源 handle。Message/actor 控制区按实际大小计费，固定执行槽预留另有结构大小回归。

[ADR-0050](adr/0050-account-node-admission.md) 将子账户节点的逻辑存储纳入父路径准入，内部票据仅引用父级，无自循环。worker 创建计费进程账户；actor 退出后，跨线程入口/结果仍可延长节点及祖先租约。[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 增加计费根/门闩并强制该树子节点计费；旧工具树和其余分配仍未完成全部门禁。

[ADR-0049](adr/0049-record-message-copies.md) 升至 ABI v11，在 EffectRequest 末尾新增 u64 record_mask，结构从 48 到 56 字节，context/入口不变。源堆仅在 callback 内校验并复制 RecordInt 字段，目标进入业务前按预算重建，固定宿主入口根覆盖 GC/暂停；无共享堆或外部 handle 注入。

[ADR-0048](adr/0048-bounded-loopback-service.md) 的 loopback TCP Counter 由一个有预算的外部适配线程接收固定帧，经有界请求入口交给原 worker；native I/O 不进入业务线程。线程/连接数、缓冲及等待有明确上限，错误先停止并 join，再释放 VM；该示例不是通用 reactor 或可部署的网络执行引擎。

[ADR-0047](adr/0047-cross-thread-ingress-requests.md) 在固定外部入口上增加请求模式，参数后由 ADR-0115 扩为类型化副本：结果表无模块/actor 引用，调用者与消息发布者共同持有有界槽位和资源票据；worker 提交 state 后才发布回复。外部 Condvar 等待不进入受管 handler，不修改 ABI 或 M:N 执行权。

[ADR-0046](adr/0046-bounded-cross-thread-ingress.md) 让外部线程通过固定计费 ingress 发布单向消息并通知原线程 worker，类型化内容副本后由 ADR-0115 扩展；HandlerId、actor 和活栈不跨线程。ring 与 OS 通知预算跟随最后 handle，不由 worker 提前释放；不据此声称通用 reactor 或 M:N 完成。

资源账户按 [ADR-0045](adr/0045-concurrent-resource-admission.md) 用同 VM 共享门闩串行提交三级四维准入、净转移和归还；只在固定长度计数更新中持锁，不在锁内执行分配/业务/OS 等待或协程切换。账户与票据自动 Send/Sync，actor/活栈仍保持原线程执行权。

原生模块按 [ADR-0044](adr/0044-native-module-leases.md) 持有 Library 代码租约，最后一个模块/ID/实例/活栈所有者销毁后交还装载器引用。`load_accounted` 先准入 2 MiB 控制预算和文件页，再按 [ADR-0128](adr/0128-native-module-image-admission.md) 解析有界 ELF64/Mach-O64 段表；文件快照释放后补足原生页对齐的映像跨度预算，成功才调用装载器。常驻总额为控制预算加文件页/映像跨度的较大者，映像独立限制 64 MiB；失败完整退账，跨 VM 计费 worker/pool 执行前拒绝。不把该逻辑容量当动态装载器/依赖库内部开销或实际 RSS，也不是热更新。

计费产物按 [ADR-0129](adr/0129-native-module-dependency-boundary.md) 限制原生依赖：macOS 只接受普通系统 libSystem 装载命令，Linux 只接受 libc 的明确 SONAME，不支持额外原生库、rpath 或其他加载重定向。依赖预检在动态装载前进行；平台系统运行库属于可信宿主基线，仍计入整机内存观测。该规则不认证宿主装载器环境，也不是 native 沙箱。

[ADR-0041](adr/0041-worker-deadline-parking.md) 用计费 parker 集成空闲等待与请求 deadline；ADR-0046 仅经有界 ingress 提供通知，不导出裸 notifier，也不是完整 reactor。

[ADR-0040](adr/0040-language-request-events.md) 在 ABI v10 扩展固定 EffectRequest，整数请求使用静态完成事件；完成消息预留随 pending 保留、入队时净转移，普通外部入口不能伪造 RequestId。记录/列表复制最初在 ABI v12 接入，当前 Bytes 消息使用 ABI v15。

[ADR-0039](adr/0039-local-process-capabilities.md) 的 ABI v9 以固定 EffectRequest 和结果槽扩展 effect 回调；局部 Pid 能力按 worker/module 校验，同模块创建继承资源上限而非状态，无跨 worker 迁移。

[ADR-0038](adr/0038-self-send-effects.md) 升至 ABI v8，context 尾增加可空 EffectCall；只在 worker 执行 handler 时连接自发送。effect 按值交接，禁止回调重入 actor，旧 v7 产物须重编译。

[ADR-0037](adr/0037-immutable-int-lists.md) 升至 ABI v7，在原有 context/回调签名中增加有类型 ListRef、不可变 IntList 节点与传递 GC；旧 v6 产物须重编译。列表仍不跨消息边界，不改变非移动 GC。

源码业务函数按 [ADR-0042](adr/0042-managed-function-frames.md) 编译为私有非递归受管调用：四参数协议复用 ABI v11，每层薄入口检查实际剩余栈，caller/callee 链接精确根，失败逐层正常返回。最多 32 层函数链、64 个聚合根；不开放递归或动态派发，不等于完整 M0。

受管返回状态与输出校验按 [ADR-0035](adr/0035-bounded-exit-diagnostics.md) 分类，固定退出快照不持有模块、业务引用或资源所有权；ABI v11 与既有事件形状不变。

[ADR-0032](adr/0032-safe-point-diagnostics.md) 规定宿主诊断只传递开始/让出/返回边界的值快照，不保存悬挂 context 或跨线程读取 actor；不改变 ABI v11 与现有协程所有权。

当前有界记录状态契约见 [ADR-0027](adr/0027-persistent-record-state.md)：ABI v11 沿用新增 effect 回调后的 context 布局，允许所属进程堆内的 Ref 状态，以宿主持久根合并 compiler 根；原始整数参数仍限定标量，复制消息与回复的新增范围见 ADR-0094/0096/0097，系统完成事件首参额外允许 RequestId。无显式计费堆不能初始化 Ref 状态，旧产物须重新编译。

宿主监督通过显式堆容量配置重建 RecordInt 进程，沿用现有 worker 准入与 initializer，不新增 ABI 或依赖；新堆不继承旧状态根。见 [ADR-0028](adr/0028-supervised-record-heaps.md)。

有界条件表达式按 [ADR-0029](adr/0029-scalar-conditional-expressions.md) 降为真实分支，只执行选中臂；[ADR-0033](adr/0033-record-conditional-roots.md) 增加 RecordInt 合流，恢复外层路径根并在合流检查前发布选中结果。ABI v11 不变，尚无臂内语句或任意引用 phi；state while 另见 ADR-0031/0034。

标量比较使用显式类型化 IR 与 Cranelift signed compare，不通过可能溢出的相减模拟顺序；根、检查点、ABI v11 及消息边界不变。见 [ADR-0030](adr/0030-scalar-comparisons.md)。

state 循环使用结构化 Loop/While/EndLoop、header/exit 块参数与回边强制检查；[ADR-0034](adr/0034-record-state-loops.md) 开放唯一 RecordInt state 携带值，检查前发布 header、回边保活候选、出口恢复外层根。当前 ABI v12 的固定栈与预算契约保留；无任意引用 phi，基础标量契约见 [ADR-0031](adr/0031-scalar-state-loops.md)。

[文档目录](../docs.md) · [项目概览](../README.md)

Wings 用 Rust 实现，作为 VM 程序运行。业务代码使用 `wings-lang` 编写，`wings-compile` 通过 Cranelift 生成 AOT 产物，`wings-vm` 负责装载、进程、内存、调度和消息。`wings-lsp` 与编译器共用语言分析库，提供编辑器服务。产物格式、入口、启动配置、退出行为与对外 I/O 范围在 M0 确定。

包职责与依赖见 [Crate 拆分](crates.md)，当前选择的理由见 [ADR](adr/README.md)。`wings-vm` 包含执行引擎与薄的命令行入口。

性能优化集中在 handler 执行、消息传递和调度路径；内存优化集中在进程元数据、arena 分配与空闲状态。BEAM 是两项优化的主基线，根维护、额度检查、编解码和 GC 都计入成本。

## 设计约束

1. **边界先于性能**：隔离范围仅限受管业务故障；runtime、编译器与可信原生内建属于可信计算基。不可信原生代码必须放在 OS 子进程中。
2. **逻辑独立堆，不承诺每进程独立 mmap**：共享分配池降低映射成本，专属堆块与受控指针保证逻辑归属；单独核算映射、RSS 和资源元数据。
3. **handler 返回才无栈**：运行中的 handler 可以被 safepoint 抢占，Idle 才能释放栈并进一步冬眠；首版不支持函数内部阻塞等待。
4. **轮询抢占，不依赖信号**：计时线程设置原子标志，编译器保证检查覆盖，runtime 长操作也受预算约束；不承诺硬实时。
5. **代码生命周期与状态迁移分开**：首版静态加载，后续在 Idle 边界做状态迁移与版本化入口切换，不因 VTable 已替换就卸载旧代码。
6. **性能与内存同时验收**：目标是在同一组代表性负载下，吞吐高于 BEAM、整机内存低于 BEAM；AOT 也会引入根维护、额度与插桩成本，按[评测协议](evaluation.md)计入并验证。

## 架构总览

```text
┌─────────────────────────────────────────────────────┐
│ 业务代码                                             │
│   wings-lang：不可变业务值、异步消息、返回式 handler    │
├─────────────────────────────────────────────────────┤
│ wings-compile                                         │
│   单前端 → 受管 IR → Cranelift AOT                    │
│   safepoint、shadow stack、栈限额与版本化 ABI          │
├─────────────────────────────────────────────────────┤
│ wings-vm（Rust）                                      │
│   逻辑独立 arena + 精确非移动 GC                       │
│   M:N + 原子轮询抢占 + 有限 inbox + 多级资源账户        │
│   request/timer、监督、诊断、可选 Idle 冬眠             │
├─────────────────────────────────────────────────────┤
│ VM 的可信原生内建与少量平台代码                       │
│   有预算的 I/O/内建桥接、受管上下文切换                 │
│   原生插件子进程池与 IPC 后续立项                      │
└─────────────────────────────────────────────────────┘
```

编译前端拆为源码、语法和语义分析库，供编译器与 LSP 共用。LSP 不依赖受管执行 IR、Cranelift 或 VM；编译器与 VM 通过 `wings-abi` 和 `wings-artifact` 共享契约，不互相依赖实现。

## 编译后端、受管 ABI 与 GC

未发布源码另有 [ADR-0018](adr/0018-bounded-record-heap.md) 的 `RecordHeap`/`ShadowRoots` 原型：不可变定长记录按布局保存无动态标签的字段，显式根帧配合精确非移动 mark-sweep，构造前向进程/租户/VM 预留容量。[ADR-0025](adr/0025-aot-record-roots.md) 接入 compiler 精确活跃根，[ADR-0026](adr/0026-language-temporary-records.md) 开放语言单 Int 字段记录，[ADR-0027](adr/0027-persistent-record-state.md) 增加持久状态根；已有 RecordInt/有界 IntList/Bytes 复制消息及入口根，对应回复出口根与复制由 ADR-0096/0097/0098 接入，通用业务值仍未完成，下面的完整受管契约仍待实现。

首版采用 **wings-lang 单前端 + Cranelift AOT 后端 + 显式 shadow stack**。M0 先验证两个目标平台上的产物生成、调用约定、根发布和上下文恢复，确保每个 safepoint 的活跃值都能恢复，再优化 GC。当前仅接入下面的标量原型，完整后端仍待这一步验证。

- **不提供 `#[process]` Rust 子集前端**：proc-macro 无法自动控制依赖库内部的循环、最终寄存器分配和全部栈布局。普通 Rust 只能作为可信内建，通过受管桥接调用，且不能把普通 Rust 栈帧当作可扫描、可任意抢占的业务帧。
- **最小 ABI 先冻结**：规定 context 参数、值与错误表示、栈对齐、寄存器 clobber、根链发布顺序、返回路径和代码版本引用。GC 与切换只能发生在满足 ABI 的位置，禁止 Rust panic/unwind 跨越受管边界。
- **根枚举是精确的**：所有在 safepoint 后仍活跃的堆引用都进入有类型的 shadow stack 槽；初版不混用寄存器保守扫描。不访问原生帧的任意内存来猜测指针。
- **初版 GC 是进程内、非移动式 mark-sweep**：从业务 state、shadow stack、当前消息和受管内建的显式根开始标记，再回收不可达对象。其他进程无需为该次本地 GC 全局停顿，但共享分配器和代码加载仍可能引入同步。
- **先正确再并发增量**：小堆仍执行完整 GC，但按 [ADR-0043](adr/0043-chunked-runtime-gc.md) 每 16 个节点/根检查预算、取消和让出；同 actor 在收集期间不执行其他入口，不需要并发写屏障。公开各块与完整收集的时间；M3 再按延迟数据决定是否并发增量，须明确标记不变量、分配颜色、根变更和 runtime 写入协议，不能仅凭业务值不可变就跳过同步设计。
- **升级到 stack map 是可选优化**：只有在 shadow stack 开销成为瓶颈时，才接入后端精确元数据与全部相关寄存器位置。GC 遍历本身优先用 Rust，不为了栈扫描额外手写汇编。

是否需要为每条堆写加屏障，取决于最终 GC 算法。评测分别列出分配、根维护、扫描和屏障成本。GC 的 `<10µs` 只是在限定活数据与根数量下的候选目标，不适用于任意进程。

## 标量 AOT 原型

执行所有权有两种宿主接口：`ScalarExecution<'_>` 独占借用已有实例；`ScalarTask` 按值拥有实例与私有 invocation，无需把借用和被借用者放进自引用槽位。任务可在同线程容器内移动，挂起时不暴露可变实例；`start` 直接接收模块绑定的 `HandlerId`，忙时拒绝新调用，完成后才允许下一条。两条路径共用原栈恢复、候选提交和正常取消清理，均不能跨线程恢复。任务仍无身份、mailbox 或调度协议，细节见 [ADR-0015](adr/0015-owned-scalar-tasks.md)。

当前源码的 `wings-compile build` 将单个 process 的 handler 降为有类型寄存器 IR，校验结构化分支、标量/RecordInt state 回边及作用域后由 Cranelift 0.118 生成 PIC 对象，再用系统 `cc` 链接为共享库。不使用 JIT，也不执行源码完成编译或 LSP 查询。每入口最多 4096 条静态操作、每 process 最多 128 个 handler、每 handler 最多 64 个参数，超限拒绝构建；动态执行仍受总预算约束。

输出目录必须新建，成功后包含 `manifest.json` 与 `program.dylib`（macOS）或 `program.so`（Linux）。清单上限 64 KiB，共享库上限 64 MiB；版本、标量 ABI、目标、名称、参数和回复类型在装载前检查，库路径固定，不接受清单指定的任意路径。链接失败不生成清单，可能留下不完整目录，需要使用新的输出目录重试。

未发布源码 ABI v22 的入口仍是 `(state: i64, args: *const i64, output: *mut ScalarOutput, context: *mut ScalarContext) -> u32`，C 调用约定；输出固定为两个连续 i64：候选状态、回复。context 按 C 布局依次保存取消标志指针、总剩余 `u64` 步数、让出标志指针、当前时间片剩余 `u64` 步数、可空慢路径函数指针、opaque 数据指针、`usize` 栈下界、shadow frame 链头、可空堆回调、独立 heap data 指针及可空 effect 回调，context 共 88 字节。两个标志均指向稳定且有效的 `AtomicU32`，其他线程只能原子设置；其余上下文只由执行线程访问。宿主原始整数参数只允许 Int/Bool/Unit；RecordInt 回复的 ABI 输出为当前堆引用，runtime 在候选提交前验证并复制唯一 i64 内容；语言消息与 ADR-0094 类型化宿主消息参数额外允许 RecordInt/有界 IntList/计费 Bytes/String 复制载荷，系统完成事件首参额外允许 RequestId，状态额外允许所属进程堆内 Ref/ListRef/BytesRef/StringRef，String 消息按 ADR-0105 复制，回复按 ADR-0106 导出独立计费结果；String 索引/切片按 ADR-0127 使用标量位置。Bool 编码 0/1，Unit 编码 0。宿主校验参数及成功输出，产物只在正常返回时写输出；算术、取消、预算、栈、堆、无效根、非法字节参数、非法字符串和 String 越界均以状态码返回，不 trap 或 unwind。

薄入口先检查剩余栈，业务帧内再于每条 IR 指令前与输出提交前固定插入检查：优先返回取消，再判断总预算耗尽，随后处理让出请求或时间片耗尽，最后同时扣减总预算与时间片。慢路径返回后重新检查，尚未执行的指令只执行一次、不重复扣费；无调用及 GC 的 N 条直线 IR 指令成功需 N+1 步。源码函数逐帧计算，受管 GC 另按 [ADR-0043](adr/0043-chunked-runtime-gc.md) 每块扣一步。每个入口默认总预算 1,000,000，handler 可指定。预算不是机器指令数或 CPU 时间，检查后的迟到取消不能撤回成功调用。已发布 `0.0.2` 使用 v1，历史源码 v2–v16 均与当前 v17 拒绝混用，须使用匹配的编译器和 VM。

`start_controlled` 从每线程有界共享池借出 256 KiB 可用 guarded stack（默认 64 槽、缓存 2 槽），`start_in_pool` 可显式传入 `StackPool`；耗尽或分配失败先返回，不终止实例。池使用自研的整块 mmap、按平台页大小设置 guard page、已完成槽位缓存和 `trim` 页回收。`ScalarExecution` 拥有参数、令牌、协程，并独占借用实例，不能 Send/Sync。`resume(非零时间片步数)` 在生成代码检查点经 C ABI 慢路径暂停，保留原栈和全部标量活跃值，再从原位置恢复；完成后才提交并立即归还槽位。切换使用禁用 unwind 的 `corosensei 0.3.3`，默认栈分配器只用于对照实验。丢弃未完成 handle 会终止实例；挂起栈先收到内部取消命令、正常返回再释放，不调用强制 unwind/reset。同步 `invoke`/CLI 与 initializer 也借用池内栈，正常返回立即归还；worker 通过 `instantiate_in_pool` 将初始化栈计入进程额度，额度不足在 spawn 时回滚。池不代替 compiler 根、GC 或完整资源账本，当前栈检查见 [ADR-0024](adr/0024-scalar-stack-boundary.md)，原栈恢复与池契约见 [ADR-0011](adr/0011-scalar-stackful-resume.md) 和 [ADR-0012](adr/0012-bounded-stack-pool.md)。

`wings-vm run --trusted <目录>` 只装载自己构建的可信本机产物。**清单校验不能证明机器码安全**；共享库装载本身可能执行原生初始化代码，`--trusted` 不是沙箱。库 API 的装载入口为 `unsafe`，调用者必须保证来源、ABI 和无外部 native 回调/线程的生命周期要求。共享库由代码租约持有，不再永久泄留；OS 是否立即解除页映射由装载器决定，不提供运行中热卸载。CLI 两种模式都使用计费加载，独立库工具入口 `load_trusted` 仍不计费。

驱动从 stdin 接收最多 64 KiB 的逐行 JSON：`{"message":"Inc","args":[3]}`。Int 使用有符号 64 位 JSON 整数，Bool 使用 JSON 布尔，Unit 使用 null。RecordInt 回复输出 `{"reply":{"record_int":N}}`，JSON 输入仍不接受记录或内部编号。其他请求输出 `{"reply":值}`，`-> Unit` 输出 `{"reply":null}`；单向入口成功处理后输出 `{"accepted":true}`，这是驱动确认，不是语言自动回复。输入错误或受管算术失败写 stderr 并退出非零，失败调用不提交候选状态，也不输出成功回复；此前已完成调用的输出不撤回。stdin EOF 正常退出，启动失败退出 1，命令用法错误退出 2。原型没有网络、文件或语言内 I/O。

CLI 路径没有计时线程、actor 调度、mailbox 或监督，不能运行不可信代码；`--heap-records` 可显式使用有界 RecordInt/IntList GC，宿主 worker 另见下节，资源账本仍未覆盖全部分配。原标量 AOT 在 macOS AArch64 与 Linux x86-64（glibc）均已通过 [CI 原生执行验证](https://github.com/wsafight/wings/actions/runs/43089909858)；活栈、有界池、资源守恒与 owned task 切片已另通过本分支的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43099984279)，历史结果不覆盖后续 ABI/GC/OS 唤醒变更。浮点/SIMD、全部业务根、跨线程迁移与完整受管执行模型仍未验证。原型边界见 [ADR-0009](adr/0009-scalar-aot-prototype.md)。

共享代码通过 `ScalarModule` 装载一次，再 `instantiate` 创建状态独立的实例；清单和入口表只保存一份，每个实例都执行 initializer。`HandlerId` 持有模块身份与序号，用前检查身份及参数 schema，不接受其他模块的 ID；ID 不是线协议。便利入口 `ScalarProcess::load_trusted` 仍会装载独立模块，不隐式按路径缓存。JSON 适配层只解析一次名称，参数暂存在有界数组，再直接按 ID 调用，避免原先重复查找和参数 Vec 分配。共享对象仅为只读代码元数据，不改变业务状态隔离。见 [ADR-0013](adr/0013-shared-scalar-modules.md)。

## 低层实现范围

[ADR-0127](adr/0127-accounted-string-indexing-and-slicing.md) 在 HeapCall 操作码 22/23 增加 scalar value 字符读取与子串复制，使用独立越界状态 16，源码 ABI v22。旧产物须重编译；不修改 context/EffectRequest 布局、执行预留或既有容量。

[ADR-0074](adr/0074-bounded-concurrent-recovery.md) 以现有 ABI 和公开入口集成核对活栈候选 Bytes/collect、有限 mailbox、宿主结果取消/超时、stop 与全新监督重启。没有改变查询活栈 actor 时拒绝 idle Bytes snapshot 的边界，不把结果取消作为执行取消。

宿主 `SchedulerHandle` 与 `SchedulerSupervisor` 可通过 `spawn_with_heap_in_pool` 显式借用同 tenant 的计费初始化池，顺序创建复用固定缓存，不再逐次创建 mapping。每次仍绑定目标 process、完整准入并正常返回后发布；不把池保存为跨线程状态，自动重启仍使用原临时池。默认入口不变，详见 [ADR-0073](adr/0073-reusable-initializer-stack-pool.md)。

v2 与 v5/v6 对照驱动使用一槽显式池，构造包含在原阶段 deadline 内；全部 actor 初始化/配置后才开始业务，最终核对账目。开环驱动在批量创建后立即销毁初始化池。该集成只复用宿主初始化资源，不改变 worker 池、受管 ABI 或验收规模。

[ADR-0058](adr/0058-byte-i64-big-endian-access.md) 为原 Bytes HeapCall 添加固定宽度 signed big-endian 读写；ABI v14 不改变 context 或 effect 布局。读取固定八字节，替换复制原对象并复用相同根、预算和三级计费边界，非法完整范围受管返回 InvalidBytes。

当前 ABI v22 保留 [ADR-0024](adr/0024-scalar-stack-boundary.md) 的薄入口与静态帧准入，使用 [ADR-0025](adr/0025-aot-record-roots.md) 的 compiler shadow frame：在 safepoint 和堆回调前发布仍有后续用途的 Ref/ListRef/BytesRef/StringRef/RecordRef，候选状态保活至出口，所有返回路径恢复父帧。每函数最多 64 个同时活跃根；GC 汇总最多 64 帧/64 个 compiler 根，另加一个宿主持久状态根及最多 64 个已导入消息参数根，先完整验证，再收集。`instantiate_with_heap` 建立固定容量、三级计费堆；堆控制块按预留后 fallible 分配，实例只保留可选独占指针，执行时将所有权移入协程、返回再归还，未产生实例自引用。暂停保留链和根槽，Drop 正常取消解链；没有后台 GC 或宿主暂停期间收集接口。String 索引/切片的边界与分块语义见 [ADR-0127](adr/0127-accounted-string-indexing-and-slicing.md)。状态提交契约见 [ADR-0027](adr/0027-persistent-record-state.md)。

[ADR-0057](adr/0057-accounted-immutable-bytes.md) 的 Bytes 占一个堆槽及独立计费 buffer，GC 作为无出边叶节点。四个 HeapCall 操作从 compiler 固定三整数 scratch 读取参数，先复制参数再覆盖输出；构造/复制每块最多 256 字节并扣预算，未发布 builder 同样计费且取消清理。scratch 是可信指针，不构成 native 沙箱。Bytes 可作受管内部值、持久状态及 ADR-0076 的语言复制消息参数；计费回复和完成导入见 ADR-0098。

宿主调度入口 `ScalarWorker` 将模块实例化为 generation Pid，使用有界 mailbox 与计费栈池轮转执行；固定 pending 表提供异步请求及轮询 deadline。`spawn` 默认无堆，`spawn_with_heap` 在初始化前建立进程计费堆；`instantiate_with_heap` 要求传入同 process 绑定池，实例的同步调用复用该池，显式传入其他账户或未计费池则执行前拒绝。CLI 可用 `--heap-records`，不改变标量 JSON 消息协议，不将它当作语言内消息或完整受管执行。契约见 [ADR-0020](adr/0020-single-worker-mailbox.md)、[ADR-0021](adr/0021-bounded-requests.md) 与 [ADR-0026](adr/0026-language-temporary-records.md)。

可选执行计时线程只跨线程设置原子让出标志，不访问协程或资源账户；原线程仍在强制 safepoint 正常挂起/恢复。默认关闭，Drop 撤销并 join；不额外改变当前 ABI，不作为完整公平性证明，见 [ADR-0022](adr/0022-worker-deadline-preemption.md)。

`ScalarSupervisor` 独占宿主 worker，固定子项及重启历史，按策略有界重建失败实例；每次重启重新执行 initializer、生成新 Pid，不保存失败状态、不重放消息，也不改变 ABI 或语言范围。当前仅 one-for-one 宿主接口，边界见 [ADR-0023](adr/0023-bounded-host-supervision.md)。

当前标量后端另按 [ADR-0017](adr/0017-scalar-frame-admission.md) 在生成机器码后、写产物前检查 initializer、每个 handler 与源码 fn 的实际业务帧，默认最大 64 KiB，库调用可降低但不可提高。超限构建失败且不创建输出目录。该值不包含 ABI setup、宿主包装和慢路径调用链；[ADR-0024](adr/0024-scalar-stack-boundary.md) 的薄入口检查实际可用空间，源码 fn 按 [ADR-0042](adr/0042-managed-function-frames.md) 逐帧应用，不把单帧上限当整条调用链的总栈开销。

语言、IR、调度与资源契约共用，目标 ABI、对象格式、上下文切换和 OS I/O 由平台层处理。汇编行数少不代表实现正确或容易维护。

| 模块 | 首版选择 | 验证重点 |
|---|---|---|
| 上下文切换 | 优先评估已有实现；需要时分别实现 Darwin AArch64 与 Linux x86-64 切换桩 | 栈对齐、受管调用约定、浮点/SIMD、迁移、平台控制流保护，以及调试工具兼容性 |
| safepoint 与栈限额检查 | 后端生成，受管慢路径可见 | 检查覆盖、优化后不消失、活跃值恢复与超限错误路径 |
| GC 根遍历 | Rust 遍历显式 shadow stack | 根发布完整性、内建桥接和生命周期 |
| 信号与原生栈增长 | 首版不实现 | 不依赖捕获未知 `SIGSEGV` 后继续运行 |

## 首要运行平台

未发布源码提供独立的 `WorkerParker`/`WorkerNotifier`：Linux 使用 epoll/eventfd，macOS 使用 kqueue 用户事件。等待端独占、通知端可克隆，通知可合并且先通知后等待不会丢失提示。单 worker 已通过计费 parker 接入就绪检查与 deadline；跨线程通知只经 ADR-0046 的有界 ingress 导出，不提供裸 worker notifier。它不是计算抢占或完整 I/O reactor。底层超时与生命周期见 [ADR-0016](adr/0016-worker-wakeup.md)，等待边界见 [ADR-0041](adr/0041-worker-deadline-parking.md)，各后端须分别执行本分支 CI。

macOS AArch64（Apple Silicon）与 Linux x86-64 同为首版目标，M0 原型和后续运行时验收均须覆盖两者。Linux AArch64、macOS x86-64 留待 M7 扩展；最低 OS 版本和工具链版本在 M0 冻结。

| 项目 | macOS AArch64 | Linux x86-64 |
|---|---|---|
| AOT 产物 | Darwin ABI、Mach-O，经系统工具链链接与装载 | System V ABI、ELF，经系统工具链链接与装载 |
| I/O 与 worker 唤醒 | `kqueue`，用 `EVFILT_USER` 等经验证的通知路径唤醒 | `epoll` + `eventfd`；`io_uring` 仅作后续优化 |
| 上下文恢复 | 验证栈对齐、平台保留寄存器、浮点/SIMD 与跨线程恢复 | 验证栈对齐、浮点/SIMD 与跨线程恢复 |
| 内存 | 按实际页大小映射，统计 RSS、物理占用与压缩内存 | 按实际页大小映射，统计 RSS/PSS、映射与页表成本 |
| 原生装载 | 验证代码签名及目标部署策略，不默认依赖 JIT entitlement | 验证重定位、装载权限及目标部署策略 |

两个目标分别生成产物，不跨 OS/架构直接装载。生成 AOT 对象后，M0 还要验证链接、入口、装载校验、执行与退出的完整流程。macOS 的签名和 hardened runtime 限制按实际分发方式验证，不以全局关闭系统保护为前提。

平台层向 OS 查询页大小和计时能力，不写死 `4KiB` 页。Darwin AArch64 与其他 AArch64 平台的 ABI 不能直接混用，不得占用平台保留的 `x18`；实际启用的指针认证和控制流保护也要通过切换测试。即使其他 AArch64 平台已经能恢复上下文，Darwin 仍需专项验证。

各平台在同机条件下对照 BEAM，分别记录 Linux 与 macOS 的 CPU/内存限制能力。macOS 的限制方式不能按 Linux 的 cgroup 或硬绑核接口来假定。

## 优化演进

保留最小受管实现作为正确性基线，逐项测试[激进优化](optimizations.md)。先处理值布局、分配、消息和调度的热点；移动式 GC、无栈续体和替代编译后端另行设计，不作为首版默认方案。

## 节点分布（首版之后）

首版只运行单节点，远程 Pid 的位分配暂不确定。`Envelope` 带版本，本地寻址接口也为未来协议留出扩展位置，但分布能力尚未实现。跨节点认证、额度、重试、顺序和分区时 link/monitor 的语义在 M7 单独立项；跨机器的进程死亡与网络断连需要区分，本地投递契约不能直接用于网络。

## 相关文档

- [语言设计](language.md)定义受管业务代码的语义。
- [Crate 拆分](crates.md)说明包职责、内部模块与 LSP 的实现边界。
- [架构决策](adr/README.md)记录当前设计的理由和重审条件。
- [运行时](runtime.md)说明内存、调度、消息与代码生命周期。
- [评测与验收](evaluation.md)列出 ABI、根发布和上下文恢复的验证要求。
- [激进优化](optimizations.md)列出性能与内存优化的候选、验证条件和回退路径。
