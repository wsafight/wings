# Wings VM 协作约定

本文件适用于整个仓库。

## 项目现状

- `a7812ec` 后 TCP 原限专项两构建各两轮/36 次握手通过，但 Debug 完整复验在 observed_yield 原十秒入口无输出超时；冻结该入口两构建定向又通过。仅首次 Debug TCP/观察入口的采样进入模块 dlopen/fcntl，不能归因原失败或宣称修复；原失败、原限与证据见 `docs/validation-startup-followup-2026-11-18.md`。不继续盲重试换取全绿，MVP 门禁不变。

- ADR-0137 补无限 handler 下同 worker 健康进展的真实 AOT 前置回归：一步片/忙总预算为 u64::MAX，一/双 worker 各八个健康 actor、16 轮；固定 256 槽让出 FIFO、初始原子确认、开放态旧身份/槽复用与退账独立重放。最终两种构建各 106 harness/707 项、定向进展、Release 完整 AOT 及工程/MSRV/Linux 交叉检查通过；本轮 Debug 完整 AOT 在后续 TCP 5 秒启动失败，原限与证据保留，见 `docs/validation-atomic-progress-2026-11-18.md`。不改变原业务/ABI/预算，不关闭完整公平性、原生双平台或 BEAM/MVP 门禁。

- ADR-0136 将计费模块的 canonicalize 改为调用者提供固定来源/`PATH_MAX` 输出的原生 realpath，先验证再按已知长度构造 Rust 结果；原控制/映像预算、ABI v22/4352 B 和工具加载行为不变。初次非 UTF-8 目录前提与 Clippy 常量断言失败保留；修正后两种构建各 105 harness/707 项、十二场景分配探针及完整 AOT、工程/MSRV/Linux 交叉检查通过。libc 内部暂存与完整账本、双平台和 BEAM/MVP 门禁仍待完成，见 `docs/validation-native-paths-2026-11-18.md`。

- ADR-0135 修复计费模块的大路径来源暂存：原 2 MiB 控制准入后、canonicalize 前无分配检查原始编码字节至多 16 KiB，解析后再检查才拼接；固定错误，不放宽 CLI 参数/映像预算。修改前十二场景复现 ASCII/非 UTF-8/NUL 3 MiB 来源各产生 3145729 B Rust 分配，修复后各一次 38 B 固定错误；Debug/Release 各十二场景、105 harness/702 项及完整 AOT、工程/MSRV/Linux 交叉检查通过。原生 canonicalize/libc/装载器完整账本及原 MVP 门禁仍保留，见 `docs/validation-module-paths-2026-11-18.md`。

- ADR-0134 的 `74678b9` 冻结两对原 hot-queue 80000/s 长测及独立重放已完成：开启轮 worker CPU 合计 87.216872916/90.976441875s，仅覆盖 worker 函数；四轮采样队满 13444/12153/31105/22714，第二开启轮保留 108 pending 满及 27 超时，首开启预热保留 282632 队满。全部实际提交/清理正确，仍不作 CPU 路径归因、优化收益、BEAM 双目标或 MVP 通过声明，见 `docs/validation-worker-cpu-2026-11-18.md`。

- ADR-0134 在原可选锁统计中增加固定计费的 worker 函数 CPU 区间：入口/退出两次线程时钟、默认无 CPU 读时钟，单原子发布 Pending/Unavailable/实际纳秒；不含 timer/发生器或 OS 线程启动退出，也不是采样期/锁内 CPU。Debug/Release 各 104 harness/702 项及全量 AOT、九项 Python 模型、工程/MSRV/交叉检查通过，实际两对长测另记。ABI v22/4352 B 与原业务/预算保持，验证状态见 `docs/validation-worker-cpu-2026-11-18.md`；完整 MVP 门禁不变。

- ADR-0133 冻结 `e2fe7de` 的 hot-queue 80000/s 两对锁诊断已完成：原 30s/60s、关闭/开启与反序，逐条重放和账本清理通过；采样队满依次 14180/18734/14845/43489，无超时，两对扰动方向相反。开启轮 claim 各约 2300 万次、四阶段累计持锁约 11.8s，都是完整生命周期墙钟，不作 CPU 或吞吐收益结论；下一步补 CPU 范围，保留原失败与全部 MVP 门禁。

- ADR-0133 为原 `beam_open_loop --measure` 增加显式 `--observe-locks`，join 后输出已计费四阶段 worker 锁累计并独立核对 Configure+实际提交的 handler 数；覆盖完整 Scheduler 生命周期，不称 CPU 或采样阶段分布。默认关闭、原业务/ABI v22/4352 B/资源/时段不变；六项模型、两种构建示例各七项、CLI 拒绝、工程/MSRV/交叉检查及顺序 Debug/Release 全量 AOT 通过；初次 Debug 的原 30s worker_ingress 超时保留，显式冻结两对长测另记。完整账本、启动根因、过载/服务 I/O、原生双平台与 BEAM/MVP 门禁仍待完成，见 `docs/validation-open-loop-locks-2026-11-18.md`。

- ADR-0132 修正 v2 多 actor 验证宿主的退出后才限输出与超时证据丢弃：非阻塞 I/O 在读取时执行原 1 MiB/64 KiB 上限，所有管道和进程等待共用原十秒期限，失败保留有限输入/用例身份、双流前缀/EOF 和清理前后退出值，清理整个自建进程组。七项短测试、Debug/Release 真实三端 18/17/49 用例及共享调用器的七个 release Wings/BEAM 入口回归通过；原 Debug 全量十秒超时仍保留且根因未定，原业务/ABI v22/资源限制不变。完整账本、最新原生双平台和 BEAM/MVP 门禁仍待完成，见 `docs/validation-multi-counter-evidence-2026-11-18.md`。

- ADR-0131 增加真实 OTP 29 的有限请求和取消/恢复对照：票据保留至 caller 消费且 publisher 实际回复/退出，取消 Fail 仍实际除零；每目标独立 OTP 监督分支保留一次/60s 的按 child 强度范围，健康 sibling 不受牵连。Debug/Release 两端一/双配置各三轮、75 组伪造拒绝、CLI 拒绝与 Release 完整 AOT 通过；冻结 ae5706d 的双端一/双配置各 900 轮、30s/60s 与独立重放通过，共 3600 轮。本轮 Debug 完整 AOT 的既有多 actor 子进程在原 10s 超时，证据保留；ABI v22/容量/原限不变，完整账本、过载/服务 I/O、原生双平台与 BEAM/MVP 门禁仍未完成，见 `docs/validation-beam-recovery-2026-11-18.md`。

- ADR-0130 为计划开环和 CPU/心跳长测增加可选成对 CPU quota/period 参数，只读核对预配置的独占 Linux cgroup v2 leaf domain；范围含 driver、runtime 和 probe helper，记录祖先配置/成员/策略/cpu.stat 并在变化时保留失败。无参数保持原探索模式，不写/创建/迁移 cgroup、不提高任何业务/时间/资源上限。十项模型、原 Wings/BEAM 长测入口回归与 macOS 早拒绝通过；原生 Linux 执行/节流、观测偏差和 CPU/MVP 门禁仍未验证，见 `docs/validation-cpu-quota-2026-11-18.md`。

- ADR-0129 为计费模块限定系统运行库依赖：Mach-O 只接受普通 libSystem load，ELF 只接受 libc SONAME 或无依赖；额外库/加载重定向与错误动态表在装载前拒绝，文件/控制票据完整退账。真实只读依赖专项、两种构建各 698 项、MSRV 与 Linux 交叉检查通过；本轮完整 Debug 在 control_tokens 原 3s 阶段失败，Release 在 TCP readiness 原 5s 超时，三次正常启动诊断未捕获根因。ABI v22/额度/时限和显式非计费工具入口不变，完整账本、原生双平台与 BEAM/MVP 门禁仍未完成，见 `docs/validation-module-dependencies-2026-11-18.md`。

- ADR-0128 在计费模块装载前解析 ELF64/Mach-O64 段表，预算改为原 2 MiB 控制加文件页/原生页对齐映像跨度的较大者，含零填充和空洞；映像独立上限 64 MiB。先由文件票据保护快照，释放后追加不足预算，末代码 owner 交还 Library 后退账。原 ABI v22/执行/账户限制不变；两构建各 695 项及 Release 完整 AOT 通过，Debug 完整 AOT 的 TCP 启动 5s 超时仍保留。逻辑映像不是 RSS/装载器完整成本，完整账本与原生双平台/BEAM/MVP 门禁仍待完成，见 `docs/validation-module-image-2026-11-18.md`。

- ADR-0127 新增 `string_at(String, Int) -> Int` 与 `string_slice(String, Int, Int) -> String`，按 Unicode scalar value 操作，切片独立计费；索引/长度负数、溢出和越界返回独立受管错误。ABI 升至 v22，context/EffectRequest/4352 B 预留、单值 64 KiB 与原阶段限额不变。定向 Debug/Release AOT、三层配额/GC/取消、两种构建各 691 项测试及 Release 完整 AOT 通过；Debug 完整套件的 TCP/Counter/fairness 超时仍保留，记录见 `docs/validation-string-slicing-2026-11-18.md`。完整账本、原生双平台及 BEAM 门禁仍未完成，不代表 MVP 完成。

- ADR-0125/0126 为原百万 checked 加法/100 突发心跳增加独立恢复区间诊断。v12 首轮约 51.65s 触及 800 万历史上限并保留失败；v13 不提高上限，分别披露 FIFO/历史丢样并继续原 30s/60s 业务。独立重放核对时钟/业务配对与可观察前缀，缺样不报完整分布；默认 AOT 不自动跑长测，原 v7/v8、ABI v21、资源和阶段限额不变，MVP 门禁仍待完成。

- ADR-0124 为 Scheduler 增加默认关闭的 slice_samples，启用后按 worker 固定计费 FIFO 记录实际 resume 墙钟区间、完整 Pid/序号和三种结果。满则丢新样、计数饱和显式标记，末 handle 清理；与原子请求让出延迟分开，不修改 ABI v21、片预算或 quantum。短时一/双 worker 回归不替代长时公平性、CPU 硬配额、原生双平台或 BEAM/MVP 门禁。

- ADR-0115 为固定跨线程 ingress 增加 MessageValue send_values/request_values，支持计费 String/Bytes/IntList/RecordInt 内容复制，不传原引用；原标量 API 继续拒绝引用整数。64 参数/64 整数/合计 64 KiB、原容量与时限不变；超时/取消/Drop 不撤回消息，目标原栈分块导入，满/关闭/失败清理保留原租约。ABI v21/4352 B 不变，仍非通用 reactor、完整账本或 MVP。

- ADR-0114 为纯标量实例保留创建时的 process 账户身份，直接调用及 detach/attach 都不能改借兄弟账户、其他 tenant/VM 或工具池绕过原额度；同账户可换池/线程。拒绝前不借栈、不创建控制 token、不消费镜像；末实例/镜像保留账户节点租约。ABI v21、4352 B 控制预留及原容量/时限不变，完整账本与 MVP 门禁仍未完成。

- ADR-0113 开放具名记录单向/请求参数，源码名义类型检查后有界展开为既有叶值，目标业务前计费重建独立记录；空记录及重复子树按出现次数计入合计 64 节点界，列表/字节原合计上限不变。宿主用清单叶签名与 MessageValue，不传原 handle；记录回复仍未开放。ABI v21/4352 B/原容量时限不变，完整账本与原生双平台/BEAM/MVP 门禁仍待完成。

- ADR-0112 将 TCP 启动读取限制为原 5 秒绝对 deadline/4096 B 完整握手，失败保留退出状态/有界管道前缀并清理独立进程组；6 项注入测试，原业务/CPU/墙钟/ABI 不变。一次独立采样停在 `_dyld_start`，不将它认定为所有间歇超时根因；复验不抹去失败，MVP/双平台/BEAM 门禁仍未完成。

- ADR-0111 开放 String 按完整 UTF-8 内容 `==`/`!=`，独立 Bool IR/heap opcode，每至多 256 B 检查预算/取消/让出，两个源精确根保活，不分配业务存储。不做规范化/排序/隐式转换，其他引用仍不比较。源码 ABI v21，context88B/EffectRequest80B/4352B控制预留及原额度/时限不变；具名记录传输、完整账本和原生双平台/BEAM/MVP 门禁仍未完成。

- ADR-0110 将全目标测试有界运行器纳入仓库/双平台 CI，`scripts/verify_workspace.py` 先编译清单再实际执行，CPU 20s/墙钟 30s/core 0，指纹/原始输出/失败报告保存在全新目录且不重试；doctest/AOT 单独验证。不把 CI 配置或交叉检查当最新原生平台结果，MVP/BEAM 双门禁仍未完成。

- ADR-0109 将 OS 唤醒票据内嵌共享端点，增加 WorkerParker::new_accounted，ScalarWorker/ingress 不再平行复制独立票据；最后 notifier 关闭 FD 后归还原 4096 B，Linux poller 先于端点释放。修复保留通知者但 worker 退账的确定性回归，ABI v20/4352 B 预留和原额度/时限不变；工具 new 不计费，仍不代表完整账本、原生双平台或 BEAM/MVP 门禁完成。

- ADR-0108 接入本地/state/fn 具名不可变记录，每 process 最多 32 schema、每种最多 8 字段，可引用更早记录及既有业务值，拒绝递归/前向/能力字段和消息/回复具名记录。`record` 只在声明位置识别，不保留旧变量名；布局表计费、名义布局独立、精确根/GC/取消和失败旧提交保持。源码 ABI v20，原 context/EffectRequest/4352 B 预留不变；具名记录传输、完整账本、公平/故障、最新原生双平台及 BEAM 双门禁仍未完成。

- ADR-0107 增加 CancellationToken/PreemptionToken 显式 `new_accounted`，共享原子存储先准入、最后 token/调用 owner 清理；ScalarWorker 常驻标志与 Scheduler actor 取消分别绑定 tenant/process，移除 actor 原 64 B 合并预付。默认工具/内部调用控制边界不变，保留4352 B/ABI19及原额度/时限；通用记录、完整账本、原生双平台和 BEAM 双门禁仍待完成。

- ADR-0106 开放计费 String 回复、独立 StringReply/ReplyString/CompleteString 和匹配完成事件，非成功终态导入空字符串；出口根/分块复制与 UTF-8 校验、末结果租约清理保持。源码 ABI v19，EffectRequest 80 B/context 88 B/控制预留 4352 B 不变，旧产物须重编译；通用记录、完整账本及原生双平台/BEAM 双门禁仍未完成。

- ADR-0105 开放 String 语言/宿主消息参数，与 Bytes 共用单条合计 64 KiB、独立掩码与计费副本；源码 ABI v18、EffectRequest 80 B/context 88 B，控制预留仍 4352 B，旧产物须重编译。目标分块校验 UTF-8 并保留入口根，String 回复、通用记录、完整账本及原生双平台/BEAM 双门禁仍未完成。

- ADR-0104 补双引号 UTF-8 字面量及六种显式转义，单值解码至多 64 KiB、IR 合计至多 1 MiB；常量存只读模块段，每次执行先三级准入再分块复制为独立 String，保留 GC/取消/预算与模块租约。源码 ABI 为 v17，context/EffectRequest 布局不变，旧产物须重编译；String 消息/回复、通用记录与完整账本及双平台 BEAM 门禁仍未完成，不改待审大小写提案。

- ADR-0103 接入独立计费 UTF-8 String 的本地/state/有界 fn 值、Bytes 显式复制转换、Unicode scalar value 计数和拼接，编码上限仍为 64 KiB，分块检查/精确根与非移动 GC 保持。源码 ABI 升至 v16，context/EffectRequest 布局不变，旧产物须重编译；尚无字符串字面量、消息/回复或通用记录，不改变类型大小写提案。MVP 仍缺业务值交互、完整资源账本、长时公平/故障、最新原生双平台及 BEAM 吞吐/整机内存双门禁。

- ADR-0102 增加默认关闭的 worker 锁区间统计：固定计费每 worker claim/start/snapshot/finish 的次数、获取前墙钟/锁内墙钟总和与最大值，饱和标记，不保存逐条业务对象。未启用不额外读时钟/更新统计；不覆盖 park、宿主/effect/账本/结果/timer 锁，不将墙钟当 CPU 或 p99。功能验证另记，ABI/原预算/时限和 MVP/BEAM 门禁不变。

- ADR-0101 的空语言请求表维护快路径功能通过，但 hot-queue 10000/s 五对采样候选 86 次超时、1350 次队满（原版 0/475），无稳定收益，已回退计数和提前返回、恢复 `8cc7878` 代码。冻结候选/726 个输入及十轮原始数据保留，不改时限/预算；MVP/原生双平台及 BEAM 双门禁未完成。

- ADR-0100 的计时通知合并候选在 hot-queue 10000/s 原规模五对中无稳定收益，采样阶段候选新增 1240 次超时、队满 41747 次（原版 2485），已回退 timer 实现，保留原无条件 arm/disarm 通知。功能通过不替代性能，失败实验/原始数据保留；ADR-0099 轮询快路径不受影响，MVP/BEAM 双门禁仍未完成。

- ADR-0099 为宿主结果表增加固定计费每槽原子就绪标志；期限前未就绪 `try_take` 免锁，真实终态、取消/回收和槽代次仍由原锁保护。新增控制存储从原额度支付，不改 ABI v15、4352 字节控制预留或时限；收益须配对测量，不将其当作全部尾延迟根因或 MVP/BEAM 双门禁通过。

- ADR-0098 接入 0..=64 KiB Bytes 回复、原协程出口根/分块导出和独立计费结果；完成信封只持不可变结果租约，在发起方另计费分块导入，不在 Scheduler 锁内整块复制。ABI v15、4352 字节控制预留与原额度不变；字符串/通用记录、完整账本、长时公平/故障、原生双平台及 BEAM 双门禁仍未完成。当前平台证据见 2026-11-16 验证记录，不以历史通过代替最新验收。

- ADR-0077 的独立 v8 长测沿用百万次 checked 加法与 100 actor 每 10ms 同刻心跳，30s/60s、五对重复；运行端固定 buffer 落盘，独立逐条重放及每 actor 延迟。本机默认五对已完成，Wings 第三轮保留 291 次队满，BEAM 无拒绝/超时；独立观测只有两条预热样本，采样 p99 为 null。请求/确认时钟各自单调，合法区间重叠不能拒绝；原 v7 短测和 ABI v15 不变，仍缺 CPU 硬配额、过载分组、原生双平台与 BEAM 双门禁。语法草案仍待审。

- ADR-0076 接入 Bytes 语言消息参数，单条合计至多 64 KiB；源暂存计费、发布时向目标净转移、目标导入另计费并分块检查/保留入口根。未发布 ABI 为 v15，EffectRequest 72 字节，旧产物须重编译；原语法大小写不变，语法草案继续待审。执行控制统一预留 4352 字节，不提高资源上限；完整业务值/账本和 MVP 门禁仍未完成。

- ADR-0075 为 v6 长测增加冻结的原生 C 内存 probe：Linux smaps_rollup/PSS、macOS rusage physical footprint、主机压缩/swap 分列；有界 census/身份/错误保留，不完整总数为 null，不把主机值归因给 VM。原时段/重复/负载和配额不变，仍缺完整内存口径/监测偏差/CPU 硬配额与原生双平台门禁，语法草案仍待审。

- ADR-0074 新增真实 AOT 的一/双 worker 并发恢复回归：两个发送者排满 Bytes actor，已超时/取消结果不撤回消息或提前释放 publisher；stop/取消竞争只承诺单一合法终态。取消 Fail 结果仍须实际失败、保留旧提交状态、一次全新重启后强度超限。固定容量/原时限不变，不替代完整并发故障/原生双平台或 BEAM 双门禁，语法草案仍待审。

- ADR-0073 允许宿主批量 spawn 显式借用同 tenant 的计费初始化栈池；只同步借用，不存入 Scheduler/supervisor，自动重启仍走原临时池。v2 保留原规模、配额与 3 秒阶段，池创建也计时；缓存不等于 RSS 消失，回归不替代原生双平台或 BEAM 双门禁，语法草案仍待审。

- ADR-0072 把原子让出观测接入 Scheduler：`yield_samples` 默认 0，每 worker 可预先计费固定 FIFO；真实切回后带完整 Pid/worker/序号发布，满则计丢样，不覆盖或等宿主。请求/合并/消费/清除/确认计数保留，v7 独立 `--observe` 不改原业务/片预算/quantum，零样本不报零延迟。仍非长时公平性或 MVP，通过情况见验证记录；语法草案仍待审。

- ADR-0071 增加显式计费的可选原子让出观测：只在启用时记录首次未消费请求与最近真实切回 worker 的时间；默认无时钟/mutex 热路径，不改 ABI v14。独立 AOT fixture 不是 Scheduler 计时线程的完整测量，单样本和心跳 pending 重叠都不构成公平性门禁。语法草案仍待审。

- ADR-0070 新增短时 fairness 前置：101×256 B actor、同 worker 的六层受管函数恰好百万次 checked +1，其余 100 actor 每 10ms 同刻突发心跳，mailbox 1、实际冷 pending 255 加热 pending 1。默认 AOT 仅 Wings/模型；单 worker/单 BEAM scheduler 不是 OS 单核配额，CPU pending 区间不是实际运行/让出时间戳，不将其称完整公平性或 MVP 通过。语法草案仍待审。

- ADR-0069 新增独立 `--measure`：固定 30 秒预热/60 秒采样/五次配对重复，原始 40 B 事件固定缓冲写文件、离线精确分位和有界进程树 RSS 观测；原 smoke 限制不变。只执行冻结副本，缺少 CPU 硬配额、平台完整内存及原生双平台时不作性能门禁通过声明，语法草案仍待审。

- ADR-0068 接入四组代表负载的短时计划开环和逐条迟发/拒绝/超时；最多 256 实际未收结果的 pending，超时不释放旧 pending、不取消远端提交，最终收齐并核对 state。模式只叫 smoke，默认 AOT 不自动跑 OTP；原 CPU/墙钟/3 秒阶段不变，不替代长时采样、fairness、原生双平台或 BEAM 吞吐/整机内存门禁，语法草案仍待审。

- ADR-0067 增加可信 BEAM 对照端发送前 CAS 有限准入、actor 开始时归还及终态关闭，不以原生无界 `!` 代替 mailbox 限额。独立 v4 在 1/100/256 排满、健康 4 KiB actor、停止/旧入口拒绝上与 Wings 核对；默认完整 AOT 仍不自动执行 OTP，真实三端须 `--compare-beam`，不是开环/性能门禁。

- ADR-0066 将 Scheduler 可配置 mailbox 上限从 64 扩至 256，其他默认/显式容量不变；创建前按实际固定槽数三级准入，全局结果/语言 pending 仍各最多 256。100 槽满队列 AOT 验证拒绝、健康 actor 与取消清理，仅补 hot-queue 前置，不等于开环/BEAM 等价准入或性能门禁通过。

- ADR-0065 新增独立 v3 正确性协议：两个真实宿主线程/BEAM 发送进程，各 16 请求窗口，完整 Bytes 内持久核对各发送者到每目标序号；独立模型验证合法交错而不强求跨发送者全序。只覆盖 1..100 actor、安全加法集合的闭环，不替代 v2 大容量、并发故障、开环/超载或 BEAM 吞吐/内存双门禁；语法草案仍待审。

- ADR-0064 将 SchedulerSupervisor 创建/查询/停止改为固定空槽链和完整身份校验的槽索引，失败创建不摘链、成功重启保持稳定 child ID；poll 仍扫描固定容量。原 CPU/墙钟和 actor 阶段 3 秒限制不变，不将仿真回归或索引改造称为性能/内存门禁通过。

- ADR-0063 增加 10000×256 B、1000×4 KiB、100×64 KiB 多 actor Counter 的有界闭环正确性对照及 Scheduler 已提交 Bytes 长度/32 B 前缀副本。真实三端仍须显式 `--compare-beam`；只有同发送者到同目标 FIFO，不保证跨 actor 全序。完整 Bytes 不是完整状态导出，仍无开环、双发送者或 BEAM 性能/整机内存门禁结论。

- ADR-0062 将 Scheduler 改为固定 intrusive 就绪队列、本地活栈 FIFO 和空槽链，容量上限 16384 actor；宿主结果与语言 pending 仍各最多 256，mailbox 上限后由 ADR-0066 扩至 256。首次派发按 slot 分布，返回后固定编号迁移，不是自适应窃取；每轮最多认领一个 handler 并恢复一个时间片。`scheduler_sparse` 的 1000/10000 标量探测不替代完整 Bytes 同业务、开环或 BEAM 双门禁。

- ADR-0061 将 Scheduler worker 的业务栈改为按需 guarded mapping，冷借入前完整 process/tenant/VM 准入，缓存净转移计费且最多两个；活栈仍原线程独占至正常返回。公开固定池不变，按需模式统计已映射容量而非潜在上限，均非 RSS。后续容量见 ADR-0062，语法草案待审，BEAM 双目标与 MVP 不因空闲预留减少而完成。

- ADR-0060 将 Scheduler 宿主 Int/Bool/Unit 请求结果改为实例共享的 `config.requests` 总槽数，不再逐 actor/类型预留；语言 pending 表仍独立。每个请求票据仍向原目标 process 三级计费，最后结果 handle 可保留表控制租约但不保留 actor/模块。`scheduler_density` 只测逻辑预留，后续栈/调度安排见 ADR-0061/0062，BEAM 双目标仍待推进。

- ADR-0059 接入固定字节 Counter 的 Wings/BEAM 正确性测试和单 worker 已提交 Bytes 的固定前缀副本诊断；默认 AOT 只跑 Wings/独立模型，真实 OTP 对照须显式 `--compare-beam`。不把逻辑状态/账目清理或有界闭环测试当作性能、RSS、多 actor/开环或 MVP 完成。

- 2026-11-13 增量：当前源码 ABI v14，已有有界 IntList 语言消息、线程安全只读代码租约、固定多 worker 调度及宿主 poll 驱动监督（ADR-0051–0056）；Bytes 按 ADR-0057 支持 0..=64 KiB 不可变计费 state/局部/fn 值、分块复制及精确 GC，ADR-0058 增加 signed i64 大端读写，尚不支持 Bytes 消息/引用回复或字符串。下述早期切片说明不替代各新增 ADR；完整资源账本、原生最新双平台与同业务 BEAM 双目标仍待验收。语法大小写草案待审，不提前迁移类型拼写。

- 项目名为 Wings，对外称 Wings VM，定位是微进程虚拟机；核心目标是在相同业务负载下，性能高于 BEAM、内存占用低于 BEAM。
- VM 用 Rust 实现，独立运行；执行引擎包与命令叫 `wings-vm`，编译工具叫 `wings-compile`，语言服务器叫 `wings-lsp`。`wings-lang` 是业务语言名，不是聚合 crate。名称尚未完成重名与商标核查。
- 当前处于 M0：已有前端、LSP 和可信本机 AOT 原型；语言支持局部/持久 RecordInt/IntList、精确引用条件合流/state while、有界非递归 fn 受管调用帧及受限内建，已有 RecordInt 语言消息复制，仍无列表消息/引用回复、递归/动态派发或通用业务值。未发布源码使用 ABI v11，每层薄入口在业务帧前检查剩余栈；compiler 发布跨帧精确活跃根及候选状态，循环外活跃根延长至回边，宿主持久根保活已提交状态，opt-in 计费堆接入非移动 GC，堆实例同步/显式执行均使用同 process 计费栈池。另有强制原子取消/让出检查、总预算、同线程活栈恢复及有限宿主 worker/mailbox/request/监督和语言自发送/同模块创建与发送（局部 Pid）/整数异步请求与系统完成事件（局部 RequestId）；worker/supervisor 可显式 spawn_with_heap，新实例不保留旧状态。worker 计费 OS 等待可重查就绪/deadline，宿主有单 actor 单入口的有界跨线程标量 ingress，仍无完整 reactor；可选计时线程只请求原子让出，不访问 actor。CLI --heap-records 显式开堆，不是网络 VM。已发布 `0.0.2` 仍是 v1。同步、初始化与活栈调用统一使用禁用 unwind 的 corosensei guarded stack，丢弃挂起调用必须先正常取消返回。完整受管执行、通用消息根、M:N、通用业务值与基准尚未完成，不把切片称为完整 VM 或 MVP。
- 只处理当前任务涉及的内容。文档整理任务不顺带创建代码骨架、依赖、分支或 Git 仓库。
- 受管 GC 已按 ADR-0043 拆成最多 16 节点/根的工作块，每块检查并扣 runtime 预算，允许原栈暂停/取消；heap_stats 为常量时间计数。仅同 actor 不重入的非移动收集，不是并发 GC；其他 runtime 长操作、全部分配计费和双平台性能门禁仍未完成。
- 模块按 ADR-0044 持有 Library 租约，最后一个代码所有者释放后交还装载器引用；load_accounted 在解析/装载前向 VM 预留逻辑预算并校验计费执行的 VM 归属。旧 load_trusted 是不计费工具入口；不提供热更新，不把文件大小预算当 RSS 或动态装载器实际内存。
- 资源账户/票据按 ADR-0045 允许跨线程共享和移动，同 VM 门闩串行提交三级四维准入、净转移、Drop 与 used；锁内无分配/业务/协程，used 仅单账户一致快照。actor/活栈/模块 Rc 租约仍受线程限制，不能由账本 Send/Sync 推断 M:N 已实现。
- 宿主按 ADR-0046 提供单 actor 单标量入口的有界跨线程 ingress；消息发布后 WakeFailed 仍是已接收，不能盲目重试。关闭清理消息，控制/ring/通知计费租约保留至最后 sender；actor/代码租约仍不跨线程，不是通用 reactor 或服务 I/O。
- ADR-0047 可将外部入口配置为固定标量请求模式，IngressRequest 唯一消费结果，超时/取消/Drop 不撤回消息；结果槽/requests 票据在调用者结束且发布者完成后归还。外部等待不允许进入受管 handler，不改语言 RequestId 或 ABI。
- ADR-0048 的 tcp_counter 是真实 loopback TCP 验证示例，单计费适配线程、固定 32 B 帧、有限连接/帧与绝对 I/O 截止时间，退出先停止/join。不是生产网络 VM、通用 reactor 或 BEAM 等价性能负载，不改变语言语法或 MVP 门槛。
- ADR-0049 使用 ABI v11 的 record_mask 将 RecordInt 语言消息复制为固定标量载荷，目标在原栈按预算导入并以最多 64 个宿主入口根保活；普通宿主/JSON/ingress 仍拒绝引用整数，列表消息/引用回复未开放，类型拼写不变。
- ADR-0050 的 tenant_accounted/process_accounted 在子账户分配前向父路径预留节点逻辑字节，最后持有者释放才归还；worker 默认使用计费进程账户。旧账户构造器、VM 根/门闩 bootstrap 和 allocator 内部元数据仍未闭环，不把该切片称完整资源隔离。
- 功能开发围绕同一组业务负载的吞吐与整机内存双目标逐步推进。先完成可验证的功能切片，再测量瓶颈、单独优化并保留回归；优先消除重复元数据、对象分配和间接访问，不为自研而重写已验证的底层组件。自研栈池不等于栈成本消失，局部微基准不等于已领先 BEAM。

## 文档入口

先读 `README.md` 和 `docs.md`，再按任务查阅对应主题：

| 文件 | 负责内容 |
|---|---|
| `README.md` | 项目概览、状态、交付形态与平台范围 |
| `docs.md` | 文档目录和阅读顺序，不复制各主题正文 |
| `docs/product.md` | 场景、替代方案、采用路径和产品价值 |
| `docs/architecture.md` | 架构约束、后端、受管 ABI、GC 和平台实现 |
| `docs/crates.md` | 计划中的 crate、依赖方向、工具链与 LSP 边界 |
| `docs/publishing.md` | 初始版本、包名查询、许可证与发布顺序 |
| `docs/adr/README.md` | 架构决策索引、记录规则和当前状态 |
| `docs/runtime.md` | 进程、内存、调度、消息、额度、监督与热更新 |
| `docs/language.md` | 语言示例、handler、异步交互与副作用语义 |
| `docs/optimizations.md` | 激进优化候选、依赖、风险、实验与回退要求 |
| `docs/evaluation.md` | 实验条件、基线、指标与故障注入 |
| `docs/roadmap.md` | 风险、里程碑和各阶段验收范围 |

每项设计只在对应主题文档中详细说明，其他文档用相对链接引用。修改契约时，检查架构、运行时、语言、评测和里程碑是否一致。

主题文档维护当前契约，`docs/adr/` 保存选择理由。实质改变已采纳决策时新增 ADR，互相链接替代关系，并同步索引与相关文档，不覆盖历史理由。

## 不要擅自改变的设计约束

以下是当前设计，尚未实现。调整时说明影响，并更新相应的验证要求。

- Rust VM + `wings-lang` 单前端 + Cranelift AOT 后端；交付 VM、编译工具与 LSP。首版不提供受限 Rust 子集前端，不承诺任意 Rust 库直接进入受管路径。
- 完整执行引擎使用 `wings-vm`。调度、GC、mailbox 与平台实现先作为内部模块；按 `docs/crates.md` 保持 workspace 依赖无环。
- LSP 与编译器共用源码、语法和语义分析库。`wings-lsp` 不依赖代码生成或 VM，不执行用户程序完成查询；编辑器协议类型不得进入语言分析库。
- 首要运行目标为 macOS AArch64（Apple Silicon）与 Linux x86-64，M0 起并行验证完整 VM。Linux AArch64、macOS x86-64 为后续扩展，不把 macOS 降为仅前端或工具开发平台。
- 业务数据不可变，微进程之间不共享可变业务数据。逻辑独立 arena 不等于每进程独立 `mmap` 或硬件隔离。
- 语言使用 `process`、具名 `state`、统一 `on` 和 `{}` 块。消息字段与回复显式标注类型；有 `->` 的入口处理请求，无 `->` 的入口处理单向消息，`-> Unit` 不能当作单向消息。只允许替换状态绑定，参数与局部 `let` 不可变，不恢复旧回调语法。
- handler 返回后才释放执行栈；被抢占的 handler 仍有活栈，不能冬眠。首版不支持 actor 内阻塞 `receive`、`await` 或同步 `call`。
- `send`、`request`、`spawn` 的副作用在调用点生效；handler 失败不会撤回这些副作用。状态与自动回复只在正常返回后提交，不承诺事务或 exactly-once。
- 抢占依赖编译器强制 safepoint 和原子标志轮询；runtime 长操作也必须有界或可分块让出。不用取消检查换取性能，不承诺硬实时。
- 初版使用显式 shadow stack 和进程内精确、非移动 mark-sweep GC。禁止 Rust panic/unwind 跨越受管 ABI。
- 可增长资源在分配前经过微进程、租户、VM 实例三级准入；预留失败回滚，资源转移与清理不能逃逸计费。
- 同进程内的受管机制无法隔离编译器、runtime 或可信 native 的内存错误；不可信原生扩展需要后续的 OS 子进程方案，线程池不能代替。
- 首版静态加载。热更新、原生子进程池、额外平台组合与分布式均按里程碑独立决策，不提前写成现有功能。
- 激进优化集中记录在 `docs/optimizations.md`；候选不等于默认设计。涉及 GC 移动性、对象所有权、受管 ABI 或执行模型变更时，先更新契约和验证要求，再接入实现。

## 写作约定

- 默认用中文，写清行为、条件、顺序和失败路径；保留必要的英文术语与代码标识。
- 表达具体、自然、克制，不加“下一代”“极致性能”等宣传语，不为命名补写宏大品牌故事。
- 产品说明围绕性能与内存目标展开；组件与运行方式集中在架构文档，不反复解释实现语言、独立运行或嵌入式 SDK。
- 明确区分拟定设计、待验证假设、实验目标和已测结果。不要把可能写成确定，把计划写成已完成。
- 保留对照方案、限定条件、失败处理和不做的事项；不要为压缩篇幅删掉安全边界。
- 性能数字附负载、平台、资源上限与测量区间；没有数据就明确写未验证。
- BEAM 是性能与内存的主基线，kameo/Tokio 和 Wasmtime 是辅助对照。两项目标须在同一组代表性负载下验证，不能拼接不同场景的最优值。
- 两个首要平台分别在同机、同系统下对照 BEAM，不混合跨平台跑分；RSS、物理占用、压缩内存和 PSS 等统计不能当作同一口径。
- 语言示例区分检查器已支持的子集与拟定能力，不把静态检查通过写成编译运行成功，也不宣称 Elixir 或其他语言工具可以直接执行。接口草案不当作已发布 API。
- 调整标题或移动章节时，更新相对链接和目录，不维护重复的整篇设计稿。

## 实现与验证

- workspace 初始版本为 `0.0.1`，采用维护者确认的 `Apache-2.0` 许可证，仅向 crates.io 发布。未经维护者确认不更换许可证，不发布空包占名；包发布细节见 `docs/publishing.md`。
- `main` 受保护，代码和文档变更必须从分支提交 PR，通过 macOS AArch64、Linux x86-64 与 MSRV 检查后合并；不直接推送 main、不绕过管理员保护。包发布独立手动执行，不由 PR 事件触发；发布已合并、通过验证的源码，未经用户要求不自动发布。
- 收到实现任务时，先对照 `docs/roadmap.md` 确定阶段；优先解决 M0 的执行模型与后端验证，不一次铺开所有可选功能。
- 文档任务检查相对链接、章节完整性、名称一致性和代码围栏。代码改动运行 `cargo check --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`，格式检查使用 `cargo fmt --all -- --check`。
- 后续新增可运行代码时，同时记录实际可用的构建与验证命令，再按改动范围执行检查。
- 崩溃、非法访存与 OOM 注入只能在有资源限制的隔离测试 OS 子进程内进行，不在开发宿主制造无约束资源耗尽。
