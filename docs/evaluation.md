# 评测与验收

## 可复现全量测试

按 [ADR-0110](adr/0110-reproducible-bounded-workspace-validation.md) 使用仓库内入口，输出目录必须不存在：

```sh
python3 scripts/verify_workspace_tests.py
python3 scripts/verify_workspace.py --output target/workspace-validation/debug-new
python3 scripts/verify_workspace.py --profile release --output target/workspace-validation/release-new
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
```

入口先编译所有 test target，再按指纹清单逐个实际执行。每 harness CPU 20s/墙钟 30s/core 0，输出、失败和退出码保留在新目录，失败不重试、不覆盖；不要同时重建同 profile。all-targets 不包含 doctest，零测试 harness 不计为业务用例。原生双平台 CI 使用相同入口并保存日志，既有真实 AOT 仍独立运行；本机通过和 Linux 交叉编译都不等于最新原生双平台或性能门禁通过。

## 增量验收契约

[ADR-0113](adr/0113-bounded-named-record-message-expansion.md) 验证嵌套/空记录、混合叶类型、名义类型拒绝、一次实参求值、记录节点及叶值合计 64 含界/超界与重复子树有界拒绝。真实 AOT 验证目标重建/精确根/GC、逐片预算/停止、单/双 worker 语言请求、消息发布后源失败、入口导入失败、既有合计限额和取消不撤回；原限不变，记录回复及完整 MVP 仍独立验收。

[ADR-0112](adr/0112-bounded-tcp-startup-evidence.md) 保留 TCP 原 5 秒绝对启动窗口，完整握手最多 4096 字节；半行/静默/EOF/非法输入失败后保留退出码和有界管道前缀，终止独立进程组，不自动重试。运行 `python3 scripts/verify_scalar_aot_tests.py`；取证与复验不等于间歇超时根因已修复。

[ADR-0111](adr/0111-bounded-string-content-equality.md) 验证空/Unicode/NUL、独立同内容对象、块边界和 64 KiB 首尾差异、不同长度与非规范化文本；IR 拒绝错类型/裸引用比较，两个末次使用源在全部 GC 检查点保活。预算/取消/陈旧和错标签保持输出及账目，真实 AOT 覆盖 CLI、fn、原栈恢复、一/双 worker 与失败清理，原限不变。

[ADR-0109](adr/0109-accounted-wakeup-endpoint-leases.md) 唤醒租约验收先复现 worker 销毁/通知者存活时的零账目，再检查三级零/少一/精确准入、OS 工厂失败回滚、账户句柄退出、跨线程末 owner 和既有 ingress/TCP 通知语义。保留原 4096 B、全部 CPU/墙钟/阶段限额，不将控制账本等同 kernel FD/RSS 或 BEAM 双门禁。

[ADR-0108](adr/0108-bounded-named-record-values.md) 须覆盖空/8 字段/32 schema 边界、同形错名义类型、前向/递归/能力字段拒绝，以及 IR/清单伪造类型索引与外部记录签名拒绝。真实 AOT 验证嵌套 String/Bytes/列表/记录根、跨 fn/分支/循环与逐片 GC、额度/预算/取消清理、单/双 worker、重启及 CLI；失败候选必须保留旧 state/root。原限不变，本地通过不等于原生双平台或 BEAM 双门禁完成。

[ADR-0107](adr/0107-accounted-control-token-leases.md) 控制租约回归验证三级不足/精确边界、跨线程末释放、worker/actor 退出后的独立标志存活及真实 AOT 正常/让出/取消/挂起 Drop；不以逻辑账目等同 RSS，不改变原时限、quota、ABI 或双平台 BEAM 门禁。

[ADR-0106](adr/0106-accounted-string-replies.md) 须覆盖 String/Bytes 独立类型与 callback 精确匹配、最大/Unicode/NUL 内容、出口预算/取消/GC 根、三级存储/控制拒绝、结果跨 actor 退出存活、满完成队列和末 owner 清理。真实 AOT 验证 direct/单 worker/一双 worker、ingress/监督、72 组完成情形与 CLI，原限不放宽；结果见[记录](validation-string-replies-2026-11-17.md)。

[ADR-0105](adr/0105-accounted-string-messages.md) 须验证 String/Bytes 共用合计上限、独立类型/内容、混合记录/列表入口根、UTF-8 分块、三级拒绝回滚及预算/取消清理；真实 AOT 覆盖单/双 worker、发布后失败、超时和停止。执行限额保持 CPU 20s/墙钟 30s/core 0/阶段 3s，结果见[验证记录](validation-string-messages-2026-11-17.md)，不替代完整 MVP 或 BEAM 双门禁。

[ADR-0104](adr/0104-bounded-string-literals.md) 须验证双引号/反斜杠/六种转义、直接 UTF-8/非法控制/错误恢复、解码后 64 KiB 和 IR 合计 1 MiB；常量与受管副本不共享计费生命周期。runtime 覆盖 UTF-8 跨块、三级预留/取消/预算回滚，真实 AOT 核对大字面量 initializer/fn/handler、根/GC/让出、一/双 worker 与原额度清理；旧 ABI 拒绝、平台及 BEAM 门禁保持，结果见[验证记录](validation-string-literals-2026-11-17.md)。

[ADR-0103](adr/0103-accounted-utf8-string-values.md) 须覆盖独立 String 类型/标签、合法及非法 UTF-8、跨 256 字节边界、Unicode scalar value 计数、64 KiB 拼接边界、String/Bytes 独立副本和精确根。语义、IR 与清单分别拒绝未开放消息/回复；实际 AOT 覆盖 fn/state/分支/循环、GC/让出/取消/Drop、一/双 worker 失败退出、旧提交保留和三级额度拒绝清理。原 CPU 20s/墙钟 30s/core 0、三秒业务阶段与资源上限保持；完整业务值、最新原生双平台及 BEAM 同负载双门禁另行验收，实际结果见[验证记录](validation-string-values-2026-11-17.md)。

[ADR-0102](adr/0102-opt-in-worker-lock-statistics.md) 须验证默认关闭、固定存储 tenant/VM 准入/回滚、字段饱和、并发读取、原锁释放后发布和末 handle 清理。真实 AOT 一/双 worker 开关对照相同状态/失败，停机后核对四类次数；额外时钟/原子开销须独立评估，不把诊断演示当未扰动吞吐或 CPU 证据。

[ADR-0101](adr/0101-empty-language-request-maintenance.md) 候选通过存活计数、双端回收及真实 AOT 回归，但 hot-queue 原 10000/s、30s/60s/五对中候选出现 86 次超时、更多队满，收益不稳定，已回退。完整拒绝/迟发/内存与回退验证见[记录](validation-empty-request-optimization-2026-11-16.md)，不把固定速率或局部测试当作吞吐上限/MVP。

[ADR-0100](adr/0100-coalesced-deadline-timer-notifications.md) 候选通过实际 timed wait 的早/晚重置、取消旧期限、过期 arm 和关闭回归，但 hot-queue 10000/s 原 30s/60s/五对无稳定收益，故回退。两版均含 ADR-0099；保留全部拒绝/超时和回退后验证，不以功能通过、最好轮次或两个中位数之比宣布性能门禁通过。

[ADR-0099](adr/0099-unready-result-poll-fast-path.md) 须以持结果表锁的确定性测试证明未就绪轮询免锁，并检查原 deadline、取消/发布/Drop、槽代次与三级准入。优化前后沿同业务 30s/60s/五对交替重复，保留全部尾延迟、拒绝、超时与分口径内存；不把一次 profile 或局部结果当作尾延迟根因、原生双平台或 BEAM 双目标通过。

[ADR-0098](adr/0098-accounted-byte-replies.md) 须覆盖 0/1/256/257/65536 字节回复、出口候选/结果根、逐块总预算/暂停/取消/Drop、三级存储及控制拒绝、末拥有者清理、六种回复精确错配、满队列/发起方退出及导入失败。真实 AOT 沿 CPU 20s/墙钟 30s/core 0 与三秒业务阶段运行；长测冻结包含实际参与构建的未提交源码，排除未采纳提案。不以功能回归或探索性长测替代原生双平台与同负载吞吐/整机内存门禁，实际证据见[验证记录](validation-2026-11-16.md)。

[ADR-0097](adr/0097-accounted-int-list-replies.md) 须验证出口根/暂停/取消、64 元素边界、三级结果预留、结果 Clone/Drop 生命周期、类型错配及无效/超限回复不提交候选，实际覆盖同步/任务/worker/监督/ingress/Scheduler 完成导入与失败清理；保持原预算和时间/资源上限。

[ADR-0096](adr/0096-copied-record-replies.md) 的实现须验证回复引用检查先于候选提交、宿主内容独立、完成记录导入/入口根、GC/活栈、失败默认值与导入失败、零超时/取消不撤回及最终清理。沿原资源/阶段/CPU/墙钟限制，不能把固定记录切片当作全部引用回复或 BEAM 性能证据。

[ADR-0095](adr/0095-typed-scalar-request-events.md) 须检查 Int/Bool/Unit 精确匹配与交叉错配、系统事件不可伪造、runtime 错配 effect 的发布前回滚，以及非法标量输出不提交候选状态、保留旧状态。真实 AOT 覆盖正常三类型、取消/超时/失败、自请求及一/二 worker 完成事件；保持原预算/阶段/CPU/墙钟限制，不将功能扩展记为引用回复、原生双平台或性能门禁通过。

[ADR-0094](adr/0094-typed-host-message-values.md) 的宿主类型化消息须验证严格 schema、规范载荷和原合计边界、源修改后的独立性、三级准入回滚、stale Pid/错误模块/队列与结果表满、超时/取消保留及准备期间目标退出。真实 AOT 复用原三秒阶段与 CPU/墙钟边界核对混合导入和 GC，不将新增入口当作完整业务值或 BEAM 性能/内存验收。

[ADR-0093](adr/0093-scheduler-ready-before-park.md) 先以空认领、发布/通知、等待决定的确定顺序复现旧停车分支，再核对本 worker 立即重试、外来队列/准入失败退避、只认领一次与清理；非零 deadline 使用实际操作前后时间界检查发布/失败/取消和定时 wait，允许合法竞争而不强求线程获胜。原回归/AOT 限制不变，不将修正写成全部超时或公平性达标。

[ADR-0092](adr/0092-concurrent-failure-counter.md) 的 v9 在两个真实 sender 下核对失败前缀、旧提交序号、逐请求终态和健康目标；用枚举小流产生正例及伪造历史反例，再运行 Wings/OTP 29/独立模型。保留原三种完整状态和业务/AOT 限制，不比较两端实际交错完全相同，不据此验收超时、恢复或性能/内存双门禁。

[ADR-0091](adr/0091-manifest-string-preflight.md) 保留清单错误的文本、活跃返回存储和累计分配请求探针，再对同一输入复测；分别验证长字符串/键的所有错误位置、转义/字符/UTF-8 字节边界、短非 ASCII 错误、原 serde 表示兼容及语法/递归/尾随拒绝。真实 CLI 的该错误足够短而不依赖输出截断，仍独立保留固定输出和断管回归。

[ADR-0090](adr/0090-bounded-cli-reporting.md) 先保存帮助断管 panic 与非法产物诊断放大的字节数，再验证 VM 级精确控制准入、固定缓冲/UTF-8 截断、描述符失败/关闭与写失败。真实帮助、参数拒绝和运行错误在断管下保持正常退出，默认/堆模式非法产物诊断最多 4096 B；来源错误分配不以截断测试代替验收。

[ADR-0089](adr/0089-bounded-cli-startup-arguments.md) 先复现真实非 UTF-8 参数 panic，再验证参数读取前三级准入、原选项/数字范围、七参数及含 NUL 16 KiB 精确边界、非 UTF-8 正常失败与生命周期清理。Linux 额外经非 UTF-8 目录别名运行真实 AOT，不能用只替换 args_os 的提前复制路径通过验收。

[ADR-0088](adr/0088-owned-cli-stdio.md) 先记录全局 stdin/stdout 锁释放后仍保留的分配，再验证三级精确 I/O 准入、复制前拒绝、首个/第二个描述符失败回滚、close-on-exec/原所有者保留与先销毁后归还。真实默认/堆 CLI 在 stdin 尚未 EOF 时必须输出回复，断开输出和读错误均正常失败；不提高容量或原时限。

[ADR-0087](adr/0087-accounted-beam-workload-roots.md) 要求当前业务正确性与新长测采集验证正启动成本和相等清理余量，伪造/缺失回执使实验无效。历史离线重放允许旧报告缺少回执，但出现任一字段即严格校验；新采集明确传入预期运行时，不能按报告字段自行降级。冻结测量不回写，原双门禁未通过。

[ADR-0086](adr/0086-accounted-cli-json.md) 先复现重复 JSON 字段折叠后执行，再验证固定解析、64/65 参数、转义/Unicode、数值类型、最大行、三级精确准入、缓冲复用和 I/O 失败清理。真实 CLI 在默认/堆模式拒绝当前非法请求并保留此前合法回复；已有正常回复与全部业务回归继续执行。

[ADR-0085](adr/0085-module-path-admission.md) 先复现零额度仍先解析路径，再在旧根和启动计费根检查控制预算零/少一字节/精确边界及库页追加失败回滚。原完整模块精确额度、循环加载、坏产物、跨线程末持有者和暂停取消回归继续执行，不提高资源或时限。

[ADR-0084](adr/0084-accounted-root-module-admission.md) 先复现计费根接受工具模块，再检查初始化/调用/owned task/镜像拒绝的入口计数、账目、池与状态。真实 AOT 标量/Bytes 和单/双 worker 拒绝后仍可在原容量接入合法计费模块，清理回到启动基线。

[ADR-0083](adr/0083-accounted-module-execution.md) 先复现计费模块的默认初始化降级，再验证入口调用计数为零、未计费/未绑定/跨 VM 池拒绝、显式与 owned task 拒绝及镜像失败后接回。实际 `module_accounting` 在三级满额下检查不能换工具池绕过，原状态、栈、账目与镜像保持；显式正路径核对新增栈/控制成本、暂停取消及最后基线，不提高原预算或时限。

[ADR-0082](adr/0082-bounded-tcp-segments.md) 用真实连接的 128×16 帧回归复现旧运行期超额，再检查双向读写、实际 MSS、超额峰值/计数及原部分帧截止时间。完整受限 AOT 增加原最大 8×64 帧业务，原正常/失败/非法/超时/断连期望不放宽；报告设置错误与缓冲拒绝必须为零，macOS MSS 不大于 1024，其余平台为 null。探针与功能重复不作为吞吐、整机内存或任意内核条件的证明。

[ADR-0081](adr/0081-accounted-default-cli.md) 在完整受限 AOT 中增加 CLI 加载前拒绝：16 MiB 稀疏伪库配坏清单，默认/堆模式都应先报 VM 资源不足且无 stdout，不解析清单或执行原生代码。旧默认路径的清单解析失败单独保留为缺陷复现；实际正常标量/堆调用与原错误/预算场景继续验证。该上限是逻辑准入，不作为 OS 内存隔离或 BEAM 内存结果。

[ADR-0080](adr/0080-central-execution-admission.md) 先复现直接执行漏计 4352 字节，再核对初始化/同步/owned task 的三级控制拒绝、准备失败栈/账目零变化、显式池保留、ready/挂起/取消/失败清理及固定控制上界。实际 AOT `accounted_stacks` 额外验证完全满 VM 拒绝、保留精确控制余量后栈净转移与实时账户数值；不扩大原 VM/业务资源上限或时限。单 worker 极小额度旧初始化现在应拒绝，再按实际初始化需求构造后续 handler 队首保留回归，不隐藏新增计费。

[ADR-0079](adr/0079-accounted-vm-bootstrap.md) 检查根/门闩精确启动边界、计费树旧子入口背压、满 VM 净转移、并发节点竞争及末持有者释放。弱引用须证明没有根自引用环；真实 AOT 核对标量暂停/取消、Bytes 旧提交失败、一/双 worker 关闭后未消费结果保留及最终启动基线。TCP 各场景必须报告正的 `bootstrap_bytes` 且 `remaining_bytes` 与之相等；原 I/O/阶段/CPU/墙钟边界不变，启动逻辑计费不等于整机内存测量。

[ADR-0078](adr/0078-request-publisher-lifetimes.md) 增加请求额度回归：单槽/三级单请求上限下，取走超时/取消结果后目标仍排队/持活栈，后续请求必须拒绝且资源零变化；目标完成/stop 后才释放。固定表覆盖完成队满、事件先入队、发起方退出/自请求、两端结束顺序、取消旧能力和代次耗尽；真实 AOT 验证状态继续提交与退出归零。保留原完整 AOT CPU 20 秒/墙钟 30 秒及业务 3 秒阶段，不以容量扩张通过。

[ADR-0077](adr/0077-streamed-fairness-measurement.md) 的独立 v8 公平性长测连续预热 30s/采样 60s、固定五对，不重复启动短测替代。运行端以有界 buffer 保留热/冷实际事件，离线核对全部 900000 个计划、实际 pending、CPU 完整结果与每 actor 状态；按计划区分阶段，迟到结果保留。每 actor 分位/最大等待、热任务完成率、生成迟到和可选让出延迟分列；诊断开销模式、丢样和无数据显式披露。原有短测/完整 AOT 时限不变，CPU 硬配额、过载分组、原生双平台和同负载吞吐/整机内存门禁仍未通过。

[ADR-0076](adr/0076-accounted-byte-messages.md) 须验证 Bytes 消息的精确合计边界、空值/最高 mask、混合记录/列表参数、三级准入/净转移/失败回滚，不能以原 handle 注入冒充复制。真实 AOT 在原 3 秒阶段及 CPU 20 秒/墙钟 30 秒边界下检查自发送、异步请求、一/双 worker、发送方后续 GC/失败、目标导入失败/旧状态与保留队列/取消清理；长时测量和完整业务值门禁独立。

[ADR-0075](adr/0075-platform-memory-observation.md) 在 v6 原采样点分列 Linux RSS/PSS/private/shared/SwapPss、macOS resident/physical footprint 和主机压缩/swap。冻结 C helper/source/编译命令，记录 PID/start_id、前后 census、时间窗与 helper 成本；拓扑变化/读取错误时完整性为 false、平台总数为 null，不删除缺失值或相加不同口径。仍缺每进程压缩/swap、整体共享/内核成本和观察器偏差验收，不据字段存在宣布整机内存门禁通过。

[ADR-0074](adr/0074-bounded-concurrent-recovery.md) 的 `concurrent_recovery` 已接完整 AOT 入口，固定两个 sender、两 actor、4 KiB Bytes、mailbox 16/结果 17、一/双 worker 和原 3 秒阶段。核对已超时/取消消息仍占队列/请求额度、队满无净预留、健康 actor 推进、stop/取消合法终态、Fail 结果取消不撤回执行、旧状态/新 heap 与强度限制、最终零；独立表测试覆盖取消/发布/丢弃竞争。它不替代完整故障注入或 BEAM 测量。

[ADR-0073](adr/0073-reusable-initializer-stack-pool.md) 的显式初始化池仅消除批量启动重复 mapping。须核对独立 state、错误 tenant/未计费/满池拒绝、process/tenant/VM 准入与失败回滚、监督重启不持有池及最终零；完整 v2/AOT 保持原规模和 3 秒阶段、CPU/墙钟限制。保留旧失败及前后版本证据，不用仿真 wall time 宣称 BEAM 性能或内存领先。

v5/v6 接入同一池时，先以独立阶段探针记录创建、配置、实际结果、核对/停止及清理；保留所有失败尝试，再执行未插桩的完整 v5 与 AOT 原参数回归。池构造仍在原 3/120 秒阶段内，计划时钟与全部到达不变；冷启动或负载波动中的复用版失败不得据后续通过删去。

[ADR-0072](adr/0072-scheduler-yield-observation.md) 将请求/真实返回时间接到 Scheduler 每 worker 有界 FIFO，独立 `--observe` 保存原始序号、actor、请求/返回时间、合并/清除/丢样；守恒与时序由 Python 独立核对。原片预算/quantum 不变，零样本的分位为 null，有丢样不称完整；不是长时公平性、CPU 配额、开销或候选 p99<5ms 验收。

[ADR-0071](adr/0071-observed-atomic-yield.md) 补独立请求/真实切回宿主的保守观测原语；仅显式启用，结果包含 safepoint、OS 调度和切栈开销。独立百万次加法 fixture 验证配对与未命中请求，不代表 Scheduler timer 的长时样本、完整丢样统计、候选 p99<5ms 或公平性通过。

[ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 的v7用单worker/单BEAM scheduler核对百万次checked加法与每10ms同刻100个心跳，保存逐条原始样本、每actor最大端到端等待、拒绝/超时和小样本分位；少于10000不报p999。CPU请求未完成期间观察到心跳仅是pending重叠，不是让出延迟；OS单核硬配额、30s/60s公平性长测、独立让出时间戳和双平台门禁仍待验收。

[ADR-0069](adr/0069-streamed-open-loop-measurement.md) 接入独立固定 30s/60s/五次重复的开环入口、可选最多八点翻倍扫描、原始事件关联核对和精确 nearest-rank 分位。每次实验冻结 runner/产物/源文件；目前进程树加驱动 RSS 只是观测，缺少 CPU 硬配额、Linux PSS、macOS physical/压缩/swap 及主机隔离证据，仍不构成同负载吞吐/整机内存门禁通过。协议和运行命令见 [BEAM v6](beam-workloads.md#v6独立长时开环测量)。

[ADR-0068](adr/0068-bounded-open-loop-counter.md) 的 v5 只验证计划时刻开环、逐条拒绝/超时、迟到实际结果和最终提交记账；短时 `smoke` 不作吞吐/尾延迟排名。完整负载、长时预热/采样/重复及双平台整机测量仍按本文与 [BEAM 协议](beam-workloads.md)验收，不能用功能探测替代。

[ADR-0067](adr/0067-beam-bounded-admission.md) 的 `verify_admission_counter.py --compare-beam` 用完整 4 KiB state 核对 1/100/256 排队、额外拒绝、健康单向 +7、满队列停止/旧入口拒绝。BEAM 模块另检查开始归还/FIFO/复用、非法容量/外来开始/重复归还、两个实际发送进程竞争不超额和关闭竞态；Wings 检查请求一次终结及逻辑归零。默认完整 AOT 仅 Wings/模型，业务 CPU 5 秒/墙钟 10 秒/actor 总 3 秒不变；不等同开环、并发业务故障/超时或整机内存结果。

[ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 的 `scheduler_large_mailboxes` 检查 64/100/256 槽排满、额外 send/request 拒绝、三级计费回滚、全局结果容量、健康 actor 推进及停止后一次终结/清理。完整 AOT 增加 `scheduler_overload`：真实 4 KiB Bytes、强制检查点 Hold、100 个排队请求与健康 actor，沿原 CPU 20 秒/墙钟 30 秒/禁 core。它不是固定速率开环/等价 BEAM 准入或尾延迟实验。

[ADR-0065](adr/0065-two-sender-counter-fifo.md) 的 `verify_senders_counter.py --compare-beam` 执行两个真实宿主线程/BEAM 进程的 Counter：各 16 条窗口、每条流至多 128 条，覆盖同目标竞争、多目标与空/非均衡流，三种完整 Bytes 状态。独立有界前缀模型检查每目标合法 FIFO 交错和实际回复/终值，不要求两端选同一交错；另拒绝伪造历史及真实 AOT 跳号/重复序号。默认完整 AOT 仍只跑 Wings/模型；CPU 5 秒/墙钟 10 秒/actor 总 deadline 3 秒不变。此处不覆盖任意交错会溢出的输入、并发故障或开环性能。

[ADR-0064](adr/0064-indexed-supervisor-slots.md) 增加监督满表拒绝/任意停止顺序/多轮槽复用/外来身份/失败创建与重启后槽恢复回归。重新运行 ADR-0063 代表容量用例，保留 actor 阶段 3 秒而不缩小规模；Linux TCG 仅记功能证据，不将耗时差异用于 BEAM 性能判断。

[ADR-0063](adr/0063-multi-actor-byte-counter.md) 的 `verify_multi_counter.py --compare-beam` 用真实 Wings Scheduler/OTP 29/独立 Python 模型验证多 actor Bytes Counter：10000×256 B、1000×4 KiB、100×64 KiB 均保留完整状态，每目标不同 ID，100 个活跃目标、最多 256 请求及总窗口 32。另覆盖单目标、空输入、跨窗口 FIFO、并发目标失败、溢出优先、旧提交状态和清理。Wings 读取长度/32 B 前缀，不声称完整 padding 导出；默认完整 AOT 只运行 Wings/模型。业务 CPU 5 秒/墙钟 10 秒/actor 总 deadline 3 秒不变，仍是单发送者闭环，不替代开环、超时/过载或性能/整机内存采样。

[ADR-0062](adr/0062-sparse-ready-scheduler.md) 的完整受限 AOT 增加 `scheduler_sparse`：1000/10000 个标量 Counter、2 worker、100 个活跃 actor，固定 32 条以内宿主请求窗口，各 actor 两条连续请求共三轮、共 600 次成功回复，核对状态独立、FIFO、五次边界迁移、空闲状态、表满无部分准入、旧 Pid/代次及最终归零。队列单测覆盖去重、头/中/尾摘链、跨 worker 归属和旧成员清理；保留暂停/取消、并发生产者、语言消息/完成、启动失败及监督回归。此处是标量功能探测，不是完整 Bytes 同业务状态或开环/RSS/BEAM 性能门禁，原 CPU 20 秒/墙钟 30 秒/禁 core 不变。

[ADR-0061](adr/0061-on-demand-scheduler-stacks.md) 的按需栈验证包含冷借入前三级准入、失败无半槽、缓存净转移、祖先额度满时复用与冷借入的不同结果、活栈独占及最终清理；完整 AOT 增加同时挂起三个 actor（超过两个缓存上限），原 owner 不变且其他 actor 可推进，继续覆盖语言 spawn/GC、迁移和监督。密度探测沿用相同 actor/worker/队列/请求容量，增加空表不超过 12 MiB 的回归，报告空表/填满逻辑预留；单测另报当前已映射栈，不把空闲预留减少当 RSS、BEAM 对照或性能门禁通过。

[ADR-0060](adr/0060-shared-scheduler-reply-table.md) 增加多 actor 混合回复共享容量、满表拒绝账目不变、消费后复用、关闭后结果及三级请求准入测试。完整受限 AOT 的 `scheduler_density` 使用 2 worker、64 宿主结果槽、mailbox 1 和 16/64/256 空闲 Counter，验证每 actor 控制增量不超过 4096 B、退出回到空表和最终零预留。该指标仅为逻辑计费；原逐 worker 全容量栈安排由 ADR-0061 替代，不是 RSS/BEAM 密度或吞吐门禁，真实业务对照与原 CPU/墙钟限制不变。

[ADR-0059](adr/0059-byte-state-counter-equivalence.md) 的 `verify_counter_equivalence.py --compare-beam` 执行真实 Wings/OTP 29 和独立 Python 模型：三种固定字节 state、最多 256 请求、32 条窗口、跨窗口 FIFO、i64 两端、溢出优先、候选失败不提交及故障队尾终结。宿主检查实际 Delivered 次序、回复前 state 提交、退出诊断旧字节和最终账目归零；JSON/二进制桥接分别验证边界。完整受限 AOT 默认执行 Wings/模型，不依赖未安装的 OTP，不能把该模式称 BEAM 对照。业务子进程 CPU 5 秒/墙钟 10 秒/禁 core、actor 总 deadline 3 秒，编译沿原限制。这不是开环、多 actor 或性能采样。

[ADR-0058](adr/0058-byte-i64-big-endian-access.md) 的单测覆盖 i64 两端/负数、未对齐/尾部八字节、短 buffer 与极端 offset、源不变、精确 GC、低预算和取消。真实 `byte_buffers` AOT 核对最大 state 的反复更新、逐步恢复和低预算清理，独立根模型验证嵌套函数读写、失败不提交及解链。此处仅验证编码原语，不将其称为 Wings/BEAM 等价业务或性能结果。

[ADR-0057](adr/0057-accounted-immutable-bytes.md) 在 `byte_buffers` 受限 AOT 覆盖 0/256/4096/65536 字节、旧值不变、fn/条件/循环、小堆 GC、非法参数、低预算与 512 组最大状态暂停后取消/Drop 清理，另验证跨 worker 迁移及监督重建为空。`--check-byte-roots` 用独立字节堆在每个 compiler 检查点和 heap 回调前收集，核对源引用、候选出口及失败解链。单测检查三级预留失败、跨堆发布拒绝、传递根、回收与字节额度 GC 重试；子进程 CPU 20 秒/墙钟 30 秒/禁 core 不变。这只是固定业务字节的表示基础，不是 BEAM 等价负载或内存排名。

[ADR-0056](adr/0056-multi-worker-supervision.md) 增加并发八个 child 失败的独立终态检查、全新 state/代次、有限强度、Never、主动停止、重启额度/initializer 失败回滚；真实 `multi_worker` AOT 验证持久列表失败后以全新空列表恢复、旧请求失败、第二次失败达到强度上限。固定 poll 不保证自动恢复延迟，未补齐语言监督、故障历史或性能门禁。

[ADR-0055](adr/0055-multi-worker-exit-diagnostics.md) 在 scheduler 单测及 `multi_worker` AOT 中覆盖退出后旧 Pid、失败 owner、handler、结构化故障、旧提交 state 和预算/GC。单测另验证引用 state 脱敏、成功/stop/关闭保留缓存和所有业务资源归零后仍能持有诊断副本；AOT 失败循环不提交候选且其他 actor 继续运行。仍无跨 worker 可靠监督/故障历史或性能结果。

`last_exit()` 在 actor/mailbox 清理前发布，不是清理屏障；运行时只断言诊断字段、旧 Pid 和其他 actor 进度。核对重复查询不改变资源时，先销毁 scheduler 并 join 所有 worker，再验证 process/message/request 为零、持有 handle 查询前后完整账目相等，最后释放 handle 归零。不能拿两个运行中的全局账目快照相等，代替查询自身没有业务租约的检查；固定重复回归保留原超时和资源限制，不改运行时发布顺序。

[ADR-0054](adr/0054-bounded-multi-worker-scheduler.md) 增加 `cargo test -p wings-vm scheduler --locked` 和完整受限 AOT 中的 `multi_worker`。以 1/2/3 个 worker 运行更多 actor，检查状态连续、边界迁移、活栈线程固定、RecordInt/IntList 消息和请求完成、GC 及最终清理；内部回归覆盖并发发送、队满、准入回滚、请求超时/取消/目标退出与销毁后结果租约。每业务子进程仍限制 CPU 20 秒、墙钟 30 秒、禁 core。固定线程轮转及逐 worker 全容量栈池的成本尚未测量，不作为吞吐/内存结论。

[ADR-0053](adr/0053-detached-process-images.md) 的 `detached_process` 在受限 AOT 中先建立持久 IntList，释放源池后在三个新线程依次绑定同账户目标池，collect 后读取/追加并核对状态连续；不重跑 initializer。错账户/未计费池、重复 attach、取消后的失败镜像及未 attach 即跨线程 Drop 均检查资源清理。内部回归另覆盖标量 state、attach 后池满不毒化进程和原堆账户保留；原执行类型仍非 Send/Sync。这是宿主无活栈转交，不是调度器迁移或性能结果。

[ADR-0052](adr/0052-cross-thread-module-leases.md) 的 `parallel_modules` 在受限真实 AOT 子进程启动四个固定 2 MiB 栈的线程，各自创建计费 tenant/worker/actor。主线程在启动前丢弃模块，四个独立 Counter 单步执行 16 次请求，验证状态隔离、同模块 handler 身份及跨 VM 拒绝；worker 退出后只剩模块和验证线程预留，最后 handler 转至新线程释放、归还线程预留后归零。线程控制/栈预留只是验证宿主的逻辑预算，不当作 RSS、生产 worker 池、迁移或性能数据。

[ADR-0051](adr/0051-bounded-int-list-messages.md) 的 `list_messages` 由受限 AOT 脚本验证跨 actor/self、整数异步请求、多列表混合 RecordInt/Bool、空列表、共享尾独立复制、总元素 64 成功/65 拒绝、源失败不撤回及目标堆满。三个入口各 160 个单步 stop/Drop 边界及预建 64 元素列表后的 320 个复制边界核对资源归零，后者检查确实覆盖源遍历和目标中途重建。`--check-list-message-roots` 在 heap/slow/effect 前强制 GC，核对最后使用的整个源列表及混合 mask。内部测试验证逐节点预算/取消、重建期间强制 GC、整数极值、全部 64 个空列表入口根、规范段/schema、stale/布局和 mask 拒绝；宿主/JSON/ingress 不接受原 handle 或段编码。没有列表性能或完整 MVP 结论。

最新已执行结果见 [2026-11-13 验证记录](validation-2026-11-13.md)：macOS 原生回归通过；Linux x86-64 隔离仿真通过独立 debug/release 测试及 release AOT 全回归，debug 大型编译触及既有 CPU 上限。不是双平台原生 CI、M0 或性能验收通过。

隔离环境可在 `cargo build --locked -p wings-compile -p wings-vm --bins --examples` 后以 `WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py` 直接运行全部已有示例，不在子进程内再次调用 Cargo。交叉构建可用绝对 `WINGS_TARGET_DIR` 指向包含 `debug`/`release` 的目录，release 仍设 `WINGS_PROFILE=release`；目标环境必须有 Python 和本机 C linker。执行上限、真实 AOT 编译/加载和断言不变；仿真执行只作功能证据，不能代替原生性能或完整双平台 CI。

[ADR-0050](adr/0050-account-node-admission.md) 验证计费子账户的精确父路径额度、祖先拒绝、层级/溢出失败无变化、子业务零用量、并发创建、跨线程最后释放及转移末票据不死锁。worker 核对成功创建的 actor 字节与独立节点字节、VM/tenant 元数据额度不足和初始化失败回滚；ingress/请求结果保留节点至末持有者归还。受限 TCP 全回归使用计费 tenant，检查服务全部退出后账目归零；这些是容量逻辑门禁，不是 RSS 数据。

[ADR-0049](adr/0049-record-message-copies.md) 的 `record_messages` 在受限 AOT 子进程验证跨 actor/self 复制、collect 后持久状态、请求参数、源失败不撤回、目标满堆及 96 个单步取消边界的清理。`--check-record-message-roots` 在每次 heap/slow/effect 回调前强制 GC，检查最后一次使用的参数根和混合 mask。内部回归核对全部 64 个导入参数、最高 mask 位、129 个汇总根槽、导入期间 GC、stale/列表布局拒绝、mask 越界/无源堆/非消息 effect 不让出；源码/IR/manifest/ABI 布局与宿主/JSON/ingress 伪造边界单独检查。不把 RecordInt 单节点复制当通用图复制或完整业务值验收。

[ADR-0048](adr/0048-bounded-loopback-service.md) 通过 `python3 scripts/verify_scalar_aot.py` 在 CPU/core/墙钟受限 OS 子进程启动真实 `tcp_counter`。Python 以临时 loopback socket 验证分片/流水线 FIFO、正常提交、checked overflow/候选失败、退出后拒绝、非法协议/模块、短帧/EOF、客户端写半关闭后仍回复并保留提交、读/accept 超时与连接/帧上限，核对服务退出时账面资源归零及 socket 收发缓冲均不超过 64 KiB；示例单测另检查缓冲配置及部分帧不刷新读 deadline。客户端保留读端直到确认回复，不能用在服务准入前就全关闭的竞态声称已验证发布后的断连。没有连接吞吐、state 大小等价或内存领先结论。

[ADR-0047](adr/0047-cross-thread-ingress-requests.md) 的 `cargo test --locked -p wings-vm ingress` 覆盖固定结果槽背压、三级拒绝回滚、完成/超时/取消/Drop 竞争、迟到回复、代次退休、Bool/Unit 与关闭后租约。受限 AOT 脚本另运行 `worker_ingress_requests`：四个生产者、16 个请求，核对实际生成代码提交后的整数/布尔/Unit 回复、失败、挂起清理和最终归零；不是服务或性能数据。

[ADR-0046](adr/0046-bounded-cross-thread-ingress.md) 的 ingress 回归覆盖配置/参数/额度拒绝、固定 FIFO、满 mailbox 保留、并发发布、关闭/槽位复用、挂起/失败退出和最后 sender 租约。私有通知失败 fixture 核对已接收但唤醒失败的结果，不关闭宿主 FD 制造错误。`cargo test --locked -p wings-vm ingress` 检查线程/账本边界，AOT 脚本在受限子进程运行 `worker_ingress` 验证生成代码的跨线程投递、等待与取消清理；两平台须分别执行，不将这些正确性用例当实际服务或吞吐结果。

[ADR-0045](adr/0045-concurrent-resource-admission.md) 的 `concurrent_resources` 使用最多 8 个工作线程、固定屏障/轮数，检查每维每层争抢额度、并发溢出、满共同祖先跨租户净转移、拒绝保留源所有权、跨线程 Drop 与四维快照一致。内部 fixture 另检查独立 VM 不共锁和门闩中毒恢复；旧确定性独立模型保留。命令为 `cargo test -p wings-vm --test concurrent_resources --test resources --locked` 与 workspace/AOT 检查；没有争用性能结果，不能当作 M:N 验收。

[ADR-0044](adr/0044-native-module-leases.md) 的 `module_accounting` 在受限 AOT 子进程验证级别/额度拒绝、同路径独立加载准入、坏清单/库/缺符号回滚、clone/ID 共享计费、挂起 task/worker/借用入口保活及正常取消清理、跨 VM worker/pool/heap 拒绝。反复加载/销毁恢复 VM 基线，不用账面归零宣称 OS 已解除全部映射。debug/release AOT 脚本覆盖；实际 dyld/ld.so 与 allocator 内存仍须平台实测。

[ADR-0043](adr/0043-chunked-runtime-gc.md) 的 core 回归在各 phase 中止后重新收集，核对传递根、16 节点处理上限、generation 退役和计数守恒。受限真实 AOT `chunked_gc` 使用 1024 槽堆逐步执行嵌套 collect，覆盖取消/Drop/预算、32 槽分配触发回收与 hot/cold 轮转；旧 `aot_records` 按实际新步数校验总预算。命令仍为 debug/release AOT 脚本与 workspace 检查；尚无块耗时或性能达标数据。

[ADR-0041](adr/0041-worker-deadline-parking.md) 验证私有等待端的配置/计费、就绪不睡眠、deadline 到期及结果不反复唤醒；OS 等待不等于业务公平性或完整 reactor 验收。

[ADR-0040](adr/0040-language-request-events.md) 要求异步整数请求的正常回复、超时/取消/退出唯一终结、队满完成事件保留、发起方清理及三级预留回滚回归，不能用同步宿主查询代替语言事件执行。

`request_events` 在受限 AOT 子进程验证正常回复、零/挂起超时、取消与目标算术失败、自请求、新状态先提交及各挂起边界 Drop 清理；workspace 可信 fixture 验证满完成 mailbox 保留预付费事件、发起方退出/槽复用、目标 stop/迟到结果、已终结取消不覆盖、过期/伪造取消、pending 饱和及三级字节/消息/请求额度失败回滚。`--check-request-roots` 在 heap/slow/effect 前强制 GC，覆盖 request/cancel 确认值、候选及外层根与拒绝解链。仍用 workspace 检查和 debug/release AOT 脚本，不是性能实验。

[ADR-0039](adr/0039-local-process-capabilities.md) 要求验证同模块创建的新状态、独立预算、跨进程发送、父失败不撤回、容量/三级准入回滚、失效能力与模块/worker 边界、effect 根及清理；不替代双平台验收。

`local_processes` 由同一 AOT 脚本在受限子进程检查创建当片可见、新 state、后续发送与一次处理、父失败/stop 后子独立存活、满表 SpawnRejected、Pid fn/条件和自身份；记录版本保留外层/候选根。`--check-local-process-roots` 在每次堆/慢路径/effect 前强制 GC，并检查 spawn/send 拒绝解链。workspace 测试另用可信 ABI fixture 检查 stale 能力、复用槽位、跨 worker/模块、VM/租户容量准入及子初始化失败回滚。仅可信 fixture 可注入能力整数，语言和 artifact 边界仍拒绝伪造。

`self_send` 由 debug/release AOT 脚本构建真实产物后在受限子进程运行：检查发送当片可见但候选尚未提交、既有队尾 FIFO、正常提交后只执行一次自消息、发送后除零/预算失败和队满故障的清理前诊断、三级消息额度拒绝零入队、stop/Drop 正常取消、0/64 参数与 Bool/Unit，以及带外层引用和候选 state 的后续 GC。脚本另在每个 heap/slow/effect 回调前强制收集，验证 effect 边界没有失根。命令仍为 workspace 检查和 `python3 scripts/verify_scalar_aot.py`，release 使用 `WINGS_PROFILE=release`。

[ADR-0038](adr/0038-self-send-effects.md) 校验自发送调用点可见、FIFO、失败不撤回、满队列/三级准入、独立执行拒绝和逐片/Drop 不重放；effect 边界同时核对引用根与退出清理，不将自发送当完整语言消息或跨 worker 证明。

[ADR-0037](adr/0037-immutable-int-lists.md) 用每个内建/慢路径前收集验证列表传递根、共享尾、条件/循环/函数和最终候选，另核对小堆 push/pop、空读取、预算/取消及完整计费清理。列表不是同业务 BEAM 基线实现。

[ADR-0042](adr/0042-managed-function-frames.md) 验证函数状态隔离、签名/声明顺序、链深、聚合根与总操作上限。真实 AOT 在每次 heap/slow 回调前强制 GC，观测至少三层根链，覆盖返回、嵌套取消、错误解链及候选不提交；`inline_functions` 保留旧示例命名，但已运行真实受管帧，并逐检查点 Drop 清理。`managed_stack` 在受限子进程用 32 层标量链验证剩余栈拒绝与正常返回；`frame_limits` 另验证源码 fn 实际帧超限在写产物前拒绝。这些检查不替代双平台或性能实验。

[ADR-0035](adr/0035-bounded-exit-diagnostics.md) 核对故障分类、清理前/后账目区别、重启后旧失败可查及后续覆盖；真实 AOT 预算、除零、堆满失败均需定位 handler 并终结请求，不能因缓存诊断保留旧资源。

[ADR-0034](adr/0034-record-state-loops.md) 的 RecordInt state while 验证 header、回边和零次出口的根，包含不读取状态的条件、嵌套和初始候选不被错误重复发布；每个慢路径/内建前强制收集，再核对 runtime 小堆/逐片恢复/失败不提交和清理。Linux x86-64 仍待独立执行。

RecordInt 条件合流按 [ADR-0033](adr/0033-record-conditional-roots.md) 在每次慢路径/内建前强制 GC，检查两臂、嵌套与循环体的路径根及持久候选；运行时逐片恢复、失败/预算/取消和额度清理另行核对，不以静态类型通过代替运行验证。

[ADR-0032](adr/0032-safe-point-diagnostics.md) 的诊断回归检查最近安全点预算单调、GC 统计、已提交状态隐藏及只读查询的计费守恒；必须包含逐片真实 AOT 循环。该快照不是吞吐/内存测量结果，Linux x86-64 仍须独立验证。

持久记录状态按 [ADR-0027](adr/0027-persistent-record-state.md) 验证跨调用 GC、已提交根/候选根、最后检查点、无堆拒绝、失败不提交、worker 隔离与三级清理。新增用例须分别执行双平台 debug/release，不能复用临时记录的既有结果。

带堆监督按 [ADR-0028](adr/0028-supervised-record-heaps.md) 验证首次与重建准入失败、全新状态、旧请求终结、强度上限、sibling 与挂起 stop/Drop 清理；不使用旧标量监督结果代替新切片验证。

条件表达式按 [ADR-0029](adr/0029-scalar-conditional-expressions.md) 检查结构化 IR、实际路径预算、未选臂无副作用/错误、逐步恢复与记录根，不以直线用例代替分支验收。

标量比较按 [ADR-0030](adr/0030-scalar-comparisons.md) 验证 i64 两端的有符号顺序、规范 Bool、类型/优先级、分支组合和预算；两个平台独立运行新原生用例。

标量循环按 [ADR-0031](adr/0031-scalar-state-loops.md) 验证回边强制检查、预算/取消、恢复不重放、跨迭代根、嵌套与双 actor 轮转；空循环和长循环均在受限 OS 子进程内运行，不用宿主无限循环制造压力。

[文档目录](../docs.md) · [项目概览](../README.md)

BEAM 是性能与内存的主基线，kameo/Tokio 和 Wasmtime 用作辅助对照。所有实验公开版本、编译参数、CPU/内存限制、数据生成器和原始结果。

## 对 BEAM 的验收标准

- **性能**：在相同 CPU 配额、业务逻辑、状态和消息协议下，Wings 的最大持续吞吐高于 BEAM，且满足相同的尾延迟要求。
- **内存**：在相同进程数、固定到达率、state、mailbox 和活跃比例下，Wings 的整机 RSS 低于 BEAM，同时满足尾延迟要求；另报 PSS、用户态分配量和内核成本。
- **共同条件**：两项使用同一套代表性负载，分别做固定负载与饱和吞吐测试。差异要超过重复测量的波动，不能拼接不同负载的最优值。普通 Idle、活跃与冬眠场景分别报告。

M0 确定 Erlang/OTP 版本、JIT 状态、调度器数量、堆与 GC 配置，以及各场景是否启用冬眠，此后按这套配置比较。BEAM 使用适合负载的正常配置，保留可用优化。当前没有基准数据，以下都是验收目标。

## 负载矩阵与基线

可执行的最小正确性 oracle 与代表组合见 [BEAM 工作负载协议 v1–v7](beam-workloads.md)。`python3 scripts/verify_beam_workload.py` 在受限 OS 子进程运行 OTP 29 的 checked i64 Counter，检查异步 request/reply、单发送者 FIFO、失败不提交及退出。当前本机 macOS 通过 11 个业务和 23 个输入拒绝用例，ADR-0059 的真实 Wings/BEAM/Python 三端另通过 45 组业务及边界拒绝；多 actor 正确性见 ADR-0063，双发送者合法交错见 ADR-0065，有限准入/关闭见 ADR-0067，短时计划开环见 ADR-0068，独立长时采样入口见 ADR-0069，CPU/突发心跳业务见 ADR-0070。Linux OTP oracle、完整 CPU/内存口径及性能验收仍未通过，不能当作性能达标。

- **内存维度**：逻辑 state 为 `0B/256B/4KiB/64KiB`，编码大小另报；mailbox 深度为 `0/1/100`，消息 payload 为 `32B/1KiB/64KiB`，活跃比例为 `0%/1%/10%/100%`。选代表性交叉组合并公开选择原因，不只挑 tiny state。
- **行为维度**：Idle、冬眠、正常计算、被抢占挂起、request/回复、超时/取消、热点收件人、spawn 风暴和关系 fan-out；空闲会话与同时执行的进程分开报告。
- **基线维度**：主基线为 BEAM 普通与冬眠进程；辅助基线为 kameo/Tokio 原生实现和固定版本、配置的 Wasmtime 实例。Lunatic 只有在版本、依赖和运行条件可复现时作为附加对照。
- **功能等价性**：无配额的原生代码可作为执行成本参考上限，但要另列满足相同资源与故障契约的部署方案。不同安全边界不能只按 ops/s 或单实例字节排名。
- **整机成本**：扣除各方案的固定进程基线（Wings 为空载 VM）后，用进程数量增长的分配量、RSS 与平台内存指标斜率观察增量成本；同时报告未扣除的整机总量、碎片与池缓存。Linux 另报 PSS/VMA，macOS 另报可获得的物理占用、压缩内存与 VM region。连接场景额外计入网络和内核成本。
- **测量质量**：在固定负载和饱和吞吐两种条件下测端到端延迟，使用可揭示过载的到达模型，公开样本数、预热、独立重复次数及统计方法；不能把排队时间从尾延迟中扣除。

WASM 常规线性内存页大小 `64KiB` 不代表每实例额外 RSS 至少 `64KiB`。模块最小内存、OS 提交、地址预留、栈和 allocator 配置都影响结果；[Wasmtime 内存配置文档](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.memory_reservation)区分了这些成本。AOT 与 JIT 是编译时机，不直接决定性能排名。

## 双平台验收

- macOS AArch64（Apple Silicon）与 Linux x86-64 都要运行完整的装载、ABI、根发布、抢占恢复、GC、消息、额度与监督测试，分别报告 M0 至 M4 的适用项目状态。Linux 测试通过不代表 macOS 也已通过。
- 每个平台都在同一台机器、同一系统下比较 Wings 与 BEAM，不跨 Apple Silicon 和 x86-64 机器比较。记录 OS、芯片、核心类型、页大小、工具链、链接和签名配置。
- 记录实际可用的 CPU/内存限制及测量方法。Apple Silicon 还需记录 QoS、性能核/能效核调度、电源模式和热状态；QoS 和 affinity hint 都不是硬绑核。除 RSS 外，报告物理占用、压缩和 swap，区分 OS 压缩与 VM 本身的节省。
- macOS 没有同口径数据时，PSS 标为不适用，physical footprint 单独报告。缺少的内核统计单独列出，不按零处理；跨平台的绝对值不合并排名。
- 针对 Darwin AArch64 弱内存序、平台保留寄存器和跨线程恢复做压力测试，并覆盖真实页大小下的 guard page、栈限额和回收。`kqueue` 与 `epoll` 的休眠、唤醒、取消和关闭竞态分别验收。
- 两个平台分别固定适用的候选指标，判断是否达标。只在一个平台达标，就只报告该平台的结论。

## 候选实验目标

以下是初始目标，硬件、数据集和测量方法在 M0 确定，不适用于任意负载。报告各百分位时，同时列出样本量、观测最大值与超标比例。

| 指标 | 限定条件与计时区间 | 初始目标与处理 |
|---|---|---|
| BEAM 吞吐对照 | 同一代表性负载、CPU 配额与尾延迟要求；测最大持续吞吐 | Wings/BEAM `>1`，并报告差异与测量波动；只优于辅助基线不算达标 |
| BEAM 内存对照 | 同一固定负载、进程数、state、mailbox、活跃比例与尾延迟要求；测整机 RSS | Wings/BEAM `<1`；降低 `≥30%` 作为进一步的候选价值门，在 M0 冻结 |
| 调度公平性 | 单核 CPU 配额，受管死循环与心跳 actor 竞争；计时请求到实际让出 | p99 `<5ms`，另测心跳端到端延迟；超标追踪检查覆盖和 runtime 长操作 |
| 原生执行成本 | 相同逻辑和 payload 的 kameo 原生基线，注明额度等功能差异 | `≥0.7×` 作为辅助成本预算，不代替 BEAM 对照或安全验收，不取消强制检查 |
| 本地 GC | 已分配堆 `≤64KiB`、活数据 `≤16KiB`、根 `≤64`；每次阻塞 worker 的 GC 区段 | p99 `<10µs` 作为探索目标；另报完整 GC 总耗时与大堆曲线，不能只报切片 |
| 冬眠唤醒 | 已接收 envelope 到进程 state 可供 handler 使用，blob `≤256B`，有可用执行额度 | p99 `<10µs`，解码、排队和总延迟分别列出；超标可禁用冬眠，不放宽准入 |
| 监督恢复 | 已接受受管终止请求到替代实例可运行，预留重启额度，无外部 I/O，固定小状态 | p99 `<1ms`；另报通知、业务恢复、退避和资源释放完成时间，不外推到 VM 进程崩溃 |
| 进程容量 | 百万微进程，先从 `256B` state、空 mailbox、`1%` 活跃负载开始，吞吐率和硬件在 M0 固定 | 完成可复现实验，报告相对 BEAM 的进程密度与整机成本，不等同于百万进程同时执行 |

M0 后沿用确定的基线，完整报告各分位结果。需求或硬件改变时，更新实验协议版本，重跑相关对照。吞吐和内存都领先才算核心目标达成；真实试点还要验证迁移、诊断和恢复成本。

## 正确性与故障注入

- **语言前端**：检查具名状态、类型化参数、局部绑定可见性、不可变绑定误赋值、重复消息名与回复类型；区分单向消息、`-> Unit` 确认和有值回复。缺失括号或 handler 结束符后仍能分析后续入口，CLI 与 LSP 诊断一致。
- **状态提交与回复（执行后端接入后）**：正常结束提交候选状态后回复，受管失败不提交也不成功回复；发送后失败不撤回副作用，回复投递失败不回滚已提交状态。候选状态和自动回复在 GC、抢占及资源超限时都保持有效且受计费约束。
- **程序装载**：编译运行最小程序，核对入口、配置、结果与退出状态；产物版本不兼容或入口无效时明确报错，不执行未通过装载校验的代码。
- **编译与根完整性**：优化级别组合、递归、深调用、浮点/SIMD、嵌套分配和 GC；比较不开 GC 与高频 GC 的结果，反复抢占、恢复和迁移，检查状态与活跃引用一致。
- **准入与计费**：消息/spawn/monitor 风暴、超大消息、解码膨胀、共享 blob 转发、编码中取消，以及迁移的临时双份状态；每条失败路径都检查额度守恒、无负值和无泄漏。
- **竞争与事件**：发送对退出、冬眠对到达、超时对回复、demonitor 对退出、杀进程对 worker 迁移；检查不丢唤醒、不重复执行，普通消息顺序满足声明的契约。
- **退出与恢复**：关系 fan-out、共享引用和 FD 释放分块执行；满 mailbox 下仍能接受终止请求，旁路心跳不被清理长时间饿死，终结通知不静默丢失。
- **升级失败**：在保留旧帧、迁移超时、schema 不兼容、内存不足和部分进程失败时，检查旧状态可继续运行；存在任意 `CodeLease` 时禁止卸载旧代码。
- **原生 worker 边界**：仅在隔离的测试 OS 子进程中注入 `abort`、非法访存和超额内存，验证父进程回收与失败通知。资源压力测试使用 OS 限额，不在开发宿主上制造无约束 OOM。

性能实验先通过受管正确性检查。有限测试和 fuzz 不能证明系统能安全运行恶意代码；每个里程碑都要保存可复现的失败用例和已知限制。

## 工具链与 LSP 验收

Rust 自动化回归入口为 `cargo test --workspace --locked`；优化构建使用 `cargo test --workspace --release --locked`。测试分为公共 API 集成测试与少量私有单测，覆盖源码位置/Unicode、词法语法及错误恢复、语义分析、LSP stdio 与文档同步、IR/清单验证、编译拒绝路径，以及资源预留/转移、精确根、活栈生命周期、mailbox、请求终结和 timer 清理。测试对象数、容量和操作次数均有固定上限；额度拒绝使用逻辑限额，不制造真实 OOM 或非法访存。

VM 宿主执行单测使用仓库内小型可信 Rust ABI 入口，不依赖磁盘动态库，不能据此证明编译器生成了强制检查点。真实 AOT 的检查点、候选输出提交及 debug/release 原栈恢复仍必须运行下面的受限子进程脚本。测试命令已进入既有双平台 CI，但本次新增测试的通过结果须以实际运行记录为准，不沿用历史结果。

已发布标量 AOT 的验证命令为 `cargo build --locked -p wings-compile -p wings-vm`，随后运行 `python3 scripts/verify_scalar_aot.py`；脚本在临时目录构建可信产物、驱动独立 VM 子进程，检查 Counter、标量类型、算术边界、错误输入与清单拒绝。该版本尚不验证执行控制或资源账户。macOS AArch64、Linux x86-64 的 [CI](https://github.com/wsafight/wings/actions/runs/43089909858) 已实际执行并通过该版本检查；后续新增用例不沿用这次结果。

未发布源码增加 ABI v3 执行控制验收：逐个检查点耗尽、提交前失败不写哨兵输出、零预算、取消优先级、旧 ABI 拒绝、每次调用预算重置、跨线程令牌取消及失败后库实例终止。脚本在有 CPU 限制且禁 core dump 的独立子进程内调用 ABI、`execution_control`、`stackful_resume` 与 `owned_tasks` 示例。活栈用例包含逐步让出、48 个跨检查点活跃标量、恰好足够的总预算、两个实例交错、未开始/挂起 Drop、挂起取消、算术失败与重复恢复错误；`owned_tasks` 另覆盖同线程槽位移动、忙时拒绝、跨模块 ID 拒绝、池耗尽不毒化实例、连续调用及未完成任务丢弃/取回时的清理，见 [ADR-0015](adr/0015-owned-scalar-tasks.md)。这些是正确性验证，不是调度或性能测量。

debug：`cargo build --locked -p wings-compile -p wings-vm --bins --examples` 后运行 `python3 scripts/verify_scalar_aot.py`。release：同一构建命令增加 `--release`，再执行 `WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py`。两个平台均需实际执行；新增用例不能沿用旧 CI 作为证据。GC 根、浮点/SIMD、迁移、guard page 故障注入、运行中并发取消延迟仍未验证；当前仅固定栈和标量，不添加无约束压力测试。

- 从编译命令产出目标平台程序，再由 `wings-vm` 装载运行。核对产物版本、目标、ABI、诊断、结果和退出状态；编译器与 VM 的共享格式要有一致性及错误输入测试。
- workspace 依赖符合 [Crate 设计](crates.md)且无环。检查 `wings-lsp` 的传递依赖不包含 Cranelift、`wings-codegen`、`wings-compile` 或 `wings-vm`，VM 构建也不引入解析器或 LSP 协议。
- 同一完整程序在编译器和 LSP 中的语法、符号、类型诊断一致；不完整输入覆盖缺失结束符、未知名称与跨文件修改，语言服务器能够恢复并继续查询。
- 测试未保存内容、连续编辑、模块依赖失效、查询取消与过时诊断，确认旧快照不会覆盖新版本。位置转换覆盖中文、emoji、组合字符、CRLF 和协议默认的 UTF-16。
- 验证 stdio 消息边界、初始化与关闭、capability 声明，以及日志不污染 stdout。测量首次诊断、增量编辑后诊断与补全的延迟、内存和取消响应，M0 确定数据集与目标，不预先编造响应时间。

`accounted_stacks` 另在相同受限子进程验证：VM/tenant 满额时净转移不双计，process 上限不足不激活/不毒化实例，同租户校验，完成/丢弃/取消归还，缓存和 trim 保留承诺，以及最后一个活栈释放时所有账目归零。见 [ADR-0019](adr/0019-accounted-stack-pools.md)。

独立记录堆、根帧与栈池计费已在 `ca8072b` 通过 macOS AArch64、Linux x86-64 的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43156013824)，并通过 Rust 1.85 all-target 检查。该证据不覆盖后续 worker 变更。

## 单 worker 标量消息回归

行为脚本构建含单向及回复入口的 Queue 产物，受限子进程运行 `scalar_worker`。检查 FIFO、多实例隔离、轮转、逐片候选提交、满队列拒绝、stale/跨 worker Pid、输入校验不改变队列、预算/算术失败退出、stop 满队列/挂起清理和三级用量回收，见 [ADR-0020](adr/0020-single-worker-mailbox.md)。

`scalar_requests` 另验证 Int/Unit 回复、pending 满（含已完成未取走）、requests/messages 限额拒绝零变化、零超时及挂起超时、取消不撤回状态副作用、迟到回复与槽位复用、stale/跨 worker 请求 ID、退出/stop 终结和结果票据的释放。请求 deadline 使用单调时钟，不改变 handler 的总步数预算，见 [ADR-0021](adr/0021-bounded-requests.md)。两示例均不覆盖语言内 send/spawn/request、自动计时唤醒、真实 I/O、compiler 根或同业务 BEAM 性能。

`worker_timer` 使用 128 个活跃标量的 Timer 产物验证可选 deadline 让出：先拒绝未启用、零时长和不可表示时长，再验证容量准入、原栈恢复/提交、总预算与挂起清理。原 400 值产物曾在共享 macOS CI 上编译触发 20 秒 CPU 限额；现缩小产物但保留强制检查和既定子进程资源上限。短 deadline 的有限观察只证明原子请求可到达强制检查点，不是抢占延迟、服务尾延迟或 BEAM 性能结果；空闲线程无轮询，Drop 应唤醒并 join，不等待原 deadline，见 [ADR-0022](adr/0022-worker-deadline-preemption.md)。

`scalar_supervisor` 在受限子进程运行同一 Queue 产物，验证失败前提交的状态不会进入重启实例、候选状态不提交、旧消息不重放、旧请求 Failed、其他子项继续执行、滚动窗口达到强度上限后不再重启，以及挂起 stop 和资源归零。Rust 单元回归另外验证精确窗口边界（不靠 sleep）、策略和构造拒绝、历史计费、stale/跨 supervisor ID、initializer 重建失败及 timer Drop。只覆盖宿主 one-for-one，不代表语言级监督、OTP oracle 或性能验收完成，见 [ADR-0023](adr/0023-bounded-host-supervision.md)。

## 独立记录堆与根帧回归

`record_heap` 行为示例检查布局、字段 schema、堆身份、stale handle、不可变记录图、精确保活、不移动、槽位复用、作用域及嵌套根、无效根导致零回收、容量拒绝和三级账目归零。与其他 VM 示例一样，在脚本的 CPU/墙钟限制、禁 core dump 子进程内以 debug/release 执行；对象数和根数均固定有界，不做真实 OOM。此结果不覆盖 compiler 根发布、AOT 中途 GC 或完整业务根集，见 [ADR-0018](adr/0018-bounded-record-heap.md)。

## AOT 临时记录与根链回归

`scripts/verify_scalar_aot.py` 在 CPU/墙钟上限、禁 core dump 子进程内执行编译端 `build_aot_records`，再由 VM 端 `aot_records` 运行生成代码，不给 VM 添加前端或代码生成依赖。覆盖一个 live root 与多个 dead records 在二槽堆反复回收、128 次单槽复用、64 个同时存活根、跨时间片与双实例交错恢复、initializer 中分配/收集、整数位型不保活、堆满/算术/预算/取消/Drop 失败及三级归零。IR 与编译回归拒绝 65 个同时活跃根且不创建产物目录；Rust 运行时回归验证嵌套链、重复根、stale handle、无效根零回收、帧/根总上限、初始化各阶段准入回滚。

构建与执行命令仍为 `cargo build --locked -p wings-compile -p wings-vm --bins --examples` 和 `python3 scripts/verify_scalar_aot.py`；release 增加 `--release` 与 `WINGS_PROFILE=release`。本机 macOS AArch64 已通过 debug/release 原生行为验证、workspace 测试、clippy 及 Rust 1.85 all-target 检查；Linux x86-64 待本分支 CI 独立验证，不沿用历史结果。此为内部 IR 的有界临时堆，不是语言业务堆或全部根证明，也没有 BEAM 性能对照，见 [ADR-0025](adr/0025-aot-record-roots.md)。

## 语言临时记录回归

`examples/records.wings` 经共享前端检查、AOT 编译及 CLI `--heap-records` 执行，校验 GC 保活、候选提交、缺少堆配置、堆满/算术失败、initializer 内建、CLI 数值/重复/未知选项拒绝。`language_records` 在受限 OS 子进程验证两个带堆 worker actor 的请求/回复、逐步交错、堆满退出不影响 sibling、挂起 stop 与三级清理。Rust 回归覆盖调用类型/数量/遮蔽、畸形及节点限额、引用边界、LSP 局部类型/跳转/补全、初始化失败回滚，以及堆实例不能绕过计费池。仍由既有 `cargo build --locked -p wings-compile -p wings-vm --bins --examples` 和 `python3 scripts/verify_scalar_aot.py` 驱动，release 使用原有 profile 参数。此切片不覆盖引用状态/消息、循环或完整 MVP，见 [ADR-0026](adr/0026-language-temporary-records.md)。

语言临时记录切片在 2026-11-13 本机 macOS AArch64 通过 workspace 测试、check/clippy/fmt、Rust 1.85 all-target 检查及 debug/release 行为脚本；BEAM oracle 另通过 11 个行为及 23 个拒绝输入用例，不含 Wings 性能测量。Linux x86-64 仍待本分支独立验证，不引用历史 CI 作为新切片证据。

## 持久记录状态回归

`examples/record-state.wings` 通过 CLI 验证初始/替换状态、GC 后读取、无堆拒绝、单槽替换堆满及失败无成功输出；`persistent_records` 验证 128 次二槽复用、逐步恢复、最后检查点 Drop、worker 多实例请求交错、失败隔离、标量诊断拒绝及三级清理。脚本的独立受限 `--check-record-roots` 子进程在所有慢路径（含最终提交检查点）及内建前主动收集，验证 initializer 与只用于候选输出的引用均被 compiler 发布，成功返回链为空。该强制收集是测试宿主，不是新增后台 GC API。

执行命令沿用 `cargo build --locked -p wings-compile -p wings-vm --bins --examples` 及 `python3 scripts/verify_scalar_aot.py`，release 使用 `--release` 和 `WINGS_PROFILE=release`。2026-11-13 本机 macOS AArch64 通过 debug/release、workspace 测试、check/clippy/fmt 与 Rust 1.85 all-target 检查；Linux x86-64 待独立验证。本切片不包含 BEAM 性能测量，契约见 [ADR-0027](adr/0027-persistent-record-state.md)。

## 带堆监督回归

同一行为脚本的 `record_supervisor` 使用 RecordState 源码产物，验证两次全新堆重建后强度耗尽、旧提交状态与候选不迁移、失败后的排队请求不重放、旧请求终结、sibling 状态和挂起 stop 清理。Rust 回归验证显式容量、三级额度、首次创建回滚及第二次 initializer 在新堆分配后失败时释放旧/新堆且不再重试。命令与资源限制同上，契约见 [ADR-0028](adr/0028-supervised-record-heaps.md)；双平台结果独立记录，不作为性能结果。

2026-11-13 本机 macOS AArch64 通过本切片 workspace 测试、check/clippy/fmt、Rust 1.85 all-target、debug/release 行为脚本与 VM release 测试（74 个 VM 单元、17 个堆及 15 个资源用例）。本机 Docker daemon 未运行，没有可复用的 Linux x86-64 验证环境；Linux 仍待本分支独立 CI，不启动额外 VM 或用模拟结果代替原生性能验证。

## 条件表达式回归

`examples/branches.wings` 和 `conditional_execution` 由同一受限脚本验证两臂/嵌套、未选错误/分配不发生、实际路径预算、跨分支保活及分支内临时记录 GC、逐步恢复、取消/Drop 清理。`--check-branch-roots` 另在每个内建及慢路径主动 GC，防止只测显式 collect 掩盖漏根。IR 回归覆盖分支类型、配对/重复/缺失、64 层、跨臂/作用域泄漏与产物写入前拒绝；前端覆盖畸形输入和后续 handler 恢复。契约见 [ADR-0029](adr/0029-scalar-conditional-expressions.md)。

2026-11-13 本机 macOS AArch64 通过 workspace 测试、check/clippy/fmt、Rust 1.85 all-target 和 debug/release 原生行为脚本；持久状态另覆盖两方向分支中的临时分配与 GC。Linux x86-64 尚待独立验证，没有性能测量或 MVP 达标结论。

## 标量比较回归

`examples/comparisons.wings` 由既有行为脚本验证六种 Int 比较（含 i64 两端、相等、反序）、四种 Bool 相等输入、Unit 相等、比较结果驱动 Max 分支、记录字段比较及恰好足够/不足一步的预算。前端/IR 验证词法、优先级、错误/混合/引用类型和无效操作数。命令及子进程限制与条件回归相同，见 [ADR-0030](adr/0030-scalar-comparisons.md)；没有性能结论。

2026-11-13 本机 macOS AArch64 通过 workspace 测试、check/clippy/fmt、Rust 1.85 all-target 和 debug/release 行为脚本；Linux x86-64 待本分支独立验证。

## 标量循环回归

`examples/loops.wings` 与 `loop_execution` 在既有受限子进程脚本验证零次/多次/嵌套循环、二槽记录复用、外层根跨回边保活、单步恢复不重放、取消/Drop 清理、候选失败不提交、双 actor 有限轮转和计时请求。`--check-loop-roots`/`--check-nested-loop-roots` 在每个慢路径及内建前主动 GC，包含零次分支，检查根、回边及最终链清理。空循环预算耗尽，Count 的零/一/十轮验证恰好足够与少一步预算。

前端/IR/LSP 回归验证语法、64 层、类型、局部作用域、控制配对及内外嵌套根延长；不测宿主无限循环，不承诺硬实时或公平性延迟目标。契约见 [ADR-0031](adr/0031-scalar-state-loops.md)，执行命令仍为既有 workspace 和 debug/release 行为脚本，Linux x86-64 需独立验证。

2026-11-13 本机 macOS AArch64 通过 workspace debug/release 测试、check/clippy/fmt、Rust 1.85 all-target 和 debug/release 原生行为脚本。计时用例不作为抢占延迟测量，BEAM 双目标与 Linux 新代码验证仍未完成。

## 标量编译帧准入回归

ABI v4 新增业务帧前的剩余栈边界回归：`stack_boundary` 在受限子进程对真实 initializer/handler（含高寄存器压力）设置零、过高、距当前 SP 仅 1 KiB 和可用限额，检查错误码 6、零扣步、输出不变及慢路径未调用；原有高压力帧、原栈恢复、取消和 worker 回归继续执行。Rust 回归检查布局、旧 v3 拒绝、guard/慢路径预留关系、同步池满拒绝及初始化计费。2026-11-12 本机 macOS AArch64 已通过 workspace 测试、check/clippy/fmt、Rust 1.85 全 targets 检查及 debug/release 行为脚本；Linux x86-64 仍待本分支 CI 独立验证，不能沿用下述历史结果。它不证明任意 Rust 慢路径或递归栈安全，也不是性能结果，见 [ADR-0024](adr/0024-scalar-stack-boundary.md)。

行为脚本执行 `wings-compile` 的 `frame_limits` 示例，检查零值及超硬上限配置、1 字节限额拒绝，以及 128 个跨检查点活跃标量在 512 字节限额下拒绝；每次失败都确认输出目录不存在。默认 64 KiB 限额下构建并运行高寄存器压力入口，校验结果；既有 48 活跃值的原栈恢复继续运行。用例验证实际后端帧和写产物顺序，不证明 Rust 慢路径栈上界或动态栈安全，见 [ADR-0017](adr/0017-scalar-frame-admission.md)。

该切片已在 `a12461e` 通过 macOS AArch64、Linux x86-64 的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43101893550)。后续变更仍需独立验证；CI 的 MSRV 检查同时覆盖所有 targets，避免行为示例脱离 Rust 1.85 基线。

## OS 唤醒原语回归

本分支活栈、owned task、资源守恒及 OS 唤醒已分别通过 macOS AArch64 与 Linux x86-64 的 [debug/release CI](https://github.com/wsafight/wings/actions/runs/43101080186)，另通过 Rust 1.85 检查。该结果对应 `a446119`，不作为后续编译帧准入或 BEAM 工作负载的验收。

同一行为脚本在受限子进程运行 `worker_wakeup`，验证零超时、无通知超时、提前通知、通知合并、跨线程通知、256 次发送/等待交错、四个生产者各 4096 次通知、等待端线程移交和 handle 释放顺序，另反复创建/释放 128 次。循环、线程数与等待时间有固定上限，不做无约束 FD 耗尽。debug/release、macOS kqueue 与 Linux epoll/eventfd 分别执行；信号打断及内核错误注入仍未验证，不把正常通知回归当作完整 reactor 或调度器证明。契约见 [ADR-0016](adr/0016-worker-wakeup.md)。

## 独立资源账户回归

未发布源码的 `resource_accounts` 示例保留 [ADR-0014](adr/0014-resource-reservations.md) 的单线程独立模型；[ADR-0045](adr/0045-concurrent-resource-admission.md) 的 `concurrent_resources` 测试补充多线程准入、转移与归还。两者都不是所有运行时分配的接入验收或完整资源隔离证明：

- 定点用例检查合法层级、三级分别拒绝、四维额度、零预留、父 handle 提前释放及 Drop 回收；每个维度分别在 VM、租户、进程层制造 `u64` 加法溢出，拒绝后逐级核对零变化。
- 固定种子 `0x0014_5eed_cafe_beef` 驱动 4096 步预留、转移和 Drop，固定两个 VM、各两个租户及每租户两个进程，最多 24 张活跃票据；VM 与租户也可直接持有预留，零预留参与序列。
- 独立模型按票据所有者求和，再从子账户向祖先汇总四维用量；每步比较全部账户、额度与票据金额，核对操作成功与否，失败比较全账本快照且不改变模型所有权，最后逐张释放并确认所有账户归零。
- 转移定点用例覆盖同账户、共同祖先已满、祖先目标、跨租户与跨 VM 拒绝；固定序列另断言预留和转移的成功、失败及 Drop 均实际发生。
- 并发测试以最多八个线程、固定轮数覆盖三级四维争抢和溢出拒绝、满共同祖先净转移、拒绝零变化、唯一票据跨线程 Drop 及四维快照；内部单元测试验证锁中毒恢复和不同 VM 不共用门闩。尚未验证真实分配失败、全部退出清理接入、OS OOM 或争用性能；账本原子性不能代替多 worker 的唯一执行权验收。

专项构建：`cargo build --locked -p wings-vm --example resource_accounts`，产物为 `target/debug/examples/resource_accounts`；加 `--release` 后为 `target/release/examples/resource_accounts`。上面的验证脚本已在禁 core dump、20 秒 CPU 限制、30 秒墙钟超时的独立子进程运行此示例；两个平台的 debug/release 结果须分别记录。此序列分配有固定上限，不是内存耗尽压力测试。

## 原生栈分配实验

本节与后续栈池/共享模块数字来自 ABI v4 之前的切片；当时同步调用及初始化使用宿主栈。当前已统一池内执行，新增切换、预热和缓存成本尚未重测，不能将这些历史数据直接作为当前 ABI v4 的内存或性能结果。

2026-11-12 在 macOS 26.6.2 / Apple M3 Pro / 36 GiB / 16 KiB 页、Rust 1.99.1 release 上测试 `corosensei 0.3.3`。每种条件用独立 OS 子进程重复 3 次，数量为 512/2048；先测基线，再测分配/挂起，最后正常返回释放。`ps` RSS、`footprint` 的 `phys_footprint` 与 `vmmap` region 分开记录，以下为 2048 个实例扣除本进程基线后的中位数，不是单实例 allocator 字节数：

| 条件 | 物理增量 | 虚拟增量 | 每实例物理增量 |
|---|---|---|---|
| 浅挂起栈，32 KiB 容量 | 32.19 MiB | 96 MiB | 16.09 KiB |
| 浅挂起栈，64 KiB 容量 | 32.22 MiB | 160 MiB | 16.11 KiB |
| 浅挂起栈，256 KiB 容量 | 32.41 MiB | 544 MiB | 16.20 KiB |
| 256 KiB 容量，16 个各 4 KiB 的活跃递归帧 | 160.41 MiB | 544 MiB | 80.20 KiB |
| AOT 实例 Idle，无独立栈 | 1.44 MiB | 本次增量 0 | 0.72 KiB |
| AOT 调用挂起，48 个跨检查点活跃标量 | 33.98 MiB | 544 MiB | 16.99 KiB |

浅栈三种容量的 RSS 增量均为 32.125 MiB；独立栈使 VM region 增加约 4096 个，另有少量 allocator 差异。浅栈正常释放后物理增量回到约 128 KiB；AOT 实例释放后仍有约 1.72 MiB allocator/装载等残留，不能把它全算为泄漏或全算为已归还。未启动协程也已触碰栈顶一页。此实验没有 BEAM 同业务对照、GC、mailbox 或三级账户，不证明完整 VM 更省内存。

结论：暂保留已验证的切换实现，按 [ADR-0012](adr/0012-bounded-stack-pool.md) 自研分配/准入/复用/回收。单纯缩小预留不能显著降低浅栈 RSS，也不能用栈池消除挂起栈。低纳秒热栈微基准不代表调度延迟或吞吐，不作为选型门槛。

复现：`cargo build --release --locked -p wings-compile -p wings-vm --bins --examples`，再执行 `python3 scripts/measure_stacks.py --output target/stack-measurements-new --repetitions 3`。驱动限定每子进程 30 秒 CPU、60 秒测量墙钟、禁 core dump、最多 2048 个栈，最大栈映射 544 MiB，阶段检查 RSS 不超过 384 MiB。当前 macOS 拒绝 `RLIMIT_AS/DATA/RSS`，因此这不是 OS 硬内存配额或 OOM 实验；分配量由程序上限约束。原始 `vmmap`/`footprint` 和 JSON 保存在本地 `target/stack-measurements-run4`；精简结果随仓库记录，Linux 不复用本机数据。

### 有界栈池复测

同机、相同工具链及 48 活跃标量 AOT 负载，512/2048 调用、三次独立子进程重复。2048 个同时挂起时，零缓存/缓存 2 槽/缓存全部的物理增量中位数均约 33.95–33.99 MiB，峰值没有实质下降。完成后分别约 1.72 / 1.78 / 33.99 MiB；缓存全部经 `trim` 后降至约 1.70 MiB。零缓存完成后 VM region 增量约 3，缓存全部仍约 4101。池证明了缓存/回收策略影响完成后的内存，不能减少同时挂起的活页。

复现同一构建命令后执行 `python3 scripts/measure_stacks.py --suite pool --output target/stack-pool-new --repetitions 3`，原始结果在本机 `target/stack-pool-measurements`。未测受限重映射失败注入，未与 BEAM 同负载比较，不把本结果作为 MVP 性能验收。正确性脚本增加 `stack_pool` 示例，覆盖容量拒绝、即时归还、独占、缓存、trim、取消和所有权生命周期。

### 共享模块复测

同一台 macOS、release、相同 48 活跃标量产物，512/2048 实例、三次独立子进程重复。2048 个 Idle 实例若每次装载模块，物理增量中位数约 1.53 MiB；复用一个已预热模块约 128 KiB（最小 112 KiB、最大 128 KiB），实例固定头部为 24 字节，其余差异包括 Vec 容量、allocator 页与固定成本。共享模块创建的挂起调用约 32.63 MiB，仍由每栈触碰页主导。不把 24 字节称为完整微进程成本，也不把初始化耗时差异称为业务吞吐提升。

复现：同一 release 构建后运行 `python3 scripts/measure_stacks.py --suite shared --output target/shared-module-new --repetitions 3`，原始结果在本机 `target/shared-module-measurements`。`shared_module` 行为示例验证状态、失败和 initializer 独立，以及 ID 身份、生命周期和兼容接口。尚无 BEAM 等价业务对照。

## 优化实验规则

- 各项[激进优化](optimizations.md)开启和关闭时分别与正确性基线比较，保留生成代码、目标特性、配置、原始结果和回归样本。先测单项，再测组合。
- 同时记录吞吐、p99/p999、RSS、物理占用、CPU 时间、分配量、消息字节数、代码体积和最大不可抢占区段；平台允许时追加缓存未命中、分支与跨核通信等计数。
- 测试包含冷启动、小负载、热点收件人、稀疏百万进程、GC/唤醒突发和长期负载，不只报告命中快路径的平均值。平台专用优化分别开启和关闭，比较时不关闭压缩、系统保护或 BEAM 优化。
- 指针压缩、所有权转移、GC 根位置、额度租借、检查合并和 mailbox 无锁协议增加溢出、取消、退出、迁移与回收交错测试；不能只凭微基准决定启用。
- 改变默认值前，更新对应契约，写清收益范围和回退方式。某个首要平台不支持优化时，保留语义等价的可移植路径；涉及语言、ABI 或 GC 模型的变更单独版本化。

## 相关文档

- [产品定位](product.md)定义目标负载与产品价值。
- [运行时](runtime.md)定义需要验证的行为契约。
- [风险与里程碑](roadmap.md)将验证要求对应到各阶段。
- [激进优化](optimizations.md)列出各优化的依赖、风险和实验顺序。
- [Crate 拆分](crates.md)定义编译器、VM 与语言服务器的依赖边界。
