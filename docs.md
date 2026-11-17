# Wings VM 文档目录

项目目标与当前进度见[项目概览](README.md)，详细设计按主题拆分如下。

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
| [BEAM 工作负载 v1–v8](docs/beam-workloads.md) | 单/多 actor、双发送者、有限准入、开环与公平性长测及有界调度让出观测 |
| [风险与里程碑](docs/roadmap.md) | 风险登记、M0–M7 范围与验收条件 |

## 维护

- 各主题的正文只在对应文档维护。保留 `docs.md` 作为目录，不存放整篇副本。
- 功能、接口与平台范围变化时，同步修改相关验收要求和里程碑。
- 架构选择及其理由记入 `docs/adr/`；实质变化新增 ADR，保留被替代的决定。
- 编码代理与贡献者的工作约定见 [AGENTS.md](AGENTS.md)。
