# 架构设计

原生模块按 [ADR-0044](adr/0044-native-module-leases.md) 持有 Library 代码租约，最后一个模块/ID/实例/活栈所有者销毁后交还装载器引用。`load_accounted` 在解析/装载前向 VM 预留库文件取整大小加 2 MiB 逻辑控制预算，跨 VM 计费 worker/pool 执行前拒绝；不把该预算当实际映射/RSS，也不是热更新。

[ADR-0041](adr/0041-worker-deadline-parking.md) 用 worker 私有计费 parker 集成空闲等待与请求 deadline；不导出跨线程 notifier，也不是完整 reactor。

[ADR-0040](adr/0040-language-request-events.md) 的 ABI v10 扩展固定 EffectRequest，整数请求使用静态完成事件；完成消息预留随 pending 保留、入队时净转移，普通外部入口不能伪造 RequestId。

[ADR-0039](adr/0039-local-process-capabilities.md) 的 ABI v9 以固定 EffectRequest 和结果槽扩展 effect 回调；局部 Pid 能力按 worker/module 校验，同模块创建继承资源上限而非状态，无跨 worker 迁移。

[ADR-0038](adr/0038-self-send-effects.md) 升至 ABI v8，context 尾增加可空 EffectCall；只在 worker 执行 handler 时连接自发送。effect 按值交接，禁止回调重入 actor，旧 v7 产物须重编译。

[ADR-0037](adr/0037-immutable-int-lists.md) 升至 ABI v7，在原有 context/回调签名中增加有类型 ListRef、不可变 IntList 节点与传递 GC；旧 v6 产物须重编译。列表仍不跨消息边界，不改变非移动 GC。

源码业务函数按 [ADR-0042](adr/0042-managed-function-frames.md) 编译为私有非递归受管调用：四参数协议复用 ABI v10，每层薄入口检查实际剩余栈，caller/callee 链接精确根，失败逐层正常返回。最多 32 层函数链、64 个聚合根；不开放递归或动态派发，不等于完整 M0。

受管返回状态与输出校验按 [ADR-0035](adr/0035-bounded-exit-diagnostics.md) 分类，固定退出快照不持有模块、业务引用或资源所有权；ABI v10 与既有事件形状不变。

[ADR-0032](adr/0032-safe-point-diagnostics.md) 规定宿主诊断只传递开始/让出/返回边界的值快照，不保存悬挂 context 或跨线程读取 actor；不改变 ABI v10 与现有协程所有权。

当前有界记录状态契约见 [ADR-0027](adr/0027-persistent-record-state.md)：ABI v10 沿用新增 effect 回调后的 context 布局，允许所属进程堆内的 Ref 状态，以宿主持久根合并 compiler 根；普通参数与回复仍限定标量，系统完成事件首参额外允许 RequestId。无显式计费堆不能初始化 Ref 状态，旧产物须重新编译。

宿主监督通过显式堆容量配置重建 RecordInt 进程，沿用现有 worker 准入与 initializer，不新增 ABI 或依赖；新堆不继承旧状态根。见 [ADR-0028](adr/0028-supervised-record-heaps.md)。

有界条件表达式按 [ADR-0029](adr/0029-scalar-conditional-expressions.md) 降为真实分支，只执行选中臂；[ADR-0033](adr/0033-record-conditional-roots.md) 增加 RecordInt 合流，恢复外层路径根并在合流检查前发布选中结果。ABI v10 不变，尚无臂内语句或任意引用 phi；state while 另见 ADR-0031/0034。

标量比较使用显式类型化 IR 与 Cranelift signed compare，不通过可能溢出的相减模拟顺序；根、检查点、ABI v10 及消息边界不变。见 [ADR-0030](adr/0030-scalar-comparisons.md)。

state 循环使用结构化 Loop/While/EndLoop、header/exit 块参数与回边强制检查；[ADR-0034](adr/0034-record-state-loops.md) 开放唯一 RecordInt state 携带值，检查前发布 header、回边保活候选、出口恢复外层根。ABI v10、固定栈和预算不变；无任意引用 phi，基础标量契约见 [ADR-0031](adr/0031-scalar-state-loops.md)。

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

未发布源码另有 [ADR-0018](adr/0018-bounded-record-heap.md) 的 `RecordHeap`/`ShadowRoots` 原型：不可变定长记录按布局保存无动态标签的字段，显式根帧配合精确非移动 mark-sweep，构造前向进程/租户/VM 预留容量。[ADR-0025](adr/0025-aot-record-roots.md) 接入 compiler 精确活跃根，[ADR-0026](adr/0026-language-temporary-records.md) 开放语言单 Int 字段记录，[ADR-0027](adr/0027-persistent-record-state.md) 增加持久状态根；尚无引用消息/回复根与通用业务值，下面的完整受管契约仍待实现。

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

未发布源码 ABI v10 的入口仍是 `(state: i64, args: *const i64, output: *mut ScalarOutput, context: *mut ScalarContext) -> u32`，C 调用约定；输出固定为两个连续 i64：候选状态、回复。context 按 C 布局依次保存取消标志指针、总剩余 `u64` 步数、让出标志指针、当前时间片剩余 `u64` 步数、可空慢路径函数指针、opaque 数据指针、`usize` 栈下界、shadow frame 链头、可空堆回调、独立 heap data 指针及可空 effect 回调，context 共 88 字节。两个标志均指向稳定且有效的 `AtomicU32`，其他线程只能原子设置；其余上下文只由执行线程访问。普通参数与回复只允许 Int/Bool/Unit，系统完成事件首参额外允许 RequestId，状态额外允许所属进程堆内 Ref/ListRef；Bool 编码 0/1，Unit 编码 0。宿主校验参数及成功输出，产物只在正常返回时写输出；算术、取消、预算、栈、堆与无效根错误均以状态码返回，不 trap 或 unwind。

薄入口先检查剩余栈，业务帧内再于每条 IR 指令前与输出提交前固定插入检查：优先返回取消，再判断总预算耗尽，随后处理让出请求或时间片耗尽，最后同时扣减总预算与时间片。慢路径返回后重新检查，尚未执行的指令只执行一次、不重复扣费；无调用及 GC 的 N 条直线 IR 指令成功需 N+1 步。源码函数逐帧计算，受管 GC 另按 [ADR-0043](adr/0043-chunked-runtime-gc.md) 每块扣一步。每个入口默认总预算 1,000,000，handler 可指定。预算不是机器指令数或 CPU 时间，检查后的迟到取消不能撤回成功调用。已发布 `0.0.2` 使用 v1，历史源码 v2–v9 均与当前 v10 拒绝混用，须使用匹配的编译器和 VM。

`start_controlled` 从每线程有界共享池借出 256 KiB 可用 guarded stack（默认 64 槽、缓存 2 槽），`start_in_pool` 可显式传入 `StackPool`；耗尽或分配失败先返回，不终止实例。池使用自研的整块 mmap、按平台页大小设置 guard page、已完成槽位缓存和 `trim` 页回收。`ScalarExecution` 拥有参数、令牌、协程，并独占借用实例，不能 Send/Sync。`resume(非零时间片步数)` 在生成代码检查点经 C ABI 慢路径暂停，保留原栈和全部标量活跃值，再从原位置恢复；完成后才提交并立即归还槽位。切换使用禁用 unwind 的 `corosensei 0.3.3`，默认栈分配器只用于对照实验。丢弃未完成 handle 会终止实例；挂起栈先收到内部取消命令、正常返回再释放，不调用强制 unwind/reset。同步 `invoke`/CLI 与 initializer 也借用池内栈，正常返回立即归还；worker 通过 `instantiate_in_pool` 将初始化栈计入进程额度，额度不足在 spawn 时回滚。池不代替 compiler 根、GC 或完整资源账本，当前栈检查见 [ADR-0024](adr/0024-scalar-stack-boundary.md)，原栈恢复与池契约见 [ADR-0011](adr/0011-scalar-stackful-resume.md) 和 [ADR-0012](adr/0012-bounded-stack-pool.md)。

`wings-vm run --trusted <目录>` 只装载自己构建的可信本机产物。**清单校验不能证明机器码安全**；共享库装载本身可能执行原生初始化代码，`--trusted` 不是沙箱。库 API 的装载入口为 `unsafe`，调用者必须保证来源、ABI 和无外部 native 回调/线程的生命周期要求。共享库由代码租约持有，不再永久泄留；OS 是否立即解除页映射由装载器决定，不提供运行中热卸载。heap CLI 使用计费加载，原标量工具入口仍不计费。

驱动从 stdin 接收最多 64 KiB 的逐行 JSON：`{"message":"Inc","args":[3]}`。Int 使用有符号 64 位 JSON 整数，Bool 使用 JSON 布尔，Unit 使用 null。请求输出 `{"reply":值}`，`-> Unit` 输出 `{"reply":null}`；单向入口成功处理后输出 `{"accepted":true}`，这是驱动确认，不是语言自动回复。输入错误或受管算术失败写 stderr 并退出非零，失败调用不提交候选状态，也不输出成功回复；此前已完成调用的输出不撤回。stdin EOF 正常退出，启动失败退出 1，命令用法错误退出 2。原型没有网络、文件或语言内 I/O。

CLI 路径没有计时线程、actor 调度、mailbox 或监督，不能运行不可信代码；`--heap-records` 可显式使用有界 RecordInt/IntList GC，宿主 worker 另见下节，资源账本仍未覆盖全部分配。原标量 AOT 在 macOS AArch64 与 Linux x86-64（glibc）均已通过 [CI 原生执行验证](https://github.com/wsafight/wings/actions/runs/43089909858)；活栈、有界池、资源守恒与 owned task 切片已另通过本分支的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43099984279)，历史结果不覆盖后续 ABI/GC/OS 唤醒变更。浮点/SIMD、全部业务根、跨线程迁移与完整受管执行模型仍未验证。原型边界见 [ADR-0009](adr/0009-scalar-aot-prototype.md)。

共享代码通过 `ScalarModule` 装载一次，再 `instantiate` 创建状态独立的实例；清单和入口表只保存一份，每个实例都执行 initializer。`HandlerId` 持有模块身份与序号，用前检查身份及参数 schema，不接受其他模块的 ID；ID 不是线协议。便利入口 `ScalarProcess::load_trusted` 仍会装载独立模块，不隐式按路径缓存。JSON 适配层只解析一次名称，参数暂存在有界数组，再直接按 ID 调用，避免原先重复查找和参数 Vec 分配。共享对象仅为只读代码元数据，不改变业务状态隔离。见 [ADR-0013](adr/0013-shared-scalar-modules.md)。

## 低层实现范围

当前 ABI v10 保留 [ADR-0024](adr/0024-scalar-stack-boundary.md) 的薄入口与静态帧准入，使用 [ADR-0025](adr/0025-aot-record-roots.md) 的 compiler shadow frame：在 safepoint 和堆回调前发布仍有后续用途的 Ref/ListRef，候选状态保活至出口，所有返回路径恢复父帧。每函数最多 64 个同时活跃根；GC 汇总最多 64 帧/64 个 compiler 根，另加一个宿主持久状态根，先完整验证，再收集。`instantiate_with_heap` 建立固定容量、三级计费堆；堆控制块按预留后 fallible 分配，实例只保留可选独占指针，执行时将所有权移入协程、返回再归还，未产生实例自引用。暂停保留链和根槽，Drop 正常取消解链；没有后台 GC 或宿主暂停期间收集接口。状态提交契约见 [ADR-0027](adr/0027-persistent-record-state.md)。

宿主调度入口 `ScalarWorker` 将模块实例化为 generation Pid，使用有界 mailbox 与计费栈池轮转执行；固定 pending 表提供异步请求及轮询 deadline。`spawn` 默认无堆，`spawn_with_heap` 在初始化前建立进程计费堆；`instantiate_with_heap` 要求传入同 process 绑定池，实例的同步调用复用该池，显式传入其他账户或未计费池则执行前拒绝。CLI 可用 `--heap-records`，不改变标量 JSON 消息协议，不将它当作语言内消息或完整受管执行。契约见 [ADR-0020](adr/0020-single-worker-mailbox.md)、[ADR-0021](adr/0021-bounded-requests.md) 与 [ADR-0026](adr/0026-language-temporary-records.md)。

可选执行计时线程只跨线程设置原子让出标志，不访问协程或单线程账户；原线程仍在强制 safepoint 正常挂起/恢复。默认关闭，Drop 撤销并 join；不额外改变当前 ABI，不作为完整公平性证明，见 [ADR-0022](adr/0022-worker-deadline-preemption.md)。

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

未发布源码提供独立的 `WorkerParker`/`WorkerNotifier`：Linux 使用 epoll/eventfd，macOS 使用 kqueue 用户事件。等待端独占、通知端可克隆，通知可合并且先通知后等待不会丢失提示。单 worker 已通过私有、计费 parker 接入就绪检查与 deadline，不导出 notifier；它不是计算抢占、跨线程 inbox 或完整 I/O reactor。底层超时与生命周期见 [ADR-0016](adr/0016-worker-wakeup.md)，集成边界见 [ADR-0041](adr/0041-worker-deadline-parking.md)，各后端须分别执行本分支 CI。

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
