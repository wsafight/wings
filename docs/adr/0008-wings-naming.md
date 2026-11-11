# ADR-0008：统一使用 Wings VM 名称

- 日期：2026-11-12
- 状态：已采纳；前端已采用，VM 尚未实现
- 补充范围：[ADR-0001](0001-vm-boundary-and-naming.md) 的命名选择，以及 [ADR-0002](0002-language-and-aot.md) 与 [ADR-0006](0006-workspace-and-lsp.md) 中的语言和包名约定，不改变职责、依赖方向或执行模型

## 背景

项目、语言、工具与包需要统一的名称，便于文档、命令和依赖相互对应。维护者选择 Wings，对外使用 Wings VM 区分同名软件，不把名称寓意当作性能承诺。

## 决定

- 项目名为 Wings，对外称 Wings VM；源码仓库为 <https://github.com/wsafight/wings>。
- 现有包统一为 `wings-source`、`wings-syntax`、`wings-sema`、`wings-compile` 和 `wings-lsp`。后续计划包沿用 `wings-` 前缀，不提前创建空包。
- 执行引擎与命令使用 `wings-vm`，编译工具使用 `wings-compile`，语言服务器使用 `wings-lsp`；业务语言名为 `wings-lang`，不是聚合 crate。
- 源码扩展名为 `.wings`；示例、命令帮助、LSP 标识、Rust 导入、Cargo 元数据和文档统一使用 Wings 名称。

## 替代方案

- 发布 `wings` 聚合包：已有同名 Rust WASM 插件项目，且本项目不需要额外聚合包，不采用。
- 各工具独立取名：命令与依赖难以对应同一项目，不采用。

## 影响

包名、Rust crate 路径、命令与示例路径使用一致前缀。初始版本为 `0.0.1`，许可证由维护者确认为 `Apache-2.0`；当前只交付静态检查前端和 LSP，不宣称 VM 或 AOT 已可用。

名称查询只记录查询时的注册表状态，未完成全面名称或商标核查，也不暗示与其他 Wings 项目有关联。各包仅向 crates.io 发布，不发布空包占名。

## 验证要求

- 运行 workspace 检查、测试、Clippy 与格式检查，确认所有依赖和 Rust 导入使用新名称。
- 检查两个命令的帮助、`.wings` 示例静态检查和 LSP 初始化、诊断标识。
- 检查文档相对链接、代码围栏及名称一致性。
- 检查包归档、许可证与仓库元数据，按依赖顺序执行发布 dry-run 和实际发布，不发布尚未实现的 VM。

## 重审条件

发现同领域名称冲突、商标问题或工具集成歧义时再评估名称；不能仅凭 crate 尚未注册认定不存在冲突。

## 相关文档

- [项目概览](../../README.md)
- [包发布](../publishing.md)
- [Crate 拆分](../crates.md)
