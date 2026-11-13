# Wings VM

面向高并发有状态应用的微进程虚拟机。

Wings 的目标是**在相同业务负载下，比 BEAM 更快、更省内存**。业务代码通过消息交互，各微进程保存自己的状态，VM 负责调度、内存管理和监督恢复。

- **性能**：在相同 CPU 配额和尾延迟要求下，取得更高的持续吞吐。
- **内存**：在相同状态、消息和活跃比例下，降低整机内存占用，提高单机进程密度。
- **执行模型**：各进程的业务状态相互隔离，保留公平调度、监督恢复和资源检查。

VM 用 Rust 实现，`wings-lang` 业务代码通过 Cranelift AOT 编译执行。首要运行平台是 **macOS AArch64（Apple Silicon）与 Linux x86-64**，两者都要通过完整的 VM 运行测试和性能验收。组件说明见[架构设计](docs/architecture.md)。

## 文档

- [产品定位](docs/product.md)：性能与内存目标、适用场景和采用路径。
- [架构设计](docs/architecture.md)：技术选型、编译后端、受管 ABI、GC 和平台范围。
- [Crate 拆分](docs/crates.md)：workspace 依赖、`wings-vm` 与编译工具的边界，以及 `wings-lsp` 方案。
- [架构决策](docs/adr/README.md)：当前已采纳的设计、替代方案与重审条件。
- [运行时](docs/runtime.md)：微进程、内存、调度、消息、资源额度、监督和热更新。
- [语言设计](docs/language.md)：`wings-lang` 的 handler、状态和异步交互语义。
- [激进优化](docs/optimizations.md)：编译、值布局、分配、消息、调度、GC 与平台专用优化。
- [评测与验收](docs/evaluation.md)：BEAM 对照、负载、指标和故障注入。
- [风险与里程碑](docs/roadmap.md)：已知风险、M0–M7 的范围和验收条件。

完整阅读顺序见[文档目录](docs.md)。参与修改前请阅读[协作约定](AGENTS.md)。

## 当前进度

处于 M0：已有前端与单文件 LSP，可信本机 AOT 支持标量/RecordInt/IntList 分支、比较、state while、有界非递归 fn 调用帧与临时/持久记录及列表。另有单 worker mailbox、异步请求、监督与强制检查点活栈恢复；已接入自发送及同 worker 同模块创建/单向发送、异步整数请求事件，已有 RecordInt 语言消息复制，尚无列表消息/引用回复、递归/动态派发或 M:N。历史 Counter 双平台 CI 不覆盖后续扩展，完整 M0 与同业务 BEAM 性能/内存目标尚未验证。更新日期：2026-11-13。

## 开发

ABI v11 按 [ADR-0049](docs/adr/0049-record-message-copies.md) 允许语言消息参数携带 RecordInt：发送点复制 Int 字段，目标堆重新分配并以入口根保活；源失败不撤回已接受消息。示例 `examples/record-messages.wings` 及 `record_messages`；JSON/宿主/ingress 不接受原 handle，列表消息和引用回复仍不支持。

受限 TCP Counter 示例使用真实 loopback socket、固定 32 B 帧及一个有预算的适配线程，业务仍在原 worker 执行；连接/帧数和 I/O deadline 均有限，见 [ADR-0048](docs/adr/0048-bounded-loopback-service.md)。运行：`cargo run --locked -p wings-compile -- build examples/tcp-counter.wings -o target/tcp-counter`，再 `cargo run --locked -p wings-vm --example tcp_counter -- --trusted target/tcp-counter 1 4`。它只输出临时监听地址并等待有限请求，不是生产网络 VM 或 BEAM 等价性能负载。

`ScalarWorker::open_request_ingress` 可将单个外部入口绑定为 Int/Bool/Unit 请求协议；发送端取得唯一 `IngressRequest`，固定结果表在取走结果且消息完成后才释放槽位。超时/取消不撤回已接收消息；示例 `worker_ingress_requests`，见 [ADR-0047](docs/adr/0047-cross-thread-ingress-requests.md)。仍不提供网络服务或语言阻塞请求。

宿主可在配置等待后用 `ScalarWorker::open_ingress` 注册单 actor 单入口的固定跨线程队列，发送端只传 Int/Bool/Unit，并以原票据转入 mailbox；关闭后旧 sender 拒绝，最后 sender 释放前保留控制与通知预算。示例 `worker_ingress`，契约见 [ADR-0046](docs/adr/0046-bounded-cross-thread-ingress.md)。这不是网络服务、引用消息或 M:N。

资源账户与预留票据支持跨线程共享/归还，同 VM 的三级四维准入和净转移通过一个门闩串行提交，失败不留半账；见 [ADR-0045](docs/adr/0045-concurrent-resource-admission.md)。actor/活栈仍绑定原线程，这不是 M:N，也没有无锁或吞吐领先结论。

`ScalarModule::load_accounted(directory, vm)` 在解析/装载前预留 VM 级模块逻辑预算，同模块实例共享一次预留；Library 随最后代码租约交还，挂起调用先正常取消。heap CLI 使用该路径，旧 `load_trusted` 工具入口不计费，限制见 [ADR-0044](docs/adr/0044-native-module-leases.md)。这不是热更新或精确 RSS 计量。

受管 GC 每 16 个节点/根检查预算、取消与让出，暂停后从原 phase 继续；总预算包含 GC 工作单位，堆统计不扫描全堆。示例 `examples/chunked-gc.wings`，限制见 [ADR-0043](docs/adr/0043-chunked-runtime-gc.md)；尚无块耗时或同业务性能达标结果。

宿主 `ScalarWorker::enable_wait()` 为 OS 等待端预留 4096 字节控制预算；`wait(max_wait)` 重查 mailbox、挂起入口、请求 deadline 及有界 ingress，不执行 handler，就绪时立即返回。等待边界见 [ADR-0041](docs/adr/0041-worker-deadline-parking.md)，ADR-0046 补充 ingress 通知租约；它不提供完整 I/O reactor。

`request(pid, Message, Completed, timeout_ms, args...)` 在调用点准入并返回局部 RequestId，结果异步到达 `on Completed(id: RequestId, status: Int, value: Int)`，status 的 0/1/2/3 表示回复/超时/取消/失败。当前只接受 Int 回复；队满保留预付费完成事件，不覆盖已终结结果。`cancel_request` 不撤回目标消息。示例 `examples/request-events.wings`、`examples/request-roots.wings`，限制见 [ADR-0040](docs/adr/0040-language-request-events.md)。

`self_pid()` 与 `spawn_self()` 返回局部 `Pid`，`send(pid, Message, args...)` 静态检查本模块单向入口；子实例继承资源配置、拥有新状态，父失败不撤回已创建子项。示例 `examples/local-processes.wings`、`examples/local-process-roots.wings`，见 [ADR-0039](docs/adr/0039-local-process-capabilities.md)。Pid 暂不进入 state/消息/回复，无跨 worker 投递或隐式监督；宿主 `pids()` 可无分配迭代现有实例。

未发布源码 ABI v10 支持 handler 内 `send_self(Message, args...)`：目标为本 process 单向入口，worker 在调用点准入入队，下一片恢复后继续；失败不撤回此前发送。示例 `examples/self-send.wings` 与 `examples/self-send-roots.wings`，见 [ADR-0038](docs/adr/0038-self-send-effects.md)。CLI 与独立实例无 worker 回调，调用受管失败；旧 v9 产物须重编译。

`IntList` 是不可变整数节点和共享尾，可作 state、局部或 fn 值，仍禁止引用消息。`empty_ints` / `prepend_int` / `head_int` / `tail_ints` / `is_empty_ints` 每次只处理一个节点，使用同一计费堆与传递 GC；空读取受管失败。示例 `examples/lists.wings`，见 [ADR-0037](docs/adr/0037-immutable-int-lists.md)。

process 内可声明 `fn next(value: RecordInt) -> RecordInt { record_int(read_int(value) + 1) }`。函数只访问参数/局部和更早函数，禁止递归、state 访问和 while；私有受管调用逐帧检查剩余栈，保留跨帧精确根和强制预算检查。最多 32 层函数链、64 个聚合根，示例 `examples/functions.wings`、限制和验证见 [ADR-0042](docs/adr/0042-managed-function-frames.md)。

宿主 `ScalarWorker::snapshot` / `ScalarSupervisor::snapshot` 可查询排队、当前入口、已提交标量状态、进程用量及最近安全点预算/GC 统计；不推进执行或暴露记录 handle。`ScalarTask` / `ScalarExecution` 保留最近入口的只读执行快照。挂起统计不是后台实时采样，边界见 [ADR-0032](docs/adr/0032-safe-point-diagnostics.md)；验证命令仍为 workspace 测试及 debug/release AOT 脚本。

`ExecutionSnapshot::fault` 给出 ABI 故障或输出校验类别；worker/supervisor 的 `last_exit` 保留最近一次受管失败的固定快照，旧 Pid 失效或重启后仍可定位 handler 和预算/GC。快照不保留旧资源，账目是清理前值，不是泄漏量，见 [ADR-0035](docs/adr/0035-bounded-exit-diagnostics.md)。

未发布源码支持 `if flag { expression } else { expression }`，只执行选中臂，条件为 Bool、两臂为同类型标量、RecordInt 或 IntList；臂内暂不支持语句。示例为 `examples/branches.wings` 与 `examples/record-branches.wings`，实际路径预算和根边界见 [ADR-0029](docs/adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](docs/adr/0033-record-conditional-roots.md)。

条件可使用标量 `==`/`!=` 或有符号 Int 的 `<`/`<=`/`>`/`>=`；比较不转换类型或比较记录 handle。示例为 `examples/comparisons.wings`，契约见 [ADR-0030](docs/adr/0030-scalar-comparisons.md)。

标量、RecordInt 或 IntList state 的 handler 可用 `while`，每轮更新候选状态、正常返回才提交；循环体局部不逃逸，空循环也有强制预算/让出检查。尚无 break/continue 或任意引用 phi，见 `examples/loops.wings`、`examples/record-loops.wings`、[ADR-0031](docs/adr/0031-scalar-state-loops.md) 和 [ADR-0034](docs/adr/0034-record-state-loops.md)。

需要 Rust 1.85 或更新版本；构建 AOT 原型还需系统 C 链接工具链（`cc`，也可用 `CC` 指定编译器可执行路径）。检查命令只验证当前语法子集，不生成机器码：

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p wings-compile -- check examples/counter.wings
cargo run -p wings-lsp
```

当前源码可额外构建并运行标量 AOT 示例。输出目录必须不存在；`--trusted` 表示只装载自己构建、可信且与清单匹配的本机原生代码，不是安全沙箱：

```sh
cargo run -p wings-compile -- build examples/counter.wings -o target/counter-aot
cargo run -p wings-vm -- run --trusted target/counter-aot < examples/counter.messages.jsonl
```

Rust 自动化回归运行 `cargo test --workspace --locked`，覆盖前端、LSP、编译与产物校验，以及 VM 执行、资源账户、记录堆、活栈和宿主 worker 边界。真实生成代码的端到端边界验证另行执行：`cargo build --locked -p wings-compile -p wings-vm --bins --examples`，再运行 `python3 scripts/verify_scalar_aot.py`（需要 Python 3）；release 构建增加 `--release`，脚本加 `WINGS_PROFILE=release`。两层验证互不替代。

此 CLI 路径验证原生执行与状态提交，不驱动宿主 mailbox 或监督。资源账户已接入堆、栈及有限宿主 worker，但未覆盖全部运行时分配。产物与驱动协议见[架构原型说明](docs/architecture.md#标量-aot-原型)，不将成功执行示例视为完整 VM 已交付。

LSP 使用 stdio、UTF-16 位置和整份文档同步，支持诊断、局部跳转、悬停与基础补全。目前逐份文档同步分析，尚无跨文件解析、增量失效或运行中取消；双平台 CI 覆盖构建，LSP 协议冒烟仍仅在本机 macOS AArch64 验证。语言支持范围见[语言设计](docs/language.md)。

未发布源码另有执行控制：每条 IR 指令及输出提交前强制检查原子取消/让出标志和预算。`wings-vm run --trusted <目录> --max-steps <u64>` 可设置每次 handler 的总上限（默认 1,000,000）。库接口 `start_controlled` 返回独占实例的 `ScalarExecution`，可按步数时间片在同线程让出、保留原栈恢复；取消、总预算或栈空间耗尽会终止，不提交该次状态与回复。同步调用、initializer 与挂起调用统一使用固定 256 KiB 可用栈；薄入口在业务帧前检查剩余空间，worker 初始化栈也计入进程额度，池满或执行前准入失败显式拒绝。Drop 通过正常取消返回清理，不跨受管 ABI unwind。这不是完整调度器、GC 或三级资源账本；源码 ABI v10 只装载匹配源码编译器产物，不兼容已发布 `0.0.2` 的 v1 及旧源码 v2–v9 产物，须重新编译。原栈恢复见 [ADR-0011](docs/adr/0011-scalar-stackful-resume.md)，剩余栈检查见 [ADR-0024](docs/adr/0024-scalar-stack-boundary.md)，根链与持久状态见 [ADR-0025](docs/adr/0025-aot-record-roots.md) 和 [ADR-0027](docs/adr/0027-persistent-record-state.md)。

原栈分配现由自研 `StackPool` 管理：默认每线程最多 64 个活跃槽位、缓存 2 个已完成栈，耗尽明确失败；可显式传入池并读取容量统计、调用 `trim` 回收缓存页。完成调用立即归还槽位，挂起栈不能复用。此优化主要约束容量和缓存，不消除同时挂起栈的物理成本。实验与限制见[栈分配实验](docs/evaluation.md#原生栈分配实验)。

`ScalarModule` 可装载一次并反复实例化，共享只读清单和入口数组，各实例独立执行初始化、保存状态与失败标记。宿主可预解析模块绑定的 `HandlerId`，直接调用时不再按字符串查找；外部 JSON 协议不变。独立实例自身没有 mailbox，worker 中的语言创建/发送另见 ADR-0039；模块共享契约见 [ADR-0013](docs/adr/0013-shared-scalar-modules.md)。

宿主可用 `ScalarTask` 按值保存实例和未完成调用，在同线程内移动而无需自引用；忙时拒绝新调用，正常结束才提交并释放栈，见 [ADR-0015](docs/adr/0015-owned-scalar-tasks.md)。`ScalarWorker` 在此基础上提供有限进程表、generation Pid、mailbox、固定 pending 表的异步请求及手动轮转时间片；process、消息、请求和活栈经过三级准入。超时/取消不撤回已接受消息，迟到回复不能覆盖复用请求槽位。该切片新增语言自发送及同模块创建/发送及异步整数请求事件；ADR-0048 另有受限 loopback I/O 示例，仍不是 M:N 调度器，见 [ADR-0020](docs/adr/0020-single-worker-mailbox.md) 与 [ADR-0021](docs/adr/0021-bounded-requests.md)。

worker 可选 `enable_timer` 后用 `tick_timed` 按单调 deadline 请求让出，仍在强制 safepoint 挂起、原线程恢复。默认不创建计时线程，未验证真实服务尾延迟或 M:N 扩展成本，见 [ADR-0022](docs/adr/0022-worker-deadline-preemption.md)。

`ScalarSupervisor` 可独占单 worker 做受限 one-for-one 恢复：失败后新实例重新初始化、使用新 Pid，保留旧请求结果但不重放消息；窗口内重启达到上限或重建失败即停止。显式 `spawn_with_heap` 以固定槽数创建及重建全新计费堆，不保留旧记录状态；普通 spawn 仍无堆。主动 stop 不重启，没有语言级 supervisor、link/monitor 或 OTP 兼容承诺，见 [ADR-0023](docs/adr/0023-bounded-host-supervision.md) 和 [ADR-0028](docs/adr/0028-supervised-record-heaps.md)。

语言局部与 state 均可用 `RecordInt`，受限内建为 `record_int(value)`、`read_int(record)`、`collect()`；记录不可变，不支持字段赋值，参数和回复仍只允许标量。compiler 发布活跃引用及候选状态，宿主根独立保活已提交状态。CLI 用 `--heap-records 1..=1024` 显式配置堆，并使用固定 16 MiB 的三级字节额度；宿主 `instantiate_with_heap` 与 worker `spawn_with_heap` 允许显式配置。引用状态无堆时在初始化前拒绝，临时内建无堆时受管失败；同步及显式执行均保持同 process 计费栈池。见 [ADR-0026](docs/adr/0026-language-temporary-records.md) 和 [ADR-0027](docs/adr/0027-persistent-record-state.md)，可运行示例为 `examples/records.wings` 与 `examples/record-state.wings`。

```sh
cargo run -p wings-compile -- build examples/records.wings -o target/records-aot
printf '%s\n' '{"message":"Work","args":[11]}' | cargo run -p wings-vm -- run --trusted target/records-aot --heap-records 2
```

MVP 的待办和验收顺序见[路线图](docs/roadmap.md#mvp-范围与开发顺序)：先完成活栈、精确根和资源边界，再推进消息、调度、恢复与双平台 BEAM 对照，不以示例运行成功代替可试用验收。

当前 workspace 版本为 `0.0.2`，采用 [Apache-2.0](LICENSE) 许可证。全部十个已有实现的包均已发布，包括标量 AOT 后端与可信装载原型，不代表完整 VM 已交付。发布顺序、状态和名称检查见[发布说明](docs/publishing.md)。

crates.io 的 `0.0.1` 是原前端版本，仅有 `check` 和 LSP；不能覆盖该版本。`0.0.2` 增加 AOT 构建与可信执行入口，按 PR 合并、CI 验证、手动发布的顺序交付；PR 本身不触发包上传。

`wings` 已由其他项目使用，本项目不发布同名聚合包。`wings-lang` 仅作语言名，不是待发布的 crate；项目名与商标核查仍未完成。名称约定见 [ADR-0008](docs/adr/0008-wings-naming.md)。

## 安装

可从 crates.io 安装 `0.0.2` 的编译命令、标量执行原型和语言服务器（构建 AOT 还需系统 C 工具链）：

```sh
CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/ cargo install wings-compile wings-vm wings-lsp --version 0.0.2 --locked --registry crates-io
wings-compile check examples/counter.wings
wings-compile build examples/counter.wings -o target/installed-counter-aot
wings-vm run --trusted target/installed-counter-aot < examples/counter.messages.jsonl
wings-lsp --help
```

示例路径相对于本仓库，输出目录必须不存在；检查命令只做静态检查，LSP 通过 stdio 启动，不执行用户程序。`wings-vm` 仅装载可信本机产物，不是安全沙箱。
