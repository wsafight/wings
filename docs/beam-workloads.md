# BEAM 工作负载协议 v1

本协议是 M0 的独立正确性切片，配合[评测与验收](evaluation.md)和[里程碑](roadmap.md)使用。当前只实现一个有界 Counter 的异步 request/reply、单发送者 FIFO、checked i64 和失败退出 oracle。**不表示 Wings 已实现相同业务，不是性能实验，也不作性能或内存领先声明。** 不修改现有架构、副作用或提交契约。

## 已执行的最小业务

- 一个普通 BEAM actor，初始计数器由输入给出；不冬眠、不重启。一个外部驱动进程发送请求，actor 内没有阻塞业务调用。
- 计数器和 delta 均为 `[-2^63, 2^63-1]` 的有符号整数，每次加法单独 checked；不环绕、不饱和，也不让 BEAM 的任意精度整数成为业务结果。BEAM 和 Python 可用宽整数计算临时值，但必须在编码、提交和成功回复之前检查范围。
- 逻辑 state 恰为 `256 / 4096 / 65536 B`：计数器 `i64 big-endian`、actor ID `u64 big-endian`（固定为 1），其余为零填充。Erlang 使用不可变 binary 保留该完整状态，每次成功构造候选状态；逻辑字节不等于 BEAM 堆大小、复制成本或 RSS。
- 每个请求和成功回复都有相同的 **32 B 逻辑业务记录**：`sequence:u64, operation:u64, value:i64, reserved:u64`，均为 big-endian，reserved 必须为零。请求 operation 为 1（add）或 2（add 后注入失败），value 为 delta；回复 operation 为 3，value 为提交后的计数器。JSON 和 Erlang tuple、PID、monitor 等信封成本另算，不能称物理消息也是 32 B。
- 请求 sequence 从 1 连续增长。驱动一次发送最多 32 个请求，再收齐该批回复，才发送下一批；不逐条同步调用。只保证**同一发送者到同一接收者**的 FIFO，不要求跨发送者全序。收集器按实际到达顺序检查 sequence，不能用选择性接收掩盖乱序。
- 成功路径先选定候选状态，再发布成功回复并以新状态进入下一次循环；失败路径不进入新状态、不成功回复。溢出优先于注入失败判定，actor 以携带失败序号和最后提交值的 `handler_failed` 原因退出，monitor 结束等待；已发但未处理的后续请求不再执行。不捕获失败后继续处理、不重放请求。
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

## 后续代表性负载冻结条件（尚未实现）

这些是未来同业务对照的固定小集合，不展开整个笛卡尔积，不由当前脚本接受参数或执行。普通 Idle 与活跃场景都保持 32 B 逻辑请求/成功回复；逻辑 state 仍包含计数器与 actor ID。actor ID 为 `1..N`，活动集合取前 `A` 个；其余保留状态但不处理业务消息，不冬眠。更大 actor 数、其他 payload、冬眠与 M:N 另立协议版本。

| ID | actor 数 / 活跃比例 | state | mailbox 准入上限 | 调度与目的 |
|---|---|---|---|---|
| sparse-256 | 10000 / 1%（100） | 256 B | 1 | 活动 actor 轮转，固定到达 1000 request/s；包含普通 Idle 的容量成本 |
| mixed-4k | 1000 / 10%（100） | 4 KiB | 1 | 同上，避免仅验证 tiny state |
| dense-64k | 100 / 100% | 64 KiB | 1 | 同上，覆盖大状态更新/保留成本 |
| hot-queue | 100 / 100% | 4 KiB | 100 | 固定 10000 request/s，90% 到 actor 1，剩余轮转 2..100；显式拒绝超限 |
| fairness | 101 / 100% | 256 B | 1 | actor 1 连续有界 CPU 任务，其余 100 个每 10 ms 各发一次心跳；不让热 actor 掩盖冷 actor 尾延迟 |

到达时间由单调时钟 `t0 + floor(k*10^9/rate)` ns 决定，从 k=0 开始，不能等待上一请求完成后再生成下一请求。生成器迟发仍从计划时刻计延迟；不能静默跳样、暂停时间线或把拒绝请求当作不存在。hot-queue 每连续 100 次中前 90 次给 actor 1，后 10 次在其余 actor 间连续轮转。所有请求、重试、pending 表和结果采样都须有显式上限，满时记录拒绝而非继续分配；mailbox 限额包括已接受、尚未开始的消息，不含当前 handler。BEAM 没有原生有限 mailbox，未来必须在发送前用等价准入层实现，不能拿无界 `!` 代替。

公平性 CPU 任务固定为从零开始执行 1000000 次 checked `+1` 后返回，不溢出、不使用阻塞 native；心跳到达不因其执行而暂停。记录心跳端到端 p50/p99/p999、最大值、超时比例及每 actor 最大等待；另报请求让出到实际让出的延迟，沿用[评测文档](evaluation.md)的候选 p99 <5ms，不等同于心跳业务延迟。过载与非过载组分开，不能只报聚合直方图。

未来每行先做固定负载，再做同一行的有界饱和扫描；每点预热 30s、采样 60s、独立重复 5 次。饱和扫描从该行固定速率开始逐次翻倍，最多 8 点，出现拒绝或尾延迟超标即停止提高；未找到饱和点就明确报告扫描不充分。非 fairness 请求 deadline 固定 100ms，固定负载候选要求 p99 ≤20ms、p999 ≤50ms；拒绝和超时均计入总请求数、单独报告，不能靠删样达标。分位按 nearest-rank，少于 10000 个样本的分组不报 p999，须报样本数和最大值。fairness 的受限饱和方案另行冻结，不把心跳降频当作通过。

以下验收必须在未来相应实现接入后补齐，当前全部为**未验证**：

- `spawn`：父/子/租户/实例额度在创建前准入，失败不产生半创建对象；有限批量创建与退出后计费守恒，不能无限 spawn 测宿主 OOM。
- `request/reply`：关联 ID、pending 表准入、至少两个发送者的各自 FIFO、成功状态先于回复；不要求跨发送者顺序。普通单向 `send` 单列，不能以 request 代替全部消息行为。
- `timeout/cancel`：每个 ID 只终结一次，回复/超时竞争及迟到回复明确可观测；超时不撤回已发生的远端副作用，不自动重试造成重复业务。
- `overload/exit`：有限 mailbox 满时显式拒绝且不提交；满队列仍能终止，故障退出完成 pending 清理、monitor 通知与额度归还；失败前的外部副作用不回滚。当前 escript 的 32 条窗口是流控，不是已实现 overload 验收。
- 同平台同机器的 CPU/内存配额、OTP/ERTS/JIT、堆/GC、Wings 版本、生成器开销和原始输出必须随性能实验冻结。保留 BEAM 正常优化；逻辑相同不代表分配布局相同。分别报告整机 RSS 与平台口径，不能混用 macOS 物理占用与 Linux PSS；当前没有这些性能结果。

## 独立验证

```sh
python3 scripts/verify_beam_workload.py
python3 scripts/verify_beam_workload.py --input /path/to/workload.json
```

默认套件用独立 Python checked 模型核对三种 state 大小、跨窗口 FIFO、i64 两端和加法溢出、候选更新后失败不提交、退出后的后续请求不执行，以及 schema/字节/请求数边界拒绝。`--input` 保留 oracle 的业务退出码，只读取有界文件；不写测量产物。

当前核对版本为本机 Erlang/OTP **29**、ERTS **17.1.1**（macOS AArch64，JIT）；已实际核对 `json:decode/3`、`json:encode/1`、重复字段拒绝钩子和完整 i64 JSON 往返。更换 OTP 大版本必须重核协议，Linux x86-64 尚待独立执行本脚本。此记录不能替代双平台 Wings 验收或 BEAM 性能实验。
