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

处于 M0：已有前端与单文件 LSP，可信本机 AOT 支持标量/RecordInt/IntList/Bytes/String 和具名不可变记录，覆盖分支、state while、有界非递归 fn 调用帧与精确 GC。String/Bytes 是 0..=64 KiB 的计费值；具名记录支持嵌套字段、本地/state/fn、消息参数和 ADR-0116 的有界独立回复，回复完成事件按名义 schema 重建。已有单 worker mailbox、异步请求、监督与强制检查点活栈恢复；另有固定多 worker 调度切片，提供稳定 Pid、跨 worker 消息/请求、handler 返回后的迁移及宿主有界监督重启。字符串索引/切片、递归/动态派发和语言级监督仍未完成。历史 Counter 双平台 CI 不覆盖后续扩展，完整资源账本、M0 与同业务 BEAM 性能/内存门禁尚未验证。更新日期：2026-11-18。

## 开发

[ADR-0122](docs/adr/0122-bounded-handler-lookup-diagnostics.md) 修复宿主未知 handler 名称放大错误分配：沿清单/CLI 的同一名称规则预检，非法名固定诊断。真实满额回归及修复前 3 MiB 大分配证据见[验证记录](docs/validation-handler-lookup-2026-11-18.md)，不将固定有界错误当作完整资源账本。

[ADR-0121](docs/adr/0121-persistent-cancellation-recovery.md) 将超时/取消、stop 竞争和一次监督重启接入持续 Scheduler 验证，一/双 worker 的普通三轮与显式 30s/60s 长测分开。构建与实际结果见[恢复验证记录](docs/validation-recovery-reuse-2026-11-18.md)，尚非完整故障或 MVP 门禁。

[ADR-0120](docs/adr/0120-persistent-concurrent-failure-soak.md) 增加独立持续失败生命周期入口：同一 VM/Scheduler 内逐轮复用槽，两个发送者竞争失败、健康目标推进，检查完整请求前缀、旧身份拒绝和计费缓存。默认完整 AOT 只验证三轮；显式 `measure_failure_soak.py` 才跑 30s 预热/60s 采样，实际结果见[验证记录](docs/validation-failure-soak-2026-11-18.md)。未覆盖长时取消/自动恢复或完整 MVP 门禁。

[ADR-0118](docs/adr/0118-native-sync-storage-admission.md) 修复 macOS 标准同步对象首次使用才分配原生存储的计费缺口：根启动增加实际 mutex 后备字节，共享同步对象在准入后、发布前初始化；监督退出票据也覆盖该存储。本机根基线为 232 B，历史冻结回执不回写，原额度不变；验证见[同步计费记录](docs/validation-native-sync-accounting-2026-11-18.md)。完整 MVP 门禁仍待完成。

[ADR-0115](docs/adr/0115-typed-cross-thread-ingress.md) 为固定跨线程入口补 `send_values`/`request_values`，独立复制 String/Bytes/IntList/RecordInt 等叶值，目标沿原计费/GC/分块导入；原标量 API、容量及超时不撤回语义不变。构建后用 `target/debug/examples/typed_ingress --trusted target/typed-ingress-aot` 验证，产物先由 `target/debug/wings-compile build examples/typed-ingress.wings -o target/typed-ingress-aot` 生成；这不是完整 reactor 或 MVP。

[ADR-0117](docs/adr/0117-streamed-cli-record-replies.md) 将具名记录回复接入 JSON CLI：记录按字段输出对象，嵌套/空记录保留形状，叶值沿原回复格式。输出直接写入受控 writer，固定遍历控制纳入原 adapter 预算；JSON 参数仍限标量。验证见[记录输出验证](docs/validation-cli-record-replies-2026-11-18.md)，MVP 其余门禁仍未完成。

[ADR-0116](docs/adr/0116-bounded-named-record-replies.md) 开放具名记录 `-> Reply`，以名义 schema 校验并独立复制有界叶值；语言请求完成事件按声明 schema 重建，ABI v21/执行预留与既有限额不变。单机功能验证不等于双平台 BEAM 性能与整机内存门禁。

[ADR-0114](docs/adr/0114-scalar-process-account-binding.md) 修复纯标量实例可通过换执行池或迁移镜像改借其他账户的问题。计费初始化后始终绑定原 process，同账户换池/线程仍允许；拒绝不执行业务或消费镜像，ABI、原控制预留及限额不变，仍非完整 MVP。

[ADR-0113](docs/adr/0113-bounded-named-record-message-expansion.md) 补具名记录单向/请求消息参数，按字段有界展开并在目标独立重建，保留计费复制、精确根及所有原限。ABI v21 不变；回复已由 ADR-0116 补充，完整 MVP 门禁仍待完成。

[ADR-0111](docs/adr/0111-bounded-string-content-equality.md) 补 String 完整内容 `==`/`!=`，不按引用身份、不做规范化；分块检查预算/取消/让出。源码 ABI v21，旧产物须重编译，原布局/控制预留不变；MVP 其他门禁仍待完成。

全目标测试使用 `python3 scripts/verify_workspace.py --output target/workspace-validation/debug-new`；release 加 `--profile release`，目录必须全新。每个 harness 受原 CPU/墙钟限制，保留指纹和失败日志，不把只编译计作通过。doctest 和真实 AOT 另跑，见[评测文档](docs/evaluation.md#可复现全量测试)。

[ADR-0109](docs/adr/0109-accounted-wakeup-endpoint-leases.md) 修复唤醒通知者比 worker 存活更久时提前退账：`WorkerParker::new_accounted` 在 OS 创建前准入，最后端点 owner 关闭 FD 后归还原 4096 B，worker/ingress 不重复计费。ABI v20、执行预算和时限不变，仍非完整账本或 MVP。

[ADR-0108](docs/adr/0108-bounded-named-record-values.md) 接入本地/state/fn 有界具名记录及名义类型检查，每 process 最多 32 种、每种最多 8 字段；引用字段由原精确 GC 跟踪，实例布局表先计费，失败不提交候选状态。源码 ABI v20，旧产物须重编译，控制预留不变；示例见 [`examples/named-records.wings`](examples/named-records.wings)。具名记录传输、完整账本与原生双平台/BEAM 双门禁仍待完成。

[ADR-0107](docs/adr/0107-accounted-control-token-leases.md) 补取消/让出 token 的显式计费构造、常驻 worker/actor 接入与末 owner 清理。ABI v19 和原控制预留不变，Default 宿主工具入口不代表完整资源计费；MVP 仍未完成。

[ADR-0106](docs/adr/0106-accounted-string-replies.md) 补齐 String 内容回复、类型匹配完成事件及宿主 `StringReply`，出口副本/结果控制和目标导入均计费，CLI 支持结构化文字回复。源码 ABI v19、布局和控制预留不变；具体命令与失败见[验证记录](docs/validation-string-replies-2026-11-17.md)。通用记录、完整账本与原生双平台/BEAM 双门禁仍待完成。

[ADR-0105](docs/adr/0105-accounted-string-messages.md) 接入 String 语言/宿主消息参数，独立类型复制与 Bytes 共用单条 64 KiB 上限；源码 ABI v18、EffectRequest 80 B，旧产物须重编译。该切片当时 String 回复未开放，完整账本、原生双平台及 BEAM 双门禁仍待验收；本轮命令与结果见[验证记录](docs/validation-string-messages-2026-11-17.md)。

[ADR-0104](docs/adr/0104-bounded-string-literals.md) 补齐有界 UTF-8 字符串字面量，支持直接中文/emoji 与六种显式转义；编译到只读数据段，执行时分块复制为独立计费 String。该切片源码 ABI v17，旧产物须重编译；当时仍无 String 消息/回复或通用记录。验证与未通过项见[记录](docs/validation-string-literals-2026-11-17.md)，不因本地功能通过宣布 MVP 达标。

[ADR-0103](docs/adr/0103-accounted-utf8-string-values.md) 补 MVP 业务值缺口：独立计费 String、分块 UTF-8 校验/复制、精确 GC 及失败清理。源码 ABI 为 v16，旧产物须重编译；可运行示例见 [`examples/string-values.wings`](examples/string-values.wings)，构建/验证与剩余门禁见[验证记录](docs/validation-string-values-2026-11-17.md)。本轮优先补功能，不宣称完整 MVP 或 BEAM 性能/内存达标。

[ADR-0102](docs/adr/0102-opt-in-worker-lock-statistics.md) 增加默认关闭的 worker 四类锁区间统计，可用 `SchedulerConfig::lock_statistics` 启用并通过 handle 读取。仅记录固定计费的墙钟累计/最大值，不是 CPU、p99 或性能提升结论；构建与验证命令见[记录](docs/validation-lock-statistics-2026-11-17.md)。

[ADR-0099](docs/adr/0099-unready-result-poll-fast-path.md) 为宿主未就绪结果轮询增加计费原子快路径，保留原期限和锁内终态。mixed-4k 原速率五对探索性前后比较中，p99 四对、p999 五对降低，无超时/拒绝；没有内存下降或稳定全场景加速结论，见[优化验证记录](docs/validation-optimization-2026-11-16.md)。

[ADR-0098](docs/adr/0098-accounted-byte-replies.md) 补齐 0..=64 KiB Bytes 回复：原协程分块导出并保留出口根，独立结果存储与控制先计费，再提交候选状态。宿主使用 `invoke_bytes`、`CompleteBytes` 或 `ReplyBytes`；完成事件在发起方堆分块导入。示例见 [`examples/bytes-replies.wings`](examples/bytes-replies.wings)，验证证据及剩余 MVP 门禁见[验证记录](docs/validation-2026-11-16.md)。

[ADR-0097](docs/adr/0097-accounted-int-list-replies.md) 接入有界 IntList 回复：原协程保留出口根并逐节点检查，复制为独立计费的 `IntListReply` 后才提交候选状态。宿主使用 `invoke_list`、`CompleteList` 或 `ReplyList` 取得拥有的结果，JSON 输出 `{"reply":{"int_list":[...]}}`；非成功语言完成事件为空列表。示例见 [`examples/list-replies.wings`](examples/list-replies.wings)，平台证据见[验证记录](docs/validation-2026-11-15.md)，Bytes 回复由 ADR-0098 补齐，通用业务值回复仍未完成。

[ADR-0096](docs/adr/0096-copied-record-replies.md) 接入现有 RecordInt 的内容复制回复，目标验证先于状态提交，语言完成入口在发起方计费堆导入记录。宿主结果只持 i64 字段副本，JSON 输出带 record_int 字段；仍为 ABI v15，不改变语法拼写。真实示例为 [`examples/record-replies.wings`](examples/record-replies.wings) 与 [`examples/record-reply-state.wings`](examples/record-reply-state.wings)，具体平台证据见[验证记录](docs/validation-2026-11-14.md)。

[ADR-0095](docs/adr/0095-typed-scalar-request-events.md) 将语言异步请求扩展为 Int/Bool/Unit 回复，完成入口第三参数精确匹配目标回复，系统事件仍不能伪造。真实示例见 [`examples/scalar-completions.wings`](examples/scalar-completions.wings)，沿用既有语法、ABI v15 和请求生命周期；其他引用回复和完整 MVP 门禁仍未完成。

[ADR-0094](docs/adr/0094-typed-host-message-values.md) 为 ScalarWorker、ScalarSupervisor 和 SchedulerHandle 增加 `send_values`/`request_values`，使用 `MessageValue` 传入独立的记录内容、列表和字节副本。完整受限 AOT 已接入 [`examples/host-values.wings`](examples/host-values.wings)，宿主用法见[运行时](docs/runtime.md)。原始整数/JSON/IngressSender 及回复仍遵守原边界；语法评审稿尚未采纳。

[ADR-0093](docs/adr/0093-scheduler-ready-before-park.md) 修正 Scheduler 空认领与等待之间的就绪发布竞争，保留原准入失败退避；另补非零 deadline 的发布/等待/取消竞争验证，不改变请求期限或强制检查。

[ADR-0092](docs/adr/0092-concurrent-failure-counter.md) 增加双发送者失败前缀的独立 Wings/BEAM 对照，验证候选不提交、每条请求终态与健康目标推进；不改变 ABI 或语法，超时/恢复、原生双平台和性能/内存门禁仍须独立验收。

[ADR-0091](docs/adr/0091-manifest-string-preflight.md) 在产物文件解码前检查全部 JSON 字符串/键的既有 256 字节和字符边界，避免长输入先放大为类型错误；原格式、语义检查和模块预留不变，其他错误来源与完整账本仍须收敛。

[ADR-0090](docs/adr/0090-bounded-cli-reporting.md) 为 CLI 控制输出从原 VM 额度预留 8 KiB，帮助/诊断使用独立描述符且断管正常失败；诊断最多 4096 B，来源错误构造与完整宿主账本仍须收敛。

[ADR-0089](docs/adr/0089-bounded-cli-startup-arguments.md) 为 CLI 启动参数预留 20 KiB，借用原生 argv 并限制最多七个参数、含 NUL 总计 16 KiB；只复制目录字节，非 UTF-8 参数沿正常错误路径处理，不调用会提前复制全部参数的迭代器。

[ADR-0088](docs/adr/0088-owned-cli-stdio.md) 为 CLI 业务 stdin/stdout 预留 12 KiB，持有独立缓冲与 close-on-exec 描述符副本，先清理再归还额度；回复仍逐条 flush，启动参数和完整宿主账本仍待收敛。

[ADR-0087](docs/adr/0087-accounted-beam-workload-roots.md) 将六个 BEAM 对照驱动接入计费根，并报告启动/清理后字节；新采集严格校验回执，历史冻结记录保持原样，完整性能门禁仍未通过。

[ADR-0086](docs/adr/0086-accounted-cli-json.md) 将 CLI 行缓冲、JSON 解析暂存和取消控制纳入原 16 MiB 三级额度，使用固定参数解析与回复结构；重复字段和嵌套参数在执行前拒绝，原行/参数上限不变。

[ADR-0085](docs/adr/0085-module-path-admission.md) 将原 2 MiB 模块控制预留前移到路径解析之前，库页随后按原公式追加；总预算和可信装载边界不变，失败归还两部分预留。

[ADR-0084](docs/adr/0084-accounted-root-module-admission.md) 要求计费根接入的模块具有本 VM 的计费租约；未计费工具模块在初始化、调用、spawn 或镜像接回前拒绝，独立工具路径仍可使用。

[ADR-0083](docs/adr/0083-accounted-module-execution.md) 禁止计费模块通过默认/显式未计费池执行或接回镜像；调用者须配置本 VM 的 process 池。拒绝不消耗镜像或改变旧状态，独立 `load_trusted` 工具入口仍保留原边界，ABI v15 与原资源上限不变。

[ADR-0082](docs/adr/0082-bounded-tcp-segments.md) 修正 TCP 设置缓冲后仍可能增长的问题：macOS 已接受连接先限制 MSS，再设原缓冲大小；每次读写后继续检查实际容量。原资源/时限不变，新增设置错误、缓冲拒绝和实际 MSS 报告；配置前内核瞬时占用仍不在该上限保证内。

[ADR-0081](docs/adr/0081-accounted-default-cli.md) 将 CLI 默认标量模式接入与堆模式相同的计费根、模块加载及单槽栈池。两种模式均使用原 16 MiB 三级逻辑字节额度，超额模块在解析/装载前拒绝；JSON/标准 I/O 适配层及其余分配仍待闭环。

[ADR-0080](docs/adr/0080-central-execution-admission.md) 将初始化和所有计费执行入口的控制预留统一到核心，补直接调用漏计并删除 worker 重复票据。标量实例保留显式池，控制额度不足先拒绝，原 4352 字节和 ABI v15 不变；`accounted_stacks` 随完整受限 AOT 回归。

[ADR-0079](docs/adr/0079-accounted-vm-bootstrap.md) 为 VM 根与准入门闩增加启动计费入口，并强制该树的子节点预留控制存储。CLI 堆模式与 TCP 使用此入口；业务清理回到存活根基线。`vm_bootstrap` 已接完整 AOT，旧工具模型及其余资源边界仍待完成。

[ADR-0078](docs/adr/0078-request-publisher-lifetimes.md) 修正内部请求资源生命周期：超时/取消结果取走、完成事件入队或发起方退出后，仍排队/执行的目标消息继续占请求额度，两端都结束才回收槽位。原容量、类型拼写和 ABI v15 不变；回归随 workspace 测试与完整受限 AOT 执行。

[ADR-0077](docs/adr/0077-streamed-fairness-measurement.md) 增加独立公平性长测：连续 CPU 与突发心跳的 30s/60s 五对采样，逐条流式事件、每 actor 延迟及可选让出时间戳。原短测边界不变；运行方式见 [v8 协议](docs/beam-workloads.md#v8连续-cpu-与突发心跳长测)，本机结果含队满和观测缺样，完整保留在[验证记录](docs/validation-2026-11-14.md)，不构成公平性或 MVP 门禁通过。

[ADR-0076](docs/adr/0076-accounted-byte-messages.md) 为现有语言消息开放 Bytes 复制参数，单条合计至多 64 KiB，发送暂存/目标导入均计费并分块检查。源码 ABI v15，旧产物须重编译；类型拼写不变，不采纳待审语法稿。构建 `examples/byte-messages.wings` 后，可用 `cargo run -p wings-vm --example byte_messages -- --trusted <产物目录>` 验证，完整 AOT 也包含该回归。

[ADR-0075](docs/adr/0075-platform-memory-observation.md) 为长测补 Linux PSS、macOS physical footprint 和主机压缩/swap 原始观测，分口径保存身份、缺失与采样器成本。`python3 scripts/verify_memory.py` 执行本机有界验证；不等于完整内存或 BEAM 性能门禁。

[ADR-0074](docs/adr/0074-bounded-concurrent-recovery.md) 补一/双 worker 真实 AOT 的并发取消、超时保留队列、stop 竞争与监督恢复。沿用原固定容量和时限，不把取消结果当作消息撤回，不替代完整故障或性能门禁。

[ADR-0073](docs/adr/0073-reusable-initializer-stack-pool.md) 增加宿主批量创建时显式复用计费初始化栈池，减少重复 mapping；不改变默认 spawn、自动重启或线程所有权。v2 原规模与时限不变，实际结果见[验证记录](docs/validation-2026-11-14.md)，不据此宣布性能或 MVP 达标。

[ADR-0072](docs/adr/0072-scheduler-yield-observation.md) 增加 Scheduler 可选固定容量让出样本与请求/清除/丢样计数。`verify_fairness.py` 在原参数下另跑 `--observe`；默认关闭，短测零样本明确为无数据，不把独立观测或样本缺失当作公平性达标。

[ADR-0071](docs/adr/0071-observed-atomic-yield.md) 增加可选计费的原子请求到真实 coroutine 返回时间观测，默认关闭。`verify_fairness.py` 同时核对独立 AOT fixture 的预请求/跨线程请求、状态与资源清理；未接为 Scheduler 长时公平性统计，不据此宣布让出延迟达标。

[ADR-0070](docs/adr/0070-bounded-fairness-heartbeats.md) 增加同 worker 百万次 checked 加法与 100 actor 突发心跳的短时业务核对。构建后 `python3 scripts/verify_fairness.py --compare-beam` 显式执行三端；保留逐条拒绝/超时及每 actor 等待，不等同单核 CPU 配额、让出延迟或长时公平性门禁。

[ADR-0069](docs/adr/0069-streamed-open-loop-measurement.md) 增加独立长时 runner：固定 30 秒预热、60 秒采样、五次配对重复，冻结输入并保留原始事件，精确计算分位和拒绝/超时。入口为 `WINGS_PROFILE=release python3 scripts/measure_open_loop.py --help`；尚缺 CPU 硬配额、完整平台内存口径及原生双平台，不能据此宣布 BEAM 双目标或 MVP 通过。

[ADR-0068](docs/adr/0068-bounded-open-loop-counter.md) 接入四组代表负载的短时计划开环与逐条迟发/拒绝/超时记账。构建后 `python3 scripts/verify_open_loop.py --compare-beam` 核对 Wings/BEAM/独立模型；这是有界 `smoke`，不是 30s/60s/五次重复、fairness、整机内存或性能验收。

[ADR-0067](docs/adr/0067-beam-bounded-admission.md) 为 BEAM 测试端补发送前有限准入、开始时归还和终态关闭。`python3 scripts/verify_admission_counter.py --compare-beam` 核对 1/100/256 满队列拒绝、健康 Bytes actor 与停止清理；默认完整 AOT 只跑 Wings/模型。这不等于开环、吞吐或整机内存验收。

[ADR-0066](docs/adr/0066-bounded-scheduler-mailbox-capacity.md) 将 Scheduler mailbox 可配置上限扩至 256，固定容量按实际槽数预先计费；100 槽满队列的真实 Bytes AOT 检查显式拒绝、健康 actor 推进和取消清理。全局结果/语言 pending 上限不变；这只是热点协议前置，不是开环/BEAM 性能结果。

[ADR-0065](docs/adr/0065-two-sender-counter-fifo.md) 增加两个真实发送者的闭环 Counter 对照：完整 Bytes 内核对各自序号，独立模型验证合法交错，不规定跨发送者全序。构建后 `python3 scripts/verify_senders_counter.py --compare-beam` 显式运行 OTP；默认完整 AOT 只跑 Wings/模型，仍不是开环或 BEAM 性能/内存结果。

[ADR-0064](docs/adr/0064-indexed-supervisor-slots.md) 用固定空槽链和带身份校验的槽索引，消除监督创建/查询/停止的容量扫描，保留原初始化/计费/重启语义；poll 仍扫描固定容量。原 CPU/墙钟/阶段 deadline 不变，不将功能回归当成 BEAM 性能结果。

[ADR-0063](docs/adr/0063-multi-actor-byte-counter.md) 已将多 actor 固定 Bytes Counter 接入有界正确性测试，覆盖三种状态与代表性 actor 数；显式 `--compare-beam` 才运行真实 OTP。仍不是开环性能实验、整机内存结果或 MVP 完成，运行方式见 [BEAM 工作负载](docs/beam-workloads.md)。

[ADR-0062](docs/adr/0062-sparse-ready-scheduler.md) 将全 actor 派发扫描改成固定就绪/活栈 FIFO 和空槽链，容量上限为 16384 actor，请求表各最多 256。完整受限 AOT 的 `scheduler_sparse` 检查 1000/10000 标量 actor、100 活跃、FIFO、边界迁移和清理。构建使用下列 workspace/示例命令，不需额外依赖；这不是完整 Bytes 同业务、开环或 BEAM 性能/内存结果。

[ADR-0061](docs/adr/0061-on-demand-scheduler-stacks.md) 将 Scheduler worker 的业务栈改为按需 guarded mapping，映射前完整三级准入，活栈原线程保留至正常返回，最多缓存两个已返回栈。沿用完整受限 AOT 与 `scheduler_density` 回归，不缩小单栈、不更改 ABI/语法，不把逻辑预留减少称为 RSS 或 BEAM 内存结果；后续容量/调度安排见 ADR-0062。

[ADR-0060](docs/adr/0060-shared-scheduler-reply-table.md) 消除空闲 actor 重复预留的宿主结果表：`config.requests` 现在是整个 Scheduler 的宿主 Int/Bool/Unit 与 RecordInt 内容回复的总容量，语言 pending 表独立；请求仍向原目标 process 三级计费。完整受限 AOT 增加 16/64/256 actor 的逻辑预留与清理回归；后续栈安排见 ADR-0061。

[ADR-0059](docs/adr/0059-byte-state-counter-equivalence.md) 将完整 256 B/4 KiB/64 KiB Bytes state、32 条异步请求窗口及失败退出接到 Wings Counter。`python3 scripts/verify_counter_equivalence.py --compare-beam` 用真实 OTP 29 与独立 Python checked 模型核对结果、FIFO、旧提交状态和清理；不带该选项只运行 Wings/模型，不宣称执行了 BEAM。先构建 compiler/VM 示例，边界与命令见[同协议工作负载](docs/beam-workloads.md)。未提供吞吐、尾延迟或整机内存比较。

[ADR-0058](docs/adr/0058-byte-i64-big-endian-access.md) 补充 `read_i64_be` 和 `replace_i64_be`，支持 Bytes 内连续八字节的 signed big-endian 读写、未对齐 offset 及完整 i64 范围；写操作仍复制完整对象、计费并分块检查。该片将 ABI 升至 v14，旧产物须重编译，为后续 BEAM 固定字节状态对照补齐编码基础，不代表该业务或性能门禁已经通过。

[ADR-0057](docs/adr/0057-accounted-immutable-bytes.md) 增加 `Bytes`、`fill_bytes`、`bytes_len`、`byte_at` 与复制替换的 `replace_byte`；每块最多 256 字节，检查预算/取消，未发布 buffer 也向原进程三级计费。精确 GC 支持字节叶节点，失败不修改旧 state；ABI 升至 v13，旧产物须重编译。`examples/bytes.wings` 覆盖 state、fn、条件和循环，完整验证仍用下列 workspace 与受限 AOT 命令；不是字符串/通用记录或 BEAM 性能结果。类型大小写不变，语法草案仍待评审。

[ADR-0056](docs/adr/0056-multi-worker-supervision.md) 增加 `SchedulerSupervisor`：每个子进程有独立、固定终态票据，旧业务资源清理后才可由宿主 `poll` 消费。受管失败按有限滚动窗口 one-for-one 重启为全新 state/堆与 Pid；Never、强度超限或初始化/额度失败明确终结，不重放旧 mailbox，也不自动监督语言创建的后代。没有后台监督线程，服务宿主必须驱动 `poll`。

[ADR-0055](docs/adr/0055-multi-worker-exit-diagnostics.md) 增加 `SchedulerHandle::last_exit`：固定单槽按值保留最近失败的旧 Pid、worker、handler、ABI 故障、已提交标量 state、预算/GC 和清理前账目。失败 actor 仍立即清理；成功、stop 和关闭不覆盖该值，不提供可靠监督事件或无界历史。

[ADR-0054](docs/adr/0054-bounded-multi-worker-scheduler.md) 增加 `Scheduler` / `SchedulerHandle`：固定 1..=16 个 worker，活栈固定在 owner 线程，正常返回后按 worker 编号迁移；后续容量与就绪派发见 ADR-0062。共享 registry 保持 Pid 和 FIFO，语言消息、异步请求与 GC 沿原 ABI；宿主 handle 可跨线程投递标量请求。`examples/scheduler.wings` 与 `multi_worker` 覆盖 N > M、列表/记录复制和忙 actor 共存。不是自适应 work stealing；宿主监督另见 ADR-0056，完整资源账本及 BEAM 双目标仍待完成。

[ADR-0053](docs/adr/0053-detached-process-images.md) 增加宿主 `ScalarProcess::detach` / `DetachedProcess::attach`：无活栈时消费进程并独占转交 state/堆，目标线程绑定同 process 账户的本地池，不重跑 initializer。镜像不可 Clone、不含协程或执行接口；镜像本身不含 worker/mailbox/Pid，也不转交活栈，后续调度由 ADR-0054 接入。`detached_process` 验证 IntList 状态跨线程转交与取消后失败保留。

[ADR-0052](docs/adr/0052-cross-thread-module-leases.md) 使只读 `ScalarModule`/`HandlerId` 可跨线程共享，计费模块使用 Arc 租约；四个线程各建独立 worker，退出后由最后代码所有者归还预算。`parallel_modules` 是并行宿主回归，不是 M:N 调度器；actor、堆、栈池和挂起活栈仍在各自原线程。

按 [ADR-0050](docs/adr/0050-account-node-admission.md)，`tenant_accounted`/`process_accounted` 在创建子账户前向父路径预留控制节点字节，最后持有者释放才归还。worker 默认计费进程账户，TCP 示例及 CLI 两种模式使用计费根与子账户；根启动计费见 ADR-0079，旧零基线工具构造器仍保留，不代表完整资源隔离。

ABI v12 按 [ADR-0049](docs/adr/0049-record-message-copies.md) 和 [ADR-0051](docs/adr/0051-bounded-int-list-messages.md) 允许 RecordInt/IntList 语言消息参数：固定信封按值复制，列表合计最多 64 个整数，源遍历及目标重建逐节点检查预算/取消。目标入口根保活，源失败不撤回已接受消息；JSON/宿主/ingress 不接受原 handle 或段编码，回复范围后由 ADR-0096/0097 扩展。示例 `examples/record-messages.wings`、`examples/list-messages.wings` 及 `record_messages`、`list_messages`。

受限 TCP Counter 示例使用真实 loopback socket、固定 32 B 帧及一个有预算的适配线程，业务仍在原 worker 执行；连接/帧数和 I/O deadline 均有限，见 [ADR-0048](docs/adr/0048-bounded-loopback-service.md)。运行：`cargo run --locked -p wings-compile -- build examples/tcp-counter.wings -o target/tcp-counter`，再 `cargo run --locked -p wings-vm --example tcp_counter -- --trusted target/tcp-counter 1 4`。它只输出临时监听地址并等待有限请求，不是生产网络 VM 或 BEAM 等价性能负载。

`ScalarWorker::open_request_ingress` 可将单个外部入口绑定为标量或 RecordInt/IntList/Bytes/String 内容回复协议；原 `request` 输入仅标量，ADR-0115 的 `request_values` 支持类型化独立副本。发送端取得唯一 `IngressRequest`，固定结果表在取走结果且消息完成后才释放槽位，超时/取消不撤回消息；示例 `worker_ingress_requests` 与 `typed_ingress`，见 [ADR-0047](docs/adr/0047-cross-thread-ingress-requests.md)。不提供语言阻塞请求或通用网络服务。

宿主可在配置等待后用 `ScalarWorker::open_ingress` 注册单 actor 单入口的固定跨线程队列，`send` 只传 Int/Bool/Unit，ADR-0115 的 `send_values` 另支持内容副本，并以原票据转入 mailbox；关闭后旧 sender 拒绝，最后 sender 释放前保留控制与通知预算。示例 `worker_ingress` 与 `typed_ingress`，契约见 [ADR-0046](docs/adr/0046-bounded-cross-thread-ingress.md)。不传原堆 handle，也不是通用网络服务或 M:N。

资源账户与预留票据支持跨线程共享/归还，同 VM 的三级四维准入和净转移通过一个门闩串行提交，失败不留半账；见 [ADR-0045](docs/adr/0045-concurrent-resource-admission.md)。actor/活栈仍绑定原线程，这不是 M:N，也没有无锁或吞吐领先结论。

`ScalarModule::load_accounted(directory, vm)` 在解析/装载前预留 VM 级模块逻辑预算，同模块实例共享一次预留；Library 随最后代码租约交还，挂起调用先正常取消。CLI 两种模式使用该路径，旧库 API `load_trusted` 工具入口不计费，限制见 [ADR-0044](docs/adr/0044-native-module-leases.md)。这不是热更新或精确 RSS 计量。

受管 GC 每 16 个节点/根检查预算、取消与让出，暂停后从原 phase 继续；总预算包含 GC 工作单位，堆统计不扫描全堆。示例 `examples/chunked-gc.wings`，限制见 [ADR-0043](docs/adr/0043-chunked-runtime-gc.md)；尚无块耗时或同业务性能达标结果。

宿主 `ScalarWorker::enable_wait()` 为 OS 等待端预留 4096 字节控制预算；`wait(max_wait)` 重查 mailbox、挂起入口、请求 deadline 及有界 ingress，不执行 handler，就绪时立即返回。等待边界见 [ADR-0041](docs/adr/0041-worker-deadline-parking.md)，ADR-0046 补充 ingress 通知租约；它不提供完整 I/O reactor。

`request(pid, Message, Completed, timeout_ms, args...)` 在调用点准入并返回局部 RequestId，结果异步到达 `on Completed(id: RequestId, status: Int, value: Int)`，status 的 0/1/2/3 表示回复/超时/取消/失败。当前只接受 Int 回复；队满保留预付费完成事件，不覆盖已终结结果。`cancel_request` 不撤回目标消息。示例 `examples/request-events.wings`、`examples/request-roots.wings`，限制见 [ADR-0040](docs/adr/0040-language-request-events.md)。

`self_pid()` 与 `spawn_self()` 返回局部 `Pid`，`send(pid, Message, args...)` 静态检查本模块单向入口；子实例继承资源配置、拥有新状态，父失败不撤回已创建子项。示例 `examples/local-processes.wings`、`examples/local-process-roots.wings`，见 [ADR-0039](docs/adr/0039-local-process-capabilities.md)。Pid 暂不进入 state/消息/回复，无跨 worker 投递或隐式监督；宿主 `pids()` 可无分配迭代现有实例。

未发布源码 ABI v10 支持 handler 内 `send_self(Message, args...)`：目标为本 process 单向入口，worker 在调用点准入入队，下一片恢复后继续；失败不撤回此前发送。示例 `examples/self-send.wings` 与 `examples/self-send-roots.wings`，见 [ADR-0038](docs/adr/0038-self-send-effects.md)。CLI 与独立实例无 worker 回调，调用受管失败；旧 v9 产物须重编译。

`IntList` 是不可变整数节点和共享尾，可作 state、局部、fn 值、有界语言消息参数和最多 64 元素的回复。`empty_ints` / `prepend_int` / `head_int` / `tail_ints` / `is_empty_ints` 每次只处理一个节点，使用同一计费堆与传递 GC；空读取受管失败。示例 `examples/lists.wings`，见 [ADR-0037](docs/adr/0037-immutable-int-lists.md)，回复出口根与拥有者见 ADR-0097。

process 内可声明 `fn next(value: RecordInt) -> RecordInt { record_int(read_int(value) + 1) }`。函数只访问参数/局部和更早函数，禁止递归、state 访问和 while；私有受管调用逐帧检查剩余栈，保留跨帧精确根和强制预算检查。最多 32 层函数链、64 个聚合根，示例 `examples/functions.wings`、限制和验证见 [ADR-0042](docs/adr/0042-managed-function-frames.md)。

宿主 `ScalarWorker::snapshot` / `ScalarSupervisor::snapshot` 可查询排队、当前入口、已提交标量状态、进程用量及最近安全点预算/GC 统计；不推进执行或暴露记录 handle。`ScalarTask` / `ScalarExecution` 保留最近入口的只读执行快照。挂起统计不是后台实时采样，边界见 [ADR-0032](docs/adr/0032-safe-point-diagnostics.md)；验证命令仍为 workspace 测试及 debug/release AOT 脚本。

`ExecutionSnapshot::fault` 给出 ABI 故障或输出校验类别；worker/supervisor 的 `last_exit` 保留最近一次受管失败的固定快照，旧 Pid 失效或重启后仍可定位 handler 和预算/GC。快照不保留旧资源，账目是清理前值，不是泄漏量，见 [ADR-0035](docs/adr/0035-bounded-exit-diagnostics.md)。

未发布源码支持 `if flag { expression } else { expression }`，只执行选中臂，条件为 Bool、两臂为同类型标量、RecordInt 或 IntList；臂内暂不支持语句。示例为 `examples/branches.wings` 与 `examples/record-branches.wings`，实际路径预算和根边界见 [ADR-0029](docs/adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](docs/adr/0033-record-conditional-roots.md)。

条件可使用标量 `==`/`!=` 或有符号 Int 的 `<`/`<=`/`>`/`>=`；比较不转换类型或比较记录 handle。示例为 `examples/comparisons.wings`，契约见 [ADR-0030](docs/adr/0030-scalar-comparisons.md)。

标量、RecordInt 或 IntList state 的 handler 可用 `while`，每轮更新候选状态、正常返回才提交；循环体局部不逃逸，空循环也有强制预算/让出检查。尚无 break/continue 或任意引用 phi，见 `examples/loops.wings`、`examples/record-loops.wings`、[ADR-0031](docs/adr/0031-scalar-state-loops.md) 和 [ADR-0034](docs/adr/0034-record-state-loops.md)。

需要 Rust 1.85 或更新版本；构建 AOT 原型还需系统 C 链接工具链（`cc`，也可用 `CC` 指定编译器可执行路径）。开发构建仅为 Cranelift 代码生成与 regalloc2 开启二级优化，并显式保留其 debug assertions 与溢出检查，以控制原 AOT 压力回归的编译耗时；workspace 自有代码仍使用默认开发配置，release 配置不变。检查命令只验证当前语法子集，不生成机器码：

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

CLI 每行接受一个仅含 `message` 和 `args` 的 JSON 对象，两个字段各出现一次、顺序不限；参数至多 64 个，只能是符合 handler 签名的 i64 整数、布尔或 null。行长至多 64 KiB（包含换行），允许末行无换行；重复/未知字段、嵌套参数和浮点数在执行前拒绝。单向消息输出 `{"accepted":true}`，请求输出 `{"reply":...}`。输入解析与控制按 [ADR-0086](docs/adr/0086-accounted-cli-json.md) 从原三级额度中预留，标准 I/O 全局存储仍不在完整账本保证内。

具名记录回复按 [ADR-0117](docs/adr/0117-streamed-cli-record-replies.md) 输出字段对象，例如 `record Result { count: Int, text: String }` 的回复为 `{"reply":{"count":7,"text":{"string":"内容"}}}`。嵌套记录输出嵌套对象，空记录为 `{}`；RecordInt/IntList/Bytes/String 叶分别沿用 `record_int`/`int_list`/`bytes`/`string` 包装。通过 `--heap-records` 配置计费堆后可运行 `examples/named-record-replies.wings` 的 `Value`/`EmptyValue`；输出转义可能使一行超过 64 KiB，不改变业务载荷上限。

LSP 使用 stdio、UTF-16 位置和整份文档同步，支持诊断、局部跳转、悬停与基础补全。目前逐份文档同步分析，尚无跨文件解析、增量失效或运行中取消；双平台 CI 覆盖构建，LSP 协议冒烟仍仅在本机 macOS AArch64 验证。语言支持范围见[语言设计](docs/language.md)。

未发布源码另有执行控制：每条 IR 指令及输出提交前强制检查原子取消/让出标志和预算。`wings-vm run --trusted <目录> --max-steps <u64>` 可设置每次 handler 的总上限（默认 1,000,000）。库接口 `start_controlled` 返回独占实例的 `ScalarExecution`，可按步数时间片在同线程让出、保留原栈恢复；取消、总预算或栈空间耗尽会终止，不提交该次状态与回复。同步调用、initializer 与挂起调用统一使用固定 256 KiB 可用栈；薄入口在业务帧前检查剩余空间，worker 初始化栈也计入进程额度，池满或执行前准入失败显式拒绝。Drop 通过正常取消返回清理，不跨受管 ABI unwind。这不是完整调度器、GC 或三级资源账本；源码 ABI v10 只装载匹配源码编译器产物，不兼容已发布 `0.0.2` 的 v1 及旧源码 v2–v9 产物，须重新编译。原栈恢复见 [ADR-0011](docs/adr/0011-scalar-stackful-resume.md)，剩余栈检查见 [ADR-0024](docs/adr/0024-scalar-stack-boundary.md)，根链与持久状态见 [ADR-0025](docs/adr/0025-aot-record-roots.md) 和 [ADR-0027](docs/adr/0027-persistent-record-state.md)。

原栈分配现由自研 `StackPool` 管理：默认每线程最多 64 个活跃槽位、缓存 2 个已完成栈，耗尽明确失败；可显式传入池并读取容量统计、调用 `trim` 回收缓存页。完成调用立即归还槽位，挂起栈不能复用。此优化主要约束容量和缓存，不消除同时挂起栈的物理成本。实验与限制见[栈分配实验](docs/evaluation.md#原生栈分配实验)。

`ScalarModule` 可装载一次并反复实例化，共享只读清单和入口数组，各实例独立执行初始化、保存状态与失败标记。宿主可预解析模块绑定的 `HandlerId`，直接调用时不再按字符串查找；外部 JSON 协议不变。独立实例自身没有 mailbox，worker 中的语言创建/发送另见 ADR-0039；模块共享契约见 [ADR-0013](docs/adr/0013-shared-scalar-modules.md)。

宿主可用 `ScalarTask` 按值保存实例和未完成调用，在同线程内移动而无需自引用；忙时拒绝新调用，正常结束才提交并释放栈，见 [ADR-0015](docs/adr/0015-owned-scalar-tasks.md)。`ScalarWorker` 在此基础上提供有限进程表、generation Pid、mailbox、固定 pending 表的异步请求及手动轮转时间片；process、消息、请求和活栈经过三级准入。超时/取消不撤回已接受消息，迟到回复不能覆盖复用请求槽位。该切片新增语言自发送及同模块创建/发送及异步整数请求事件；ADR-0048 另有受限 loopback I/O 示例，仍不是 M:N 调度器，见 [ADR-0020](docs/adr/0020-single-worker-mailbox.md) 与 [ADR-0021](docs/adr/0021-bounded-requests.md)。

worker 可选 `enable_timer` 后用 `tick_timed` 按单调 deadline 请求让出，仍在强制 safepoint 挂起、原线程恢复。默认不创建计时线程，未验证真实服务尾延迟或 M:N 扩展成本，见 [ADR-0022](docs/adr/0022-worker-deadline-preemption.md)。

`ScalarSupervisor` 可独占单 worker 做受限 one-for-one 恢复：失败后新实例重新初始化、使用新 Pid，保留旧请求结果但不重放消息；窗口内重启达到上限或重建失败即停止。显式 `spawn_with_heap` 以固定槽数创建及重建全新计费堆，不保留旧记录状态；普通 spawn 仍无堆。主动 stop 不重启，没有语言级 supervisor、link/monitor 或 OTP 兼容承诺，见 [ADR-0023](docs/adr/0023-bounded-host-supervision.md) 和 [ADR-0028](docs/adr/0028-supervised-record-heaps.md)。

语言局部与 state 均可用 `RecordInt`，受限内建为 `record_int(value)`、`read_int(record)`、`collect()`；记录不可变，不支持字段赋值，宿主原始整数参数仍只允许标量，语言消息参数另支持复制载荷，RecordInt 回复按 ADR-0096 复制内容。compiler 发布活跃引用及候选状态，宿主根独立保活已提交状态。CLI 用 `--heap-records 1..=1024` 显式配置堆，并使用固定 16 MiB 的三级字节额度；宿主 `instantiate_with_heap` 与 worker `spawn_with_heap` 允许显式配置。引用状态无堆时在初始化前拒绝，临时内建无堆时受管失败；同步及显式执行均保持同 process 计费栈池。见 [ADR-0026](docs/adr/0026-language-temporary-records.md) 和 [ADR-0027](docs/adr/0027-persistent-record-state.md)，可运行示例为 `examples/records.wings` 与 `examples/record-state.wings`。

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
