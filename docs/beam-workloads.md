# BEAM 工作负载协议 v1–v11

[ADR-0140](adr/0140-bytes-tcp-service-equivalence.md) 的独立受限 TCP 正确性入口沿用 v1 的三种逻辑完整状态和 32 B 请求/成功回复，另用原 TCP 错误回复映射；不改 v1–v11 的已有输入/测量协议。一个 adapter 对一个 actor，mailbox 1，连接/帧/期限固定；健康全量状态逐字节核对，失败只验提交前缀。显式 `python3 scripts/verify_tcp_bytes.py --artifact <tcp-bytes-artifact> --output <new-directory> --compare-beam` 才执行真实 OTP，默认完整 AOT 只跑 Wings/模型。范围和实测见[记录](validation-tcp-bytes-2026-11-19.md)，不是新性能协议或 BEAM 双门禁。

[ADR-0134](adr/0134-opt-in-worker-cpu-observation.md) 仅在 Wings 原独立 `--observe-locks` 诊断输出 worker 函数 CPU 区间；v6 业务、事件和 BEAM 不变。冻结驱动核对两个 join 后的实际终态，缺失使诊断失效；范围与结果见[记录](validation-worker-cpu-2026-11-18.md)，不代表两端性能验收。

本协议配合[评测与验收](evaluation.md)和[里程碑](roadmap.md)使用。v1–v5 是 M0 的独立正确性切片，**只有显式 `--compare-beam` 才实际运行 BEAM**；v6 是另行显式启动的长时测量，必须有真实 OTP 29，不回退成模型。两类入口都不作未经验证的性能或内存领先声明，不修改副作用或提交契约。

## 已执行的最小业务

独立恢复区间诊断见 [ADR-0125](adr/0125-streamed-resume-fairness-diagnostics.md)/[0126](adr/0126-bounded-resume-trace-retention.md) 与[验证记录](validation-resume-fairness-2026-11-18.md)：v12/v13 是 Wings 调度内部的诊断版本，未改变本文原 v7/v8 业务或 BEAM 对照，不表示新增一个 BEAM 同步调度协议。默认入口仍不自动执行 90s 长测。

[ADR-0087](adr/0087-accounted-beam-workload-roots.md) 让六个 Wings 驱动使用计费根，原 32/128/256 MiB 等限额不变；清理后完整账本回到实际启动基线。当前业务报告附 `bootstrap_bytes > 0` 和相等的 `remaining_bytes`，正确性入口与新 v6/v8 采集严格校验；驱动剩余外部测试内存仍不等于完整受管计费。历史冻结结果不回写，离线重放允许两字段都缺失，若出现任一字段则须完整验证。

v2 Wings 批量创建可显式复用同 tenant 的一槽计费初始化池，见 [ADR-0073](adr/0073-reusable-initializer-stack-pool.md)。池创建仍在原 3 秒 actor 阶段内；不改输入、规模、100 活动 actor、32 窗口、CPU/墙钟限制或失败业务。停止 actor 后释放池再检查启动基线，复用本身不是性能验收。

v5/v6 Wings 开环也复用一槽计费初始化池，池创建计入原 3/120 秒阶段，全部 actor 创建后销毁池，再完成配置、启动业务计划时钟。保留原到达序列、状态、mailbox 和 pending；最终仍核对启动基线，阶段失败给出执行环节，不延长 deadline 或重试业务。

- 一个普通 BEAM actor，或 Wings 单 worker 下一个有计费堆的 actor，初始计数器由输入给出；不冬眠、不重启。一个外部驱动发送请求，actor 内没有阻塞业务调用。
- 计数器和 delta 均为 `[-2^63, 2^63-1]` 的有符号整数，每次加法单独 checked；不环绕、不饱和，也不让 BEAM 的任意精度整数成为业务结果。BEAM 和 Python 可用宽整数计算临时值，但必须在编码、提交和成功回复之前检查范围。
- 逻辑 state 恰为 `256 / 4096 / 65536 B`：计数器 `i64 big-endian`、actor ID `u64 big-endian`（固定为 1），其余为零填充。Erlang 使用不可变 binary，Wings 使用不可变 Bytes 保留该完整状态，每次成功构造候选状态；逻辑字节不等于堆大小、复制成本或 RSS。
- 每个请求和成功回复都有相同的 **32 B 逻辑业务记录**：`sequence:u64, operation:u64, value:i64, reserved:u64`，均为 big-endian，reserved 必须为零。请求 operation 为 1（add）或 2（add 后注入失败），value 为 delta；回复 operation 为 3，value 为提交后的计数器。JSON 和 Erlang tuple、PID、monitor 等信封成本另算，不能称物理消息也是 32 B。
- 请求 sequence 从 1 连续增长。驱动一次发送最多 32 个请求，再收齐该批回复，才发送下一批；不逐条同步调用。只保证**同一发送者到同一接收者**的 FIFO，不要求跨发送者全序。收集器按实际到达顺序检查 sequence，不能用选择性接收掩盖乱序。
- 成功路径先选定候选状态，再发布成功回复并以新状态进入下一次循环；失败路径不进入新状态、不成功回复。溢出优先于注入失败判定，BEAM actor 以携带失败序号和最后提交值的 `handler_failed` 原因退出，monitor 结束等待；Wings 由 Exited/固定旧提交 Bytes 副本和结构化故障核对相同结果，失败 pending 全部终结。已发但未处理的后续请求不再执行，不捕获失败后继续处理、不重放请求。
- 候选状态与自动成功回复共同受正常结束条件约束，**不是外部副作用事务**。当前没有 `send/spawn` 业务副作用用例，不验证副作用回滚；未来这些副作用仍在调用点生效，失败不撤回。回复投递失败也不能回滚已提交状态。

## 输入、输出与运行限制

入口为 [`scripts/beam_workload.escript`](../scripts/beam_workload.escript)，从 stdin 读取一个 JSON 对象，直到 EOF；不接受命令行参数。所有字段都必填，不允许未知或重复字段、额外 JSON、浮点替代整数或 bool 替代整数：

```json
{"version":1,"state_bytes":256,"initial":0,"requests":[{"delta":3,"fail":false},{"delta":-1,"fail":false}]}
```

输出为一行 JSON：`version/status/state_bytes/message_bytes/environment/committed/replies`。`replies` 按到达顺序包含 `seq/value`；失败额外包含 `reason/failed_seq`，只保留失败前成功的回复。`environment` 记录 OTP、ERTS、架构、JIT/解释器类型、在线调度器数与字长，不输出吞吐、延迟或内存排名。输入错误只输出 `version/status/reason`；oracle 内部错误只输出 `version/status`。

退出码：`0` 全部成功，`1` 预期业务失败且 actor 已退出，`2` 输入或 OTP 版本不支持，`3` oracle/驱动验证失败。被 OS 信号终止、墙钟超时、非法输出也算验证失败，不当作业务失败。Python 默认回归套件中的预期业务失败不会使整个套件失败。

| 资源 | 固定上限 |
|---|---|
| 输入 | 65536 B；先限制读取，再解 JSON，不从 JSON 创建 atom |
| actor | 1 个业务 actor + 1 个驱动；BEAM 系统进程另计；节点进程/port 上限各 1024 |
| 请求 | 总计 0–256，每批最多 32 个未完成请求，最多 8192 B 累计逻辑请求 |
| 队列 | 单 actor 最多 32 个请求（含在途/当前执行的保守上界）；驱动最多 32 个该批回复或退出通知；没有无限发送者 |
| 状态 | 单 actor 最多 65536 B；更新期间旧/候选 binary 都可能存活，不等于内存总限额 |
| actor 堆 | `max_heap_size=262144` words，含共享 binary 计入检查，超限 kill，不记录巨量日志；这是 GC 检查边界，不是 OS 内存硬隔离 |
| 时间 | actor 阶段共享 3000 ms deadline；Python 子进程 `RLIMIT_CPU=5s`、`RLIMIT_CORE=0`、墙钟 10s，超时杀整个进程组并回收 |

只经 [`scripts/verify_beam_workload.py`](../scripts/verify_beam_workload.py) 启动日常验证；裸跑 escript 不具备上述 OS CPU/core/墙钟保护。脚本仅使用 Python 标准库与 OTP 29 内置 `json`；不下载依赖。驱动清除 `ERL_FLAGS/ERL_AFLAGS/ERL_ZFLAGS/ERL_LIBS`，禁用 crash dump。固定 `+S 1:1 +A 1 +SDcpu 1:1 +SDio 1`；没有 CPU affinity 或单核 OS 配额声明，dirty/辅助线程仍存在。不做无限 spawn、OOM 或 native 崩溃注入；资源限额不是恶意代码沙箱。

## 多 actor 正确性协议 v2

[ADR-0132](adr/0132-bounded-multi-counter-failure-evidence.md) 为本协议宿主按读取过程执行原 stdout 1 MiB/stderr 64 KiB 上限，所有管道与进程等待共享原十秒期限。失败保留有界用例标签、输入 SHA/头部、双流前缀/EOF 与清理状态；不等待全量退出后才发现越界，不以重试覆盖原失败。实际原限三端回归及七项短测试见[记录](validation-multi-counter-evidence-2026-11-18.md)，原业务、CPU/actor 期限和性能边界不变。

[ADR-0063](adr/0063-multi-actor-byte-counter.md) 接入独立的 `verify_multi_counter.py`、`beam_multi_counter` 和 `beam_multi_workload.escript`，保留 v1 不变。JSON 为 `{version:2, actors, state_bytes, initial, requests:[{actor,delta,fail}]}`；actor 为 `1..N`，其余严格类型/重复键/未知字段/64 KiB 输入限制沿 v1。actor 数 1..10000，完整逻辑 state 总量最多 16 MiB，单次最多 256 请求、总在途窗口 32。

真实状态布局仍为 counter/actor ID/零填充，ID 改为各目标独立编号。三组大容量测试为 10000×256 B、1000×4 KiB、100×64 KiB，每组 100 个活动目标、按 slot 间隔分布，不冒充后述性能协议的前 A 个活动集合。失败只终结原目标的已入队队尾，其他目标继续；输出按输入序号规范化，但只约束同发送者到同目标 FIFO，不要求跨 actor 全序。BEAM 在规范化前核对实际回复顺序和 monitor；Wings 核对唯一请求 handle 的值序列及每目标独立终态。

Wings 用 Never 监督、2 worker、共享 32 结果槽、堆四槽，每 actor mailbox 取其在任一 32 条窗口内的最大请求数（至少 1、最多 32）。VM/tenant 各 256 MiB、process 4 MiB。二进制桥接为 40 B 头 `version/state_bytes/initial/actors/count`，后接最多 256 条 40 B `actor/sequence/operation/delta/reserved`，路由信封不算入 32 B 逻辑业务请求。先完整校验精确长度/路由/字段，再装载可信产物。

Wings 核对实际提交的长度/计数器/ID/32 B 前缀，完整 padding 沿已测 Bytes 原语而不是全量导出；BEAM 在活 actor 停止前检查全部 padding。失败旧值与逻辑资源恢复启动基线另行核对，不等同 RSS。OTP 固定 `+S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 32768 +Q 1024`，每 actor 的 `max_heap_size=262144` words 仍非 OS 内存硬隔离。v2 输出最多 1 MiB，业务 CPU 5 秒/墙钟 10 秒/禁 core、actor 阶段总 deadline 3 秒；仅经 Python 限制器执行，不裸跑 escript。真实 BEAM 显式启用，未安装时失败、不回退成模型通过：

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_multi_counter
python3 scripts/verify_multi_counter.py
python3 scripts/verify_multi_counter.py --compare-beam
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release python3 scripts/verify_multi_counter.py --compare-beam
```

默认完整受限 AOT 接入 Wings/Python；`--artifact <目录>` 复用匹配产物。该套件是闭环正确性，不生成吞吐、尾延迟或整机内存结果，仍未覆盖两个真实发送者和热点/过载性能。

## 双发送者正确性协议 v3

[ADR-0065](adr/0065-two-sender-counter-fifo.md) 新增独立的双发送者驱动，v1/v2 不变。Wings 两个真实宿主线程直接经 SchedulerHandle 发送，BEAM 两个真实进程直接向 actor 发送，不经单代理代发。两个发送者在业务前释放开始闸门，各自最多 16 个在途请求、每条流至多 128、总计 256。此处是竞争下的各自 FIFO，不规定跨发送者/actor 的全序，也不是开环生成器。

JSON 为 `{version:3, actors, state_bytes, initial, streams:[[{actor,delta}], [{actor,delta}]]}`。actor 1..100，state 仍为 256/4096/65536 B；仅接收对所有交错都安全的 checked i64 加法集合，按目标分别检查 `initial + 全部负 delta` 与 `initial + 全部正 delta` 均在 i64 范围内。浮点/bool 替代整数、未知/重复字段、缺字段、超长流和不安全交错均拒绝。失败/溢出沿 v1/v2 验证，不将本协议称为并发故障对照。

完整 Bytes 的前 32 B 为 counter:i64 BE、actor ID:u64 BE、sender1 已提交序号:i64 BE、sender2 已提交序号:i64 BE，余下零填充。每发送者到每目标独立从 1 连续计数，Apply 在构造候选前核对下一个序号，序号/计数器共同正常提交。每条逻辑请求/回复仍是 32 B，sender/actor 在信封；Wings 可信桥接为 48 B 头 `version/state_bytes/initial/actors/count1/count2`，后接两条流各自的 32 B `actor/sequence/delta/reserved`，精确长度/路由/序号/安全集合全部验证后才装载产物。

每端输出不要求相同回复值：独立 Python 前缀动态规划逐目标验证存在保留两流 FIFO 的合法处理历史，每一步的实际回复必须等于 checked 提交值，最终值核对实际 Bytes。Wings 用唯一结果 handle 消费且业务状态检查序号；BEAM 各发送者先按实际到达顺序核对目标序号，再向父进程汇总。没有用排序掩盖同目标乱序。模型附合法不同交错及伪造历史拒绝回归；真实 AOT 另验证跳号/重复起始序号失败。

Wings 固定 2 worker、共享 32 结果槽、每 actor mailbox 32、堆四槽，VM/tenant 256 MiB、process 4 MiB。BEAM 固定 `+S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024`，业务 actor/发送者均设 `max_heap_size=262144` words；收齐两个发送者完成与 DOWN、全部 actor 停止与 DOWN 后输出。Wings 副本只检查长度/前 32 B，不导出余下 padding；BEAM 检查完整零填充。驱动输入/线程/结果属于有界外部测试成本，不宣称全部受管计费或 OS 内存硬隔离。

业务子进程 CPU 5 秒/墙钟 10 秒/禁 core、共享 actor 阶段 3 秒、输入 64 KiB/输出 1 MiB、编译及 AOT 原限制不变。仅经 Python 限制器执行，不裸跑 escript：

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_senders_counter
python3 scripts/verify_senders_counter.py
python3 scripts/verify_senders_counter.py --compare-beam
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release python3 scripts/verify_senders_counter.py --compare-beam
```

`--artifact <目录>` 复用匹配产物；默认完整受限 AOT 只跑 Wings/独立模型，真实 BEAM 仍须显式启用。未安装 OTP 29 不回退成模型通过。并发故障/过载/超时、开环、热点和公平性仍待验证，没有吞吐、尾延迟或整机内存结论。

## 有限准入正确性协议 v4

[ADR-0067](adr/0067-beam-bounded-admission.md) 的 `beam_bounded_mailbox.erl` 绑定固定 Pid、1..256 容量和 signed atomics 计数；发送前 CAS 占一个排队位置，actor 收到后开始业务前调用 `started` 归还。满时返回 `mailbox_full`、关闭后返回 `closed`，都不发业务消息。没有代理代发、等待者队列或自动重试；系统控制/诊断消息另列，不冒充业务队列容量。

占槽是准入点不是提交点；占槽与 `!` 之间退出/关闭的请求仍算已接受，依据目标 DOWN 结束 pending，不能重试造成重复。只有绑定 actor 能归还，关闭不会因迟到消息/归还重开，旧 gate 不重新绑定；可信驱动保证发送者不在占槽后永久崩溃。本模块不是恶意代码沙箱或完整资源账本，未来性能/整机内存必须计入其原子元数据和全部真实 mailbox 成本。

v4 固定三个容量 1/100/256，每个独立受限子进程内运行两个完整 4 KiB state 的业务 actor。目标先进入 CPU Hold，确认已开始再排满请求，下一条拒绝且排队数不变；健康目标仍可通过单向消息提交 +7。随后关闭/停止满队列目标，所有已接受请求失败、旧入口拒绝，清理后输出；不是等待队列自然排空来伪造关闭。Wings 检查原 live owner、每结果只能消费一次和最终三级逻辑资源回到启动基线，BEAM 消费目标 DOWN、确认完整旧 state/队列长度及 gate 关闭；不声称 BEAM 有 Wings 资源账本。`initial_value` 只表示 Hold 前状态，不是 Wings 退出时全量导出。

BEAM 固定 `+S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024`，业务 actor max_heap_size 为 262144 words；原子 gate 自测增加两发送进程竞争、各自 FIFO、开始归还/复用、非法容量、关闭与发送竞态。业务 CPU 5 秒/墙钟 10 秒/禁 core、共享 actor 阶段 3 秒，AOT 原 CPU 20 秒/墙钟 30 秒不变。真实 OTP 29 须显式启用，默认完整 AOT 只跑 Wings/模型：

```sh
cargo build --workspace --locked --bins --examples
python3 scripts/verify_admission_counter.py
python3 scripts/verify_admission_counter.py --compare-beam
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release python3 scripts/verify_admission_counter.py --compare-beam
```

`--artifact <目录>` 可复用匹配的 mailbox-boundary 可信产物。该 v4 是有限准入/关闭正确性，不是 10000 request/s 开环、超时竞争、预热/采样或性能结果。

## v5：短时开环记账验证

[ADR-0068](adr/0068-bounded-open-loop-counter.md) 将下表四组非 fairness 负载接入真实 Wings/BEAM。`beam_open_loop` 和 `beam_open_loop.escript` 接收 `<case> <duration_ms> <rate>`；Wings 另有前置 `--trusted <beam-multi-counter artifact>`。只有 `smoke` 模式：持续时间 1..1000ms，速率 1..1280000/s，原始样本总数 `ceil(duration_ms*rate/1000)` 至多 10000，所有数字必须是十进制非负整数字符串。超限在加载产物/创建 actor 前拒绝，不能把缩短时段称为性能验收。

每次请求为 checked +1，完整 state、前 100 活动 actor、热点路由、32 B 逻辑记录和 mailbox 均沿下表；一个真实发送者直接投递，BEAM 经 ADR-0067 gate，不经代理。启动确认所有 state 初始化后固定 t0，逐次按计划到达，不等旧回复；空闲 yield，每轮最多收集 32 条，Wings 轮转探测最多 32 个 pending 票据而非每次全扫带锁表。原始结果按输入序号保存 `samples`，每行为 `[sequence, actor, planned_ns, sent_ns, status, observed_ns, value]`，时间均相对同一单调 t0。拒绝的 observed/value 为 null，其余保存真实结果。另有每 actor `committed`、配置与环境；没有性能排名。

驱动最多保留 256 条未收到实际结果的请求，满时记 `pending_full`；队列满记 `mailbox_full`，均不补发、不重试。即使外部 100ms deadline 已过，原 pending 仍占空间直到实际回复，以免超时造成无界迟到结果。`observed_ns < planned_ns+100000000` 才记 `reply`，边界及之后记 `timed_out`，迟到不能改回成功；超时不取消已接受业务，最终仍核对其提交值。Wings 内部请求租约用剩余阶段时间，仅用于保留迟到实际结果，不能据此移动业务 deadline。若 3 秒总阶段内未收齐真实结果、actor 失败或清理异常，则整个探测失败，不输出成功样本集。

独立 Python 重算每次路由/计划/外部 deadline、256 pending 上界、每目标递增与最终值、拒绝不执行和样本总数；两运行时拒绝位置不必相同，不以匹配延迟为正确性条件。BEAM 在规范化前另核对实际回复的每目标顺序、完整 padding 与 monitor；Wings 核对提交 Bytes 长度/ID/32 B 前缀及资源回到启动基线。模型还验证精确截止边界、迟到不复活、超时 pending 不提前复用、伪造记录与错误配置。v5 沿用 CPU 5 秒/墙钟 10 秒/禁 core、总阶段 3 秒，默认完整 AOT 只跑 Wings/模型，真实 OTP 29 仍须显式启用：

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_open_loop
python3 scripts/verify_open_loop.py --compare-beam --output target/open-loop-smoke-debug
WINGS_PROFILE=release python3 scripts/verify_open_loop.py --compare-beam --output target/open-loop-smoke-release
```

脚本执行四组固定速率各 100ms、hot-queue 1ms×1280000/s、dense-64k 10ms×1000000/s，并保留全部迟发/拒绝/超时；后两项是短时压力，不要求两端都恰好拒绝或超时，更不是饱和点。`--artifact` 可复用产物，`--output` 保留原始 JSON；采样仍由被测进程自己的单调时钟完成。该驱动 busy-yield 的 CPU、固定采样和 gate 成本都必须纳入整机测量。长时入口另见 v6，不能将短测结果拼成完整验收。

## v6：独立长时开环测量

[ADR-0133](adr/0133-open-loop-worker-lock-diagnostics.md) 为 Wings 运行端增加仅长测可用的末尾 `--observe-locks`。业务 v6 与原始事件不变，额外统计在全部 worker join 后输出，覆盖完整 Scheduler 生命周期；独立驱动冻结两对开关诊断，不修改普通 BEAM 对照入口或五对要求。范围与实际记录见[验证记录](validation-open-loop-locks-2026-11-18.md)，累计锁墙钟不是 CPU 或采样阶段分布。

[ADR-0146](adr/0146-measurement-artifact-schema-preflight.md) 要求 v6/v8 Wings 运行端在 actor 创建和业务 `t0` 前核对完整 state/handler schema：v6 为 Bytes state、三参数 Configure 与四参数 Apply，v8 为 Bytes state、单参数 Configure 与四参数 Cpu/Pulse，参数均为 Int、回复均为 Int。错误产物保留 0 B trace、只有 `oracle_error` 终态而没有 start/sampling；正确负载、时钟、事件和判定不变。真实回归见[记录](validation-measurement-schema-preflight-2026-11-19.md)。

[ADR-0069](adr/0069-streamed-open-loop-measurement.md) 为同一业务新增 `--measure <case> <rate> <new-trace-file>`，Wings 前置 `--trusted <artifact>`；只允许下表四组非 fairness 的基准速率乘 `2^0..2^7`。固定连续 90 秒，前 30 秒预热、后 60 秒按计划时刻分组采样，边界不暂停/重置 state 或清 pending；结果仍 checked +1、完整 state、原路由/mailbox 和 256 实际 pending。外部 100ms deadline、迟发不重基准、超时保留至实际回复、拒绝不重试均不变。

每条到达和真实回复按发生顺序写 40 B big-endian：`sequence:u64, sent_ns:u64, observed_ns:u64, value:i64, kind:u64`。kind 0=接受到达，1=按时真实回复，2=超时后的真实回复，3=mailbox_full，4=pending_full；非回复的 observed/value 为零。先有接受再有回复，拒绝无回复，目标/计划由 sequence 重算。固定 64 KiB 缓冲，独占新建文件不覆盖，写入成本计入业务运行；不随总请求数分配内存样本。stdout 只有 start、sampling 和最终结果三个控制 JSON，stderr 非空、缺事件/截断/重复/未闭合 pending 都判实验无效，不能当成功或预期业务失败。

长测独立 OS 子进程 CPU 240 秒/墙钟 130 秒/总阶段 120 秒/禁 core/单文件 10 GiB。最高允许 115200000 到达、最多 230400000 事件/9216000000 B，runner 在每点之前按十次最坏输出加 1 GiB 余量检查磁盘。v1–v5 的原 CPU 5 秒/墙钟 10 秒/总阶段 3 秒及完整 AOT 限制不变，默认 AOT 只增加模型/非法长测参数校验，不自动执行 90 秒实验。

`measure_open_loop.py` 固定五次独立配对重复，交替两端先后顺序、同一时刻只跑一端；启动前冻结执行文件、BEAM/gate、artifact 和当前 tracked 源码及本切片新文件，逐个保存 SHA-256。默认只跑基准点，`--scan` 才最多八点翻倍；任一运行时有拒绝/超时/尾延迟失败则当前点五次跑完后停止提高，无失败点标扫描不充分。任何异常保留原始输出、失败 manifest，不自动重试或跳过。

离线 verifier 逐条核对计划、路由、关联、pending≤256、拒绝/超时、每目标值及最终提交；BEAM 另验实际到达 FIFO。分位包含迟到实际回复，nearest-rank 精确计算，少于 10000 不报 p999。用三轮固定 16-bit radix 桶离线选秩，不存全量延迟数组；另报全部到达的迟发、拒绝/超时比例和最大值。固定候选要求无拒绝/超时、p99≤20ms、p999≤50ms；报告 `tail_candidate_pass` 不等同 BEAM 双目标达标。RSS 每秒取被测进程树加 Python 驱动，ADR-0075 起当次 census/probe 辅助进程单列排除；保留共享页重复统计含义，超过观测 1 GiB 安全停止不是 OS 硬内存配额。

```sh
cargo build --release --workspace --locked --bins --examples
target/release/wings-compile build examples/beam-multi-counter.wings -o target/long-counter
python3 scripts/verify_measurement.py --compare-beam
WINGS_PROFILE=release python3 scripts/measure_open_loop.py --case hot-queue --artifact target/long-counter --output target/long-hot
```

选择其他 case 或增加 `--scan`，但不能减少时段/重复次数。[ADR-0075](adr/0075-platform-memory-observation.md) 增加冻结的原生 C helper，逐秒分列 Linux smaps_rollup 的 RSS/PSS/private/shared/swap、macOS resident/physical footprint 和主机压缩/swap；主机值不归因给 VM、不与进程 footprint 混加。驱动及后代有界前后 census 排除当次监测 helper，保留各 PID/start_id/时间窗、helper CPU/max RSS/wall；拓扑变化/读失败/身份变化时平台总数为 null，原始样本与完整数不丢弃。原 `ps` RSS 安全停止保持，不是 OS 硬配额。

[ADR-0130](adr/0130-readonly-cgroup-cpu-quota-observation.md) 为本入口与 v8 Python 驱动增加成对的 `--cpu-quota-us`/`--cpu-period-us`。仅在预先配置、启动时只有 driver 的 Linux cgroup v2 leaf domain 组内使用；入口只读检查，不创建、移动或调整组。原生路径/目录身份、祖先配额、成员/线程策略和 cpu.stat 随平台内存样本记录，每轮有首尾 CPU 观测；不匹配/变化/非 fair class/退出竞争使实验无效。范围是整个测量作业，含 driver 与 probe helper，不能称为单独 VM 配额；无参数不读取此控制层，原默认行为不变。本机只完成模型与 macOS 拒绝验证，实际命令和边界见[验证记录](validation-cpu-quota-2026-11-18.md)。

当前 runner 仍明确标注未验收主机隔离/CPU 硬配额、未记录电源/热状态/QoS/核分配；平台采样不覆盖全部瞬时峰值、共享/内核成本或每进程压缩/swap，监测偏差未验收。因此只能提供分口径长时集成证据。fairness、最新原生双平台、最大持续吞吐、完整整机内存与可试用 MVP 均未据此通过。

本机 hot-queue 基准点五对长测的有限证据见 [验证记录](validation-2026-11-14.md#独立长时采样adr-0069)和 [JSON 摘要](open-loop-hot-macos-2026-11-14.json)。ADR-0075 另完成 mixed-4k 原规模五对长测，见[验证记录](validation-2026-11-14.md#分口径平台内存观测adr-0075)和 [JSON 摘要](open-loop-memory-mixed-macos-2026-11-14.json)：两端各仅一轮通过候选尾延迟，拒绝/超时及一份不完整采样均保留。没有饱和扫描、CPU 硬配额或完整平台内存验收，不作最大持续吞吐/整机内存领先结论；其余两组与长时 fairness 仍待验证。

2026-11-16 的 ABI v15/Bytes 回复源码另完成 mixed-4k 1000/s 五对探索性复测，见[验证记录](validation-2026-11-16.md#实际结果)和 [JSON 摘要](open-loop-mixed-macos-2026-11-16.json)。十轮均无拒绝/超时，两端各 5/5 满足候选尾延迟，但 Wings 每轮 p99 高于 BEAM；驱动加后代的 RSS/footprint 采样较低不等于整机内存领先。未扫描饱和点、未落实 CPU 硬配额，不能据此宣布双目标或 MVP 达标。

## v7：百万次加法与突发心跳的短时验证

[ADR-0070](adr/0070-bounded-fairness-heartbeats.md) 和 [`beam-fairness.wings`](../examples/beam-fairness.wings) 将下表fairness行接入有界正确性：101个actor各保留完整256 B state/独立ID，mailbox1；Wings只有一个worker，BEAM只有一个在线scheduler（`+S 1:1`），但未设置OS单核硬配额。热actor1每个CPU请求从零执行恰好1000000次checked +1，回复百万、state计数加一；以六层非递归受管函数逐级十次调用，不改语法/ABI，不在每次加法分配Bytes，也不使用一次+百万代替。BEAM使用相同六层checked业务，保留原生reduction调度。

冷actor2..101每10ms各发送一次心跳，计划为 `t0+floor(k/100)*10000000ns`，同一burst100条同刻，不均匀摊开。短测只接受严格字符串`10`/`30`，分别100/300条到达；直接发送、不等旧回复/不暂停计划/不重试。热任务正常返回后立即续发，直到最后计划burst发送完；随后收齐已有CPU/冷请求，不继续续热任务。总结果容量256，其中255实际冷pending加一个保留热请求；队列满/冷pending满逐条记录拒绝。冷请求计划后100ms外部deadline，迟发不重基准、超时pending仍占空间至真实回复，不取消已接受业务。

输出v7 smoke包括与v5同形状的全部`samples`、热任务`hot:[sequence,sent_ns,observed_ns,1000000]`、每actor实际`committed`。最终要求至少一个CPU实际完成、所有接受的心跳实际收齐并核对state；BEAM还检查完整padding/实际每目标回复顺序/monitor，Wings检查提交Bytes长度/ID/32 B前缀及资源回到启动基线。CPU预算固定10000000 steps，另以小预算真实执行验证不可绕过。任何阶段超时、业务失败或清理不完整使整个测试失败；CPU5秒/墙钟10秒/禁core、总阶段3秒与完整AOT20/30秒不变。

独立Python重算全部同刻计划/目标/每目标值/最终状态、拒绝与超时、255实际冷pending上界，报告每冷actor最大观测等待及拒绝/超时、nearest-rank p50/p99/最大值，少于10000不报p999。`observed_while_CPU_pending`只表示CPU票据开始到结果观察区间的重叠，不能冒充CPU当时仍在执行、实际抢占或让出时间。两端CPU完成次数/拒绝位置/时间不要求相同，不将某种OS交错写成必然。

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_fairness
python3 scripts/verify_fairness.py --compare-beam --output target/fairness-smoke-debug
WINGS_PROFILE=release python3 scripts/verify_fairness.py --compare-beam --output target/fairness-smoke-release
```

`--artifact`可复用匹配可信产物；默认完整AOT只执行Wings/模型，缺少OTP29的显式三端失败、不回退。独立30s/60s/五次重复见v8；OS单核硬配额、让出观测覆盖和过载分组仍待验收。不把心跳延迟代替候选让出p99<5ms，也不将此短测称MVP或BEAM双目标通过。

[ADR-0071](adr/0071-observed-atomic-yield.md) 另提供 [`observed_yield`](../crates/wings-vm/examples/observed_yield.rs) 独立 fixture，用同一可信产物执行预请求与远端线程请求，并由 `verify_fairness.py` 在原限制内调用。只有真实 coroutine 返回后确认才产生观测；远端请求若在业务结束后才到达则单列未消费、延迟为 null。它不修改 v7 的 Scheduler/BEAM 业务或默认 worker token，也不将两个独立样本当作同 worker fairness 让出统计；该门禁仍待完成。

[ADR-0072](adr/0072-scheduler-yield-observation.md) 增加 Wings 独立 `--trusted <artifact> 10|30 --observe`，由同一 verifier 调用。只启用每 worker 64 条计费 FIFO，不改上述 actor/业务、4096 steps 片或 1ms quantum。另存 `yield_observation.samples:[sequence,actor,requested_ns,yielded_ns]`，时间以 Scheduler 启动后的独立观测起点为零，不冒充心跳 t0；保留配置初始化期间发生的样本。直接驱动每轮最多取 32 条，宿主历史最多 4096 条，超出判失败；FIFO 满拒绝新样本并计 dropped，最终停机收齐后核对请求与队列守恒。输出含每 actor/总样本 nearest-rank 分位，零样本为 null、少于 10000 不报 p999；丢样显式标不完整。短测可能一个 timer 请求也没有，不靠提高 quantum、关闭步骤片或增加热任务工作量凑观测；BEAM 与默认模式不受此诊断影响，尚无长时公平性/性能结论。

实际 timer 边界另由 [`scheduler_yield`](../crates/wings-vm/examples/scheduler_yield.rs) 用原 `mailbox-boundary.wings` 的 Hold 做 AOT 回归：双 worker、1ms quantum、无限步骤片、每 worker 一槽 FIFO，验证真实让出、身份、丢样与取消清理。它不是 v7 业务的采样参数，不参与 BEAM 性能排名。

## v8：连续 CPU 与突发心跳长测

[ADR-0077](adr/0077-streamed-fairness-measurement.md) 为 v7 增加独立 `--measure`，固定连续 30 秒预热/60 秒采样、900000 次心跳，五对交替顺序。101×256 B、mailbox 1、一个真实百万次 checked 加法热任务及其余 100 actor 每 10ms 同刻 burst、255 冷 pending 加一热 pending、计划后 100ms deadline 均不变；原短测及其限制保持。收齐最后计划后仍存活的热/冷实际结果，再检查全部状态和清理。单 worker/在线 scheduler 仍不是 OS 单核硬配额。

每条事件为 40 B `>QQQqQ`：心跳 `[sequence,sent_ns,observed_ns,value,kind]` 的类型 0 接受、1 按时回复、2 超时实际回复、3 mailbox_full、4 pending_full；接受/拒绝时 observed/value 为零。序号决定 actor `2+(sequence-1)%100` 和计划 `floor((sequence-1)/100)*10ms`。热任务用独立序号的 5 接受/6 完成，每次完成值必须百万。Wings 可选的类型 7 为 `[sequence,requested_ns,yielded_ns,actor,7]`，时钟相对初始化前观测起点；最终 `observation_offset_ns` 是业务 t0 距该起点的偏移。不要用它直接充当心跳时钟，或把心跳延迟称为让出延迟。运行端最多 64 KiB buffer，不保留完整样本数组；最终控制输出含空 samples/hot 和完整状态/计数。

独立 verifier 重放完整事件，拒绝重复/缺失、假 pending_full、提前结束 CPU、错误时基/值/FIFO、截断和未 drain。计划在 30 秒前归预热，其余归采样，迟到完成不重新归组；每个冷 actor 固定采样段 6000 次到达，p999 因少于 10000 条返回 null，仍报 p99/最大值和全部拒绝/超时。全局分位、生成迟到、热任务按起始阶段的耗时和实际落在采样窗内的完成率分别列出。让出样本分组按请求时刻减观测偏移；初始化样本仍落盘，丢样、合并/清除和无数据保留。离线分位仅在业务退出后用固定时段限定的数组计算，验证器内存不属于受管业务堆，后续轮次驱动 RSS 也会反映 allocator 保留，不能据此当作纯 VM 内存。

```sh
cargo build --release --workspace --locked --bins --examples
target/release/wings-compile build examples/beam-fairness.wings -o target/long-fairness
WINGS_PROFILE=release python3 scripts/verify_fairness_measurement.py --compare-beam
WINGS_PROFILE=release python3 scripts/measure_fairness.py --artifact target/long-fairness --output target/long-fairness-v8
```

增加 `--observe` 会为五对中的 Wings 启用原 64 槽计费 FIFO；仪器模式单列，不能写成默认性能结果。两次观测的请求时间各自单调、返回时间各自单调，但后继请求可能先于上一轮确认到达，不能据区间重叠拒绝合法记录。`measurement_elapsed_ns` 保留业务循环退出时间，完整采样窗不得早于 90 秒，即便最后一批心跳计划在 89.99 秒。运行器冻结源/runner/产物/BEAM/验证器与 hash，沿用 CPU 240 秒/墙钟 130 秒/业务阶段 120 秒/禁 core、文件与 observed RSS 安全停止和分口径平台内存采样。全局 `tail_candidate_pass` 仅按既有候选规则展示，`fairness_gate_proven` 始终 false：CPU 硬配额、监测开销、过载/非过载分组、原生双平台及同负载吞吐/整机内存双目标仍须验收，不因某轮分位较好或全部正常退出判定 MVP。

## v9：双发送者的并发失败前缀

[ADR-0092](adr/0092-concurrent-failure-counter.md) 使用两个真实宿主线程/BEAM 进程直接向目标投递，各自窗口 16、最多 128 条输入，1..100 actor、mailbox 32、三种完整 Bytes/binary 状态。输入为 `{version:9, actors, state_bytes, initial, streams:[[{actor,delta,fail}], [...]]}`，字段全部必填、不接受未知/重复字段。delta 只接受所有交错均在 i64 内的集合；正常请求更新 counter 和该 sender 的连续目标序号，显式 fail 在构造候选后失败，保留最后提交值与序号。独立的 AOT 业务用 overflow 区分非法序号，不将协议错误伪装为 division-by-zero 注入。

Wings 可信桥接为 48 B 头 `version/state_bytes/initial/actors/count1/count2`，之后每条流的 32 B `actor/sequence/delta/operation`，operation 为 1/2；每目标每发送者序号从 1 连续。完整 state 的前 32 B 与 v3 相同，保留 counter、actor ID 和两个已提交目标序号，其余零填充。Wings 从实际状态或每 child 的有界监督终态读取长度/32 B 前缀；BEAM 检查完整 padding，使用 ADR-0067 的容量 32 CAS gate，开始归还、失败关闭并在全部退出后核对关闭。两 sender 的 32 总窗口不允许队满；关闭可来自 gate 或 monitor，拒绝不重试。两端不复用已失败目标、不重启或重发。

输出包括每输入恰一次的 `{sender,actor,seq,status,value}`，status 为已接受成功的 `reply`、已接受但失败的 `failed`、投递前已观察关闭的 `closed`；无成功回复时 value 为 null。另报告每目标 committed、sequences、失败原因和清理回执。两端观察关闭及线程交错可以不同，不按 JSON 完全相等验收。独立 Python 检查成功连续前缀和合法 FIFO 交错、全部真实值、失败边界上存在一个已接受的显式 fail、候选不泄漏、健康目标全部完成及结果守恒；Wings 清理须回到非零计费根基线。

沿用业务 CPU 5 秒/墙钟 10 秒/core 0、共享三秒 actor 阶段、64 KiB 输入和 1 MiB 输出。Wings 为两 worker、共享 32 结果槽、VM/tenant 256 MiB/process 4 MiB；BEAM 固定两个 scheduler、有限 heap，parent/两个 sender 独立 monitor，按实际 reply/DOWN 次序收集。所有失败 actor 和 sender 终结、健康 actor 停止后才报告。此处没有 timeout/cancel、恢复、长时负载或性能门禁，不能代替 ADR-0074 及其余验收。

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_failure_counter
python3 scripts/verify_concurrent_failure.py
python3 scripts/verify_concurrent_failure.py --compare-beam
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release python3 scripts/verify_concurrent_failure.py --compare-beam
```

默认完整 AOT 包含 Wings/模型；仅显式 `--compare-beam` 执行 OTP 29，缺少则失败。三种状态分别覆盖空流、单/双 sender 注入、竞争失败、健康 sibling、跨窗口关闭、100 目标及 i64 边界；枚举小流全部交错生成模型正例，再拒绝伪造前缀、假失败、错误状态、重复/丢失终态和非法输入。不据此宣称完整并发故障或 MVP 完成。

## v10：持续环境的失败生命周期

[ADR-0120](adr/0120-persistent-concurrent-failure-soak.md) 共用 v9 的 Rust/BEAM 业务核心、相同输入和前缀 oracle，新增独立容器入口。Wings 全程保留同一 VM、module、tenant、Scheduler 与 supervisor；BEAM 保留同一 VM、驱动及已装载模块。每轮新建相同业务目标，两个真实 sender 各窗口 16/最多 128 请求，mailbox 32/共享结果 32；失败目标保留旧提交状态，健康目标完成全部请求，再等待 actor/sender 终态。没有自动重启或重发。

独立 `--soak` 固定 30s 预热/60s 采样，每 100ms 计划一轮，共 900 轮，不丢弃迟发或提前结束；每轮仍限 3s，整体阶段 120s、CPU 240s、墙钟 130s、禁 core，原 VM/tenant 256 MiB、process 4 MiB、两 worker 和业务堆/栈限额不变。标准输入复用 v9 的 48 B/32 B 桥接或严格 JSON，但必须有两条非空流、注入失败与实际健康目标业务。当前固定 workload 为 3 actor、每 sender 128 条、delta 1/2、第 19 条向 actor 1 注入失败；完整状态大小在 256/4096/65536 B 中显式选择。

每轮后及下一轮目标仍存活时，Wings 验证旧完整 Pid/child ID 查询和 send/request 拒绝；BEAM 验证旧 Pid 已死、旧 CAS gate 关闭并拒绝发送。每轮 Wings 的 process/message/request 归零，逻辑字节只允许冷 Scheduler 基线加 0..4 个原固定栈槽；缓存没有冒充为零成本。完整环境关闭后根 used 精确回到非零启动基线。BEAM 的进程/port 数只作本端诊断，不归一为 Wings 逻辑账户。

`cached_stacks` 是账面差额的槽数推导，不是各 worker 的独立映射普查；有界等式不能单独排除所有同尺寸漏计，完整账本继续独立验收。

独占 trace JSONL 使用固定 64 KiB 写缓冲、64 MiB 上限。每轮含计划/实际时刻、全部结果/前缀、旧身份检查及保留账目；驱动冻结源码/runner/输入/产物和 oracle，前后校验 hash，逐行重放并保留失败的部分 trace、控制/错误和 invalid manifest。RSS 观察仅为安全停止，不替代平台内存口径。

```sh
cargo build --workspace --bins --examples --locked
python3 scripts/verify_failure_soak.py
python3 scripts/verify_failure_soak.py --compare-beam
cargo build --release --workspace --bins --examples --locked
target/release/wings-compile build examples/beam-failure-counter.wings -o target/failure-soak-aot
WINGS_PROFILE=release python3 scripts/measure_failure_soak.py --artifact target/failure-soak-aot --output target/failure-soak-4k --state-bytes 4096 --compare-beam
```

默认 AOT 只执行 `--verify-soak` 三轮、三种 state 和模型伪造拒绝；原短子进程 CPU 5s/墙钟 10s 不变。只有显式 `--compare-beam` 跑 OTP 29，无模型回退；只有 `measure_failure_soak.py` 启动固定长测。实际结果见[验证记录](validation-failure-soak-2026-11-18.md)。这没有覆盖长时 timeout/cancel、自动重启强度、过载竞争、完整内存、CPU 硬配额或最新原生双平台，不宣称完整故障或 MVP 门禁通过。

## v11：持续取消与监督恢复

[ADR-0121](adr/0121-persistent-cancellation-recovery.md) 的 Wings 业务与 [ADR-0131](adr/0131-equivalent-beam-cancellation-recovery.md) 的真实 OTP 29 运行端共用独立重放契约：4 KiB 零初始 Bytes，两个 sender 各八条 Add，目标 mailbox 16、健康 sibling mailbox 1、共享结果 17。Hold 占实际 handler；每 sender 前两条立即超时、第三四条取消并消费，仍保留十六条队列消息与十六个 publisher 票据。满队列拒绝不得改变容量，健康 sibling 仍可提交 +7。

剩余八条取消与 stop 竞争只认一个合法结果，已消费结果不得再取。stop 不自动重启；显式新零状态目标两次 Fail 都须实际除零且不提交候选 99。第一次自动重启，第二次超限；健康 sibling 仍为 +7。本轮及下一轮新目标活着时验证前代 Pid/入口/child ID 拒绝。

BEAM 的持续有限请求 `gen_server` 只有在 caller 消费且真实回复或 monitor DOWN 后释放票据；没有把调用者退出当成撤回。持久 root 下每个目标 child 使用独立的一次/60s OTP supervisor，健康 sibling 单独监督，避免把按 supervisor 的强度错误解释成 Wings 的按 child 强度。每轮新建目标监督分支属于显式创建，原 root、适配器和 VM 持续复用。BEAM 控制进程/monitor 开销全计入 OS 作业，逻辑保留字节和栈为 null；队列/票据是真实协议计数，不能等同 Wings 的三级资源快照。

```sh
cargo build --workspace --bins --examples --locked
python3 scripts/verify_recovery_soak.py --compare-beam
cargo build --release --workspace --bins --examples --locked
target/release/wings-compile build examples/concurrent-recovery.wings -o target/recovery-aot
WINGS_PROFILE=release python3 scripts/measure_recovery_soak.py --artifact target/recovery-aot --output target/recovery-paired --compare-beam
```

普通验证一/双 worker 或 scheduler 各三轮；只有显式测量入口才在每配置每端跑 900 轮、30s 预热/60s 采样、100ms 计划间隔。原三秒轮期限、长测 CPU 240s/墙钟 130s/阶段 120s/core 0、短测 CPU 5s/墙钟 10s、64 KiB 写缓冲与 64 MiB trace 上限保持。冻结输入前后核对并保留失败；BEAM 总/online scheduler 必须一致，不把 scheduler 数当 CPU 硬配额。默认 AOT 只跑 Wings/两端模型，无 OTP 回退。实际验证见[记录](validation-beam-recovery-2026-11-18.md)，完整故障、过载/服务 I/O、平台内存和 MVP 门禁仍未完成。

## 代表性负载与性能验收冻结条件

这些是同业务对照的固定小集合，不展开整个笛卡尔积。v5执行四组非fairness短时验证，v6接独立长时入口，v7补fairness短时业务；均不自动构成完整性能验收。普通Idle与活跃场景都保持32 B逻辑请求/成功回复；逻辑state仍包含计数器与actor ID。actor ID为`1..N`，活动集合取前`A`个；其余保留状态但不处理业务消息，不冬眠。更大actor数、其他payload、冬眠与M:N另立协议版本。

| ID | actor 数 / 活跃比例 | state | mailbox 准入上限 | 调度与目的 |
|---|---|---|---|---|
| sparse-256 | 10000 / 1%（100） | 256 B | 1 | 活动 actor 轮转，固定到达 1000 request/s；包含普通 Idle 的容量成本 |
| mixed-4k | 1000 / 10%（100） | 4 KiB | 1 | 同上，避免仅验证 tiny state |
| dense-64k | 100 / 100% | 64 KiB | 1 | 同上，覆盖大状态更新/保留成本 |
| hot-queue | 100 / 100% | 4 KiB | 100 | 固定 10000 request/s，90% 到 actor 1，剩余轮转 2..100；显式拒绝超限 |
| fairness | 101 / 100% | 256 B | 1 | actor 1 连续有界 CPU 任务，其余 100 个每 10 ms 各发一次心跳；不让热 actor 掩盖冷 actor 尾延迟 |

到达时间由单调时钟 `t0 + floor(k*10^9/rate)` ns 决定，从 k=0 开始，不能等待上一请求完成后再生成下一请求。生成器迟发仍从计划时刻计延迟；不能静默跳样、暂停时间线或把拒绝请求当作不存在。hot-queue 每连续 100 次中前 90 次给 actor 1，后 10 次在其余 actor 间连续轮转。所有请求、重试、pending 表和结果采样都须有显式上限，满时记录拒绝而非继续分配；mailbox 限额包括已接受、尚未开始的消息，不含当前 handler。BEAM 没有原生有限 mailbox，必须经已验证的发送前准入层，不能拿无界 `!` 代替。

[ADR-0066](adr/0066-bounded-scheduler-mailbox-capacity.md) 已让 Wings Scheduler 可配置 100 槽（上限 256），完整受限 AOT `scheduler_overload` 检查真实 4 KiB Bytes 满队列拒绝/取消/健康 actor；ADR-0067 增加 BEAM 有限准入对照，ADR-0068 接入 v5 短时固定速率开环。仍没有本节预热/采样、饱和扫描或性能结论。

公平性 CPU 任务固定为从零开始执行 1000000 次 checked `+1` 后返回，不溢出、不使用阻塞 native；心跳到达不因其执行而暂停。记录心跳端到端 p50/p99/p999、最大值、超时比例及每 actor 最大等待；另报请求让出到实际让出的延迟，沿用[评测文档](evaluation.md)的候选 p99 <5ms，不等同于心跳业务延迟。过载与非过载组分开，不能只报聚合直方图。

未来每行先做固定负载，再做同一行的有界饱和扫描；每点预热 30s、采样 60s、独立重复 5 次。饱和扫描从该行固定速率开始逐次翻倍，最多 8 点，出现拒绝或尾延迟超标即停止提高；未找到饱和点就明确报告扫描不充分。非 fairness 请求 deadline 固定 100ms，固定负载候选要求 p99 ≤20ms、p999 ≤50ms；拒绝和超时均计入总请求数、单独报告，不能靠删样达标。分位按 nearest-rank，少于 10000 个样本的分组不报 p999，须报样本数和最大值。fairness 的受限饱和方案另行冻结，不把心跳降频当作通过。

以下性能场景验收仍为**未验证**；v1–v5 的有界正确性及 v6 的部分测量证据不能替代完整验收：

- `spawn`：父/子/租户/实例额度在创建前准入，失败不产生半创建对象；有限批量创建与退出后计费守恒，不能无限 spawn 测宿主 OOM。
- `request/reply`：关联 ID、pending 表准入、至少两个发送者的各自 FIFO、成功状态先于回复；不要求跨发送者顺序。v3 已补安全加法闭环下的双发送者，未覆盖本节开环与拒绝；普通单向 `send` 单列，不能以 request 代替全部消息行为。
- `timeout/cancel`：每个 ID 只终结一次，回复/超时竞争及迟到回复明确可观测；超时不撤回已发生的远端副作用，不自动重试造成重复业务。
- `overload/exit`：有限 mailbox 满时显式拒绝且不提交；满队列仍能终止，故障退出完成 pending 清理、monitor 通知与额度归还；失败前的外部副作用不回滚。v4 只补有限准入/主动停止正确性，仍非开环 overload；旧 32 条窗口是流控，不作为过载证据。
- 同平台同机器的 CPU/内存配额、OTP/ERTS/JIT、堆/GC、Wings 版本、生成器开销和原始输出必须随性能实验冻结。保留 BEAM 正常优化；逻辑相同不代表分配布局相同。分别报告整机 RSS 与平台口径，不能混用 macOS 物理占用与 Linux PSS；当前没有这些性能结果。

## 独立验证

先构建当前源码工具/示例，再运行 Wings/独立模型或真实三端对照：

```sh
cargo build --workspace --locked --bins --examples
cargo test --locked -p wings-vm --example beam_counter
python3 scripts/verify_counter_equivalence.py
python3 scripts/verify_counter_equivalence.py --compare-beam
cargo build --release --workspace --locked --bins --examples
WINGS_PROFILE=release python3 scripts/verify_counter_equivalence.py --compare-beam
```

Wings 驱动位于 [`beam_counter.rs`](../crates/wings-vm/examples/beam_counter.rs)，编译源码为 [`beam-counter.wings`](../examples/beam-counter.wings)。脚本默认构建临时可信产物，`--artifact <目录>` 可复用匹配的产物；`--input <文件>` 运行单个有界 JSON，保留业务退出码。JSON 严格拒绝重复键/未知字段、bool 或浮点替代整数及多余输入。真实三端模式要求 OTP 29，缺失或输出不符直接失败，不回退成模型通过。

两端共享 JSON schema；Wings 测试子进程经内部二进制桥接运行：32 B 头 `version:u64/state_bytes:u64/initial:i64/count:u64`，后接最多 256 条 32 B 请求，全部 big-endian；最大 8224 B，校验连续 sequence、operation 1/2、reserved 0 和精确长度后才装载产物。这不是通用网络/CLI 协议，不把四个 Int 参数或请求表的物理成本称为 32 B。

Wings mailbox/request 表各 32、堆四槽、一个 actor，VM/tenant/process 字节额度各 32 MiB；Configure 在业务窗口前设完整 state。每次成功由真实已提交 Bytes 核对计数器/actor ID 和固定前缀零填充，失败副本来自旧提交根，不从模型推测。固定诊断只含长度/32 B 前缀，不独立证明其余全部 padding；完整 state 初始化/复制沿已有字节原语回归，未声称导出全部状态。驱动输入/JSON/结果是有界外部测试内存，不等同全部受管计费。actor 总 deadline 3 秒，Wings 业务子进程 CPU 5 秒/墙钟 10 秒/禁 core，编译沿 CPU 20 秒/墙钟 30 秒；超时杀整个子进程组回收。资源账目回到启动基线只证明逻辑清理，不证明 RSS。

v1 套件跨三种 state 检查空/256 条、i64 两端与溢出优先级、窗口 1/32/33/256 失败位置、候选不提交、FIFO 与故障队尾终结；多 actor 正确性见 v2，短时开环见 v5，独立长时入口见 v6，CPU/突发心跳短时业务见 v7。完整 `verify_scalar_aot.py` 默认接入 Wings/模型模式，两平台 CI 无需自动安装 OTP，也不自动执行长测；真实 BEAM 结果需单独记录平台/版本，不把默认模式写成已经运行 BEAM。尚无长时公平性、完整过载性能或吞吐/整机内存门禁结论。

原 BEAM 独立 oracle 仍可单独运行：

```sh
python3 scripts/verify_beam_workload.py
python3 scripts/verify_beam_workload.py --input /path/to/workload.json
```

默认套件用独立 Python checked 模型核对三种 state 大小、跨窗口 FIFO、i64 两端和加法溢出、候选更新后失败不提交、退出后的后续请求不执行，以及 schema/字节/请求数边界拒绝。`--input` 保留 oracle 的业务退出码，只读取有界文件；不写测量产物。

当前核对版本为本机 Erlang/OTP **29**、ERTS **17.1.1**（macOS AArch64，JIT）；已实际核对 `json:decode/3`、`json:encode/1`、重复字段拒绝钩子和完整 i64 JSON 往返。更换 OTP 大版本必须重核协议，Linux x86-64 尚待独立执行本脚本。此记录不能替代双平台 Wings 验收或 BEAM 性能实验。
