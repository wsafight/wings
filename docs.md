# Wings VM 文档目录

项目目标与当前进度见[项目概览](README.md)，详细设计按主题拆分如下。

[2026-11-18 清单读取边界](docs/validation-manifest-loading-2026-11-18.md) 保留 FIFO 打开阻塞的修改前采样，核对普通文件/符号链接、固定读取边界与加载拒绝退账；另记录后续完整 Debug 采样的观察干扰、工具超时与 Counter 十秒失败。

[2026-11-18 宿主线程栈边界](docs/validation-native-thread-stacks-2026-11-18.md) 保留请求值不等于实际可用范围的首次失败，核对原预留桶、启动拒绝和 join 退账。

[2026-11-18 启动复验与原生采样](docs/validation-startup-followup-2026-11-18.md) 保留 Debug TCP 五秒与让出入口十秒失败，区分后续定向通过、模块 dlopen 采样与尚未定位的根因。

[2026-11-18 原子让出进展验证](docs/validation-atomic-progress-2026-11-18.md) 区分无限步骤片下的同 worker 健康进展、固定 FIFO 丢样和完整公平性门禁，另保留启动采样的有限旁证。

[2026-11-18 固定原生路径缓冲验证](docs/validation-native-paths-2026-11-18.md) 保留原生文件系统前提失败，核对固定来源/结果、符号链接与原模块边界。

[2026-11-18 恢复区间长测验证](docs/validation-resume-fairness-2026-11-18.md) 保留 v12 事件满额失败，记录 v13 的显式丢样、冻结业务和独立逐次时钟配对。

[2026-11-18 Scheduler 恢复区间验证](docs/validation-scheduler-slices-2026-11-18.md) 区分步骤片实际返回与原子请求延迟，记录有界计费 FIFO、丢样守恒和一/双 worker AOT 检查。

[2026-11-18 记录回复配额拒绝验证](docs/validation-quota-reply-2026-11-18.md) 保留原两次分配失败、三级受管拒绝和共享四维准入的回归，区分内部状态与最终宿主错误。

[2026-11-18 handler 查询错误验证](docs/validation-handler-lookup-2026-11-18.md) 保留满额下外部名字放大错误的复现、共享名称规则、零分配成功查询和有界拒绝。

[2026-11-18 持续取消与恢复验证](docs/validation-recovery-reuse-2026-11-18.md) 记录同一 Scheduler 中的保留请求、取消/stop 终态、一次全新重启和强度超限，区分普通三轮与显式长测。

[2026-11-18 持续失败生命周期验证](docs/validation-failure-soak-2026-11-18.md) 区分三轮普通回归与独立 30s/60s 长测，记录同一 Scheduler 的槽复用、合法失败前缀和计费清理。

[2026-11-18 调用参数诊断与候选回退](docs/validation-invocation-arguments-2026-11-18.md) 区分实际分配次数、闭包传递失败与冻结端到端负载，保留原实现和有界参数副本回归。

[2026-11-18 handler 迁移间隔消融](docs/validation-handler-migration-batch-2026-11-18.md) 保留两对反序完整事件、尾延迟/拒绝及未采用候选的理由。

[2026-11-18 原生同步计费验证](docs/validation-native-sync-accounting-2026-11-18.md) 保留修复前根门闩漏计失败、准入与发布前初始化的回归及完整验证状态。

## 阅读顺序

| 文档 | 内容 |
|---|---|
| [产品定位](docs/product.md) | 性能与内存目标、适用场景、方案对照与成功标准 |
| [架构设计](docs/architecture.md) | 技术选型、编译后端、受管 ABI、GC 与平台范围 |
| [Crate 拆分](docs/crates.md) | workspace 包职责、依赖方向和 LSP 方案 |
| [包发布](docs/publishing.md) | 初始版本、名称查询、许可证与依赖发布顺序 |
| [架构决策](docs/adr/README.md) | 已采纳的决策、理由、替代方案与重审条件 |
| [运行时](docs/runtime.md) | 微进程、内存、冬眠、调度、消息、额度、监督与热更新 |
| [语言设计](docs/language.md) | 类型化消息入口、具名状态、尾表达式、异步请求与副作用时机 |
| [激进优化](docs/optimizations.md) | 优化候选、适用条件、风险、实验顺序与回退路径 |
| [评测与验收](docs/evaluation.md) | BEAM 对照、负载矩阵、指标、正确性与故障注入 |
| [2026-11-13 验证记录](docs/validation-2026-11-13.md) | macOS 原生与 Linux x86-64 隔离仿真结果及未通过项 |
| [2026-11-14 验证记录](docs/validation-2026-11-14.md) | 大容量派发、初始化池、并发恢复、平台内存观测、计费 Bytes 消息和剩余门禁 |
| [2026-11-15 验证记录](docs/validation-2026-11-15.md) | 计费 IntList 回复、出口根、结果所有权与平台验证边界 |
| [2026-11-16 验证记录](docs/validation-2026-11-16.md) | 计费 Bytes 回复、debug/release 验证、mixed-4k 五对内存/延迟试测与剩余 MVP 门禁 |
| [2026-11-16 轮询优化验证](docs/validation-optimization-2026-11-16.md) | 宿主结果未就绪免锁、确定性回归与同负载前后配对实验 |
| [2026-11-16 计时通知优化验证](docs/validation-timer-optimization-2026-11-16.md) | hot-queue 五对计时通知实验、退化数据与回退验证 |
| [2026-11-16 空请求维护优化验证](docs/validation-empty-request-optimization-2026-11-16.md) | 空语言请求表快路径、双端存活计数与配对实验 |
| [2026-11-17 锁区间统计验证](docs/validation-lock-statistics-2026-11-17.md) | 默认关闭的 worker 锁区间诊断、计费与真实 AOT 开关对照 |
| [2026-11-17 字符串值验证](docs/validation-string-values-2026-11-17.md) | 独立 UTF-8 String、本地/状态/函数、分块检查与 MVP 剩余门禁 |
| [2026-11-17 字符串字面量验证](docs/validation-string-literals-2026-11-17.md) | 双引号/转义、有界只读常量、分块计费副本与全量回归 |
| [2026-11-17 字符串消息验证](docs/validation-string-messages-2026-11-17.md) | 计费 String 参数、混合载荷、类型边界和单/双 worker 回归 |
| [2026-11-17 字符串回复验证](docs/validation-string-replies-2026-11-17.md) | 独立 StringReply、分块出口/完成导入、结果生命周期及全量回归 |
| [2026-11-17 控制 token 验证](docs/validation-control-tokens-2026-11-17.md) | 可共享取消/让出标志计费、末 owner 生命周期和常驻控制回归 |
| [2026-11-17 具名记录验证](docs/validation-named-records-2026-11-17.md) | 名义类型、嵌套精确根、计费布局、取消/失败/重启与真实 AOT |
| [2026-11-17 唤醒租约验证](docs/validation-wakeup-leases-2026-11-17.md) | 末 notifier 保留计费、三级准入、跨线程清理与 Linux 交叉检查边界 |
| [2026-11-17 全量测试入口验证](docs/validation-workspace-runner-2026-11-17.md) | 所有 test target 实际受限执行、指纹清单、失败证据与 CI 覆盖 |
| [2026-11-17 字符串比较验证](docs/validation-string-equality-2026-11-17.md) | 完整内容相等、分块预算/取消、精确根和原限全量回归 |
| [2026-11-17 TCP 启动取证](docs/validation-tcp-startup-2026-11-17.md) | 完整握手绝对 deadline、失败日志、原限复验及动态加载采样边界 |
| [2026-11-17 具名记录消息验证](docs/validation-named-record-messages-2026-11-17.md) | 有界叶值展开、目标重建、单/双 worker、失败/取消/停止及全量回归 |
| [2026-11-17 标量账户绑定验证](docs/validation-scalar-account-binding-2026-11-17.md) | 原 process 身份、换池拒绝、脱栈迁移、元数据租约与真实 BEAM 回归 |
| [2026-11-17 类型化跨线程入口验证](docs/validation-typed-ingress-2026-11-17.md) | 计费混合副本、原标量拒绝、GC/失败/停止、请求双端租约与全量回归 |
| [2026-11-18 具名记录回复验证](docs/validation-named-record-replies-2026-11-18.md) | 有界独立叶复制、完成事件重建、单/多 worker AOT 与剩余平台门禁 |
| [2026-11-18 CLI 记录输出验证](docs/validation-cli-record-replies-2026-11-18.md) | 有界流式记录 JSON、全部叶值、输出扩张/写失败与原限制回归 |
| [2026-11-18 字符串索引与切片验证](docs/validation-string-slicing-2026-11-18.md) | Unicode scalar value、独立副本、GC/配额/取消、LSP 与原限回归 |
| [2026-11-18 模块映像准入验证](docs/validation-module-image-2026-11-18.md) | 原生页/零填充/空洞、装载前准入、精确预算、末 owner 和失败退账 |
| [2026-11-18 模块依赖边界验证](docs/validation-module-dependencies-2026-11-18.md) | 系统运行库白名单、额外只读依赖拒绝、格式边界与装载前退账 |
| [2026-11-18 CPU 配额条件验证](docs/validation-cpu-quota-2026-11-18.md) | 只读 cgroup v2 控制、整作业范围、格式/变化模型及 macOS 早拒绝 |
| [2026-11-18 hot-queue 原生采样](docs/validation-hotqueue-profile-2026-11-18.md) | 同一冻结源码的两种速率、原始重放、等待/锁栈样本及真实 BEAM 短测 |
| [2026-11-18 BEAM 恢复对照](docs/validation-beam-recovery-2026-11-18.md) | 有限请求双端生命周期、超时/取消、真实失败、独立 child 监督及原限长测 |
| [2026-11-18 多 actor 失败取证](docs/validation-multi-counter-evidence-2026-11-18.md) | 读时限额、全程 I/O 期限、输入身份、双流前缀及进程组清理 |
| [2026-11-18 开环 worker 锁诊断](docs/validation-open-loop-locks-2026-11-18.md) | 显式长测统计、join 后累计、handler 数核对与两对反序扰动诊断 |
| [2026-11-18 worker CPU 区间](docs/validation-worker-cpu-2026-11-18.md) | 默认关闭的线程 CPU、终态/缺失、末 owner 计费与同业务诊断 |
| [2026-11-18 模块路径暂存](docs/validation-module-paths-2026-11-18.md) | 大来源副本复现、计费路径边界、固定错误和分配请求计数 |
| [BEAM 工作负载 v1–v11](docs/beam-workloads.md) | 单/多 actor、双发送者、有限准入、开环/公平性长测及取消恢复 |
| [风险与里程碑](docs/roadmap.md) | 风险登记、M0–M7 范围与验收条件 |

## 维护

- 各主题的正文只在对应文档维护。保留 `docs.md` 作为目录，不存放整篇副本。
- 功能、接口与平台范围变化时，同步修改相关验收要求和里程碑。
- 架构选择及其理由记入 `docs/adr/`；实质变化新增 ADR，保留被替代的决定。
- 编码代理与贡献者的工作约定见 [AGENTS.md](AGENTS.md)。
