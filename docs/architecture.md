# 架构设计

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

未发布源码另有 [ADR-0018](adr/0018-bounded-record-heap.md) 的独立 `RecordHeap`/`ShadowRoots` 原型：不可变定长记录按布局保存无动态标签的字段，显式根帧配合精确非移动 mark-sweep，构造前向进程/租户/VM 预留容量。它尚未接入生成代码或 ABI，不能把 Rust 侧根表验证称为 compiler shadow stack 已完成；下面的完整受管契约仍待实现。

首版采用 **wings-lang 单前端 + Cranelift AOT 后端 + 显式 shadow stack**。M0 先验证两个目标平台上的产物生成、调用约定、根发布和上下文恢复，确保每个 safepoint 的活跃值都能恢复，再优化 GC。当前仅接入下面的标量原型，完整后端仍待这一步验证。

- **不提供 `#[process]` Rust 子集前端**：proc-macro 无法自动控制依赖库内部的循环、最终寄存器分配和全部栈布局。普通 Rust 只能作为可信内建，通过受管桥接调用，且不能把普通 Rust 栈帧当作可扫描、可任意抢占的业务帧。
- **最小 ABI 先冻结**：规定 context 参数、值与错误表示、栈对齐、寄存器 clobber、根链发布顺序、返回路径和代码版本引用。GC 与切换只能发生在满足 ABI 的位置，禁止 Rust panic/unwind 跨越受管边界。
- **根枚举是精确的**：所有在 safepoint 后仍活跃的堆引用都进入有类型的 shadow stack 槽；初版不混用寄存器保守扫描。不访问原生帧的任意内存来猜测指针。
- **初版 GC 是进程内、非移动式 mark-sweep**：从业务 state、shadow stack、当前消息和受管内建的显式根开始标记，再回收不可达对象。其他进程无需为该次本地 GC 全局停顿，但共享分配器和代码加载仍可能引入同步。
- **先正确再增量**：M1 对小堆使用完整 GC，公开其不可抢占区段；M3 再按延迟数据决定分块扫描与增量回收。必须明确标记不变量、分配颜色、根变更和 runtime 写入协议，不能仅凭业务值不可变就跳过同步设计。
- **升级到 stack map 是可选优化**：只有在 shadow stack 开销成为瓶颈时，才接入后端精确元数据与全部相关寄存器位置。GC 遍历本身优先用 Rust，不为了栈扫描额外手写汇编。

是否需要为每条堆写加屏障，取决于最终 GC 算法。评测分别列出分配、根维护、扫描和屏障成本。GC 的 `<10µs` 只是在限定活数据与根数量下的候选目标，不适用于任意进程。

## 标量 AOT 原型

执行所有权有两种宿主接口：`ScalarExecution<'_>` 独占借用已有实例；`ScalarTask` 按值拥有实例与私有 invocation，无需把借用和被借用者放进自引用槽位。任务可在同线程容器内移动，挂起时不暴露可变实例；`start` 直接接收模块绑定的 `HandlerId`，忙时拒绝新调用，完成后才允许下一条。两条路径共用原栈恢复、候选提交和正常取消清理，均不能跨线程恢复。任务仍无身份、mailbox 或调度协议，细节见 [ADR-0015](adr/0015-owned-scalar-tasks.md)。

当前源码的 `wings-compile build` 将单个 process 的直线标量 handler 降为有类型、先定义后使用的寄存器 IR，校验后由 Cranelift 0.118 生成 PIC 对象，再用系统 `cc` 链接为共享库。不使用 JIT，也不执行源码完成编译或 LSP 查询。每入口最多 4096 条操作、每 process 最多 128 个 handler、每 handler 最多 64 个参数，超限拒绝构建。

输出目录必须新建，成功后包含 `manifest.json` 与 `program.dylib`（macOS）或 `program.so`（Linux）。清单上限 64 KiB，共享库上限 64 MiB；版本、标量 ABI、目标、名称、参数和回复类型在装载前检查，库路径固定，不接受清单指定的任意路径。链接失败不生成清单，可能留下不完整目录，需要使用新的输出目录重试。

未发布源码的标量 ABI v3 是 `(state: i64, args: *const i64, output: *mut ScalarOutput, context: *mut ScalarContext) -> u32`，C 调用约定；输出固定为两个连续 i64：候选状态、回复。context 按 C 布局依次保存取消标志指针、总剩余 `u64` 步数、让出标志指针、当前时间片剩余 `u64` 步数、可空慢路径函数指针、opaque 数据指针。两个标志均指向稳定且有效的 `AtomicU32`，其他线程只能原子设置；预算与数据只由执行线程访问。`Bool` 编码 0/1，`Unit` 编码 0。宿主校验参数数量和类型，产物仅在正常返回时写入输出；算术、取消、总预算耗尽通过状态码返回，不 trap 或跨边界 unwind。

后端在每条 IR 指令前与输出提交前固定插入检查：优先返回取消，再判断总预算耗尽，随后处理让出请求或时间片耗尽，最后同时扣减总预算与时间片。慢路径返回后重新检查，尚未执行的指令只执行一次、不重复扣费；N 条 IR 指令成功需 N+1 步。每个入口默认总预算 1,000,000，handler 可指定。预算不是机器指令数或 CPU 时间，检查后的迟到取消不能撤回成功调用。已发布 `0.0.2` 使用 v1，前一源码切片使用 v2，二者均与当前 v3 相互拒绝装载，须使用匹配的编译器和 VM。

`start_controlled` 从每线程有界共享池借出 256 KiB 可用 guarded stack（默认 64 槽、缓存 2 槽），`start_in_pool` 可显式传入 `StackPool`；耗尽或分配失败先返回，不终止实例。池使用自研的整块 mmap、按平台页大小设置 guard page、已完成槽位缓存和 `trim` 页回收。`ScalarExecution` 拥有参数、令牌、协程，并独占借用实例，不能 Send/Sync。`resume(非零时间片步数)` 在生成代码检查点经 C ABI 慢路径暂停，保留原栈和全部标量活跃值，再从原位置恢复；完成后才提交并立即归还槽位。切换使用禁用 unwind 的 `corosensei 0.3.3`，默认栈分配器只用于对照实验。丢弃未完成 handle 会终止实例；挂起栈先收到内部取消命令、正常返回再释放，不调用强制 unwind/reset。同步 `invoke`/CLI 不借出协程栈。池不代替编译器栈限额、GC 或三级账户，完整契约见 [ADR-0011](adr/0011-scalar-stackful-resume.md) 与 [ADR-0012](adr/0012-bounded-stack-pool.md)。

`wings-vm run --trusted <目录>` 只装载自己构建的可信本机产物。**清单校验不能证明机器码安全**；共享库装载本身可能执行原生初始化代码，`--trusted` 不是沙箱。库 API 的装载入口为 `unsafe`，调用者必须保证来源与 ABI。共享库保留到 OS 进程结束，不做热卸载。

驱动从 stdin 接收最多 64 KiB 的逐行 JSON：`{"message":"Inc","args":[3]}`。Int 使用有符号 64 位 JSON 整数，Bool 使用 JSON 布尔，Unit 使用 null。请求输出 `{"reply":值}`，`-> Unit` 输出 `{"reply":null}`；单向入口成功处理后输出 `{"accepted":true}`，这是驱动确认，不是语言自动回复。输入错误或受管算术失败写 stderr 并退出非零，失败调用不提交候选状态，也不输出成功回复；此前已完成调用的输出不撤回。stdin EOF 正常退出，启动失败退出 1，命令用法错误退出 2。原型没有网络、文件或语言内 I/O。

这条路径没有计时线程、调度、GC、mailbox、完整资源准入或监督，不能运行不可信代码。独立资源账户原语尚未覆盖运行时分配。原标量 AOT 在 macOS AArch64 与 Linux x86-64（glibc）均已通过 [CI 原生执行验证](https://github.com/wsafight/wings/actions/runs/43089909858)；活栈、有界池、资源守恒与 owned task 切片已另通过本分支的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43099984279)，该结果不覆盖后续 OS 唤醒变更。浮点/SIMD、GC 根、跨线程迁移与完整受管执行模型仍未验证。原型边界见 [ADR-0009](adr/0009-scalar-aot-prototype.md)。

共享代码通过 `ScalarModule` 装载一次，再 `instantiate` 创建状态独立的实例；清单和入口表只保存一份，每个实例都执行 initializer。`HandlerId` 持有模块身份与序号，用前检查身份及参数 schema，不接受其他模块的 ID；ID 不是线协议。便利入口 `ScalarProcess::load_trusted` 仍会装载独立模块，不隐式按路径缓存。JSON 适配层只解析一次名称，参数暂存在有界数组，再直接按 ID 调用，避免原先重复查找和参数 Vec 分配。共享对象仅为只读代码元数据，不改变业务状态隔离。见 [ADR-0013](adr/0013-shared-scalar-modules.md)。

## 低层实现范围

宿主调度入口 `ScalarWorker` 将模块实例化为 generation Pid，使用有界 mailbox 与计费栈池轮转执行；固定 pending 表提供异步请求及轮询 deadline。ABI v3 和标量语言范围不变，不将它当作语言内消息或完整受管执行。状态/失败/预算仍复用 owned task。见 [ADR-0020](adr/0020-single-worker-mailbox.md) 与 [ADR-0021](adr/0021-bounded-requests.md)。

可选执行计时线程只跨线程设置原子让出标志，不访问协程或单线程账户；原线程仍在强制 safepoint 正常挂起/恢复。默认关闭，Drop 撤销并 join；不改变 ABI v3，不作为完整公平性证明，见 [ADR-0022](adr/0022-worker-deadline-preemption.md)。

`ScalarSupervisor` 独占宿主 worker，固定子项及重启历史，按策略有界重建失败实例；每次重启重新执行 initializer、生成新 Pid，不保存失败状态、不重放消息，也不改变 ABI 或语言范围。当前仅 one-for-one 宿主接口，边界见 [ADR-0023](adr/0023-bounded-host-supervision.md)。

当前标量后端另按 [ADR-0017](adr/0017-scalar-frame-admission.md) 在生成机器码后、写产物前检查 initializer 与每个 handler 的实际编译帧，默认最大 64 KiB，库调用可降低但不可提高。超限构建失败且不创建输出目录。该值不包含 ABI setup、宿主包装和慢路径调用链，只适用于当前无递归、无业务函数调用的子集；不是完整 256 KiB 栈成本或动态剩余栈检查，不据此开放函数或业务堆。

语言、IR、调度与资源契约共用，目标 ABI、对象格式、上下文切换和 OS I/O 由平台层处理。汇编行数少不代表实现正确或容易维护。

| 模块 | 首版选择 | 验证重点 |
|---|---|---|
| 上下文切换 | 优先评估已有实现；需要时分别实现 Darwin AArch64 与 Linux x86-64 切换桩 | 栈对齐、受管调用约定、浮点/SIMD、迁移、平台控制流保护，以及调试工具兼容性 |
| safepoint 与栈限额检查 | 后端生成，受管慢路径可见 | 检查覆盖、优化后不消失、活跃值恢复与超限错误路径 |
| GC 根遍历 | Rust 遍历显式 shadow stack | 根发布完整性、内建桥接和生命周期 |
| 信号与原生栈增长 | 首版不实现 | 不依赖捕获未知 `SIGSEGV` 后继续运行 |

## 首要运行平台

未发布源码提供独立的 `WorkerParker`/`WorkerNotifier`：Linux 使用 epoll/eventfd，macOS 使用 kqueue 用户事件。等待端独占、通知端可克隆，通知可合并且先通知后等待不会丢失提示；这只是后续 worker 的 OS 等待构件，不是计算抢占或完整 I/O reactor。超时、生命周期与集成边界见 [ADR-0016](adr/0016-worker-wakeup.md)，各后端须分别执行本分支 CI。

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
