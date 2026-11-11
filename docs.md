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
| [风险与里程碑](docs/roadmap.md) | 风险登记、M0–M7 范围与验收条件 |

## 维护

- 各主题的正文只在对应文档维护。保留 `docs.md` 作为目录，不存放整篇副本。
- 功能、接口与平台范围变化时，同步修改相关验收要求和里程碑。
- 架构选择及其理由记入 `docs/adr/`；实质变化新增 ADR，保留被替代的决定。
- 编码代理与贡献者的工作约定见 [AGENTS.md](AGENTS.md)。
