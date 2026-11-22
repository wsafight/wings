# 架构设计

[English](../architecture.md) | **简体中文** · [返回概览](../../README.zh-CN.md) · [语言设计](language.md) · [运行时](runtime.md)

## 设计概览

Wings 采用 **`wings-lang` 单前端 + Cranelift AOT 编译后端 + Rust 受管微进程运行时** 的三层架构：

```text
┌──────────────────────────────────────────────────────────┐
│ wings-lang 源码（actor / struct / state / on）            │
│   静态类型、不可变业务值、无阻塞 receive、编译期消息校验    │
├──────────────────────────────────────────────────────────┤
│ 编译与分析工具链（wings-compile / wings-lsp）             │
│   容错语法树 -> 类型化分析 -> 结构化寄存器 IR + CFG 精确根 │
│   -> Cranelift 0.118 PIC 机器码 -> 原生动态库 + 清单      │
├──────────────────────────────────────────────────────────┤
│ 受管微进程运行时（wings-vm）                              │
│   三级资源准入、可复用协程栈池、精确非移动分块 GC         │
│   O(1) slot+generation 寻址、无锁快路径邮箱、M:N 调度器   │
│   定时器、进程监控、有界监督恢复与 stdio/TCP/HTTP 适配器  │
└──────────────────────────────────────────────────────────┘
```

> **早期演进约定**：项目处于 `0.0.x` 阶段，受管 ABI、IR 指令集、产物清单 schema 与各 Crate 接口均不保留历史兼容层；修改协议时直接升级当前实现并重编译产物。

## Workspace Crate 拆分与依赖隔离

Workspace 固定拆分为 10 个包，按依赖方向严格单向组织，不把 GC、调度器等运行时内部模块拆成碎包：

| Crate | 形态 | 职责边界 | 隔离约束 |
|---|---|---|---|
| `wings-source` | lib | 源文件标识、UTF-8 字节跨度、UTF-16 行列转换、诊断结构 | 零外部重依赖 |
| `wings-syntax` | lib | 无损词法分析、容错递归下降解析、AST 定义 | 仅依赖 `wings-source` |
| `wings-sema` | lib | 符号解析、类型推导与检查、Actor/Struct/消息协议校验 | 不依赖 IR、Cranelift 或 LSP 协议 |
| `wings-lsp` | bin + lib | 基于 stdio 的 JSON-RPC 语言服务器（诊断、悬停、跳转、补全） | **严禁依赖** `wings-ir`、`wings-codegen`、`wings-vm` |
| `wings-abi` | lib | 受管 C ABI 布局（`ScalarContext`、`EffectRequest`、`ShadowFrame`）、状态码与类型元数据 | 仅共享契约，不含编译器或运行时逻辑 |
| `wings-ir` | lib | 结构化受管 IR、栈帧/步数校验、基于内部 CFG 的反向活跃引用根计算 | 仅依赖 `wings-abi` |
| `wings-artifact` | lib | 单模块 `manifest.json` 与多模块应用包 `bundle.json` 的有界读写与校验 | 仅依赖 `wings-abi`、`serde` |
| `wings-codegen` | lib | IR 到 Cranelift 机器码生成、超块步数合并、safepoint/shadow stack 插桩与系统链接 | 隔离重型 Cranelift 依赖 |
| `wings-compile` | bin + lib | 前端到后端编排、HIR 降级（`lower`）、`check` / `build` / `build-app` 命令 | 编译期工具链入口 |
| `wings-vm` | bin + lib | 模块装载、协程执行、精确 GC、M:N 调度、三级账本与 `run` / `serve` 命令 | **严禁依赖** 前端、IR、`wings-codegen` 或 `wings-compile`（含测试依赖） |

CI 自动校验依赖树隔离，确保 `wings-lsp` 保持轻量响应，且 `wings-vm` 不引入任何编译器依赖或跨 C ABI 的栈展开（`corosensei` 显式关闭 `unwind` / `asm-unwind`）。

## 编译管线、受管 ABI 与精确 GC

1. **为什么不提供 Rust `#[actor]` 宏前端**：过程宏无法控制任意第三方 Rust 函数内部的无界循环、原生栈帧增长和寄存器内隐藏指针。专用前端使编译器能在每次函数入口、循环回边与分配点精确插入受管检查。
2. **受管 C ABI（`wings-abi`）**：
   - 生成代码接收固定 C 布局的 `*mut ScalarContext`（含原子取消标志、总步数剩余、时间片步数剩余、原生栈下界指针、shadow stack 链头、慢路径回调、堆操作回调与异步 Effect 回调）。
   - 任何算术溢出、除零、越界、步数/栈耗尽或取消均通过显式状态码逐层返回，**绝对禁止 panic/unwind 跨越 ABI 边界**。
3. **精确 CFG 引用根与分块 GC**：
   - `wings-ir` 构建与后端同构的控制流图（CFG），通过反向活跃性分析精确计算每个检查点与调用点需要保活的引用槽位，写入显式 `ShadowFrame` 链表，不做保守寄存器/原生栈猜测。
   - `wings-vm` 采用进程内非移动 mark-sweep GC，从已提交 `state`、当前 `ShadowFrame` 链、入口参数与候选回复根出发标记，并按 16 节点/根分块检查执行预算与让出请求；字节缓冲支持池化复用。
4. **产物校验与装载安全边界**：
   - 单 Actor 构建输出 `manifest.json` 与平台动态库（`program.dylib` / `program.so`）；多 Actor 应用输出 `bundle.json` 与各子模块目录。
   - 装载前仅接受普通文件清单（上限 64 KiB），预解析 ELF64 / Mach-O64 段表核算页对齐映像跨度（上限 64 MiB），并校验动态库仅依赖系统基础运行库（macOS `libSystem` / Linux `libc`），拒绝额外旁加载库。

## 目标平台范围

首要支持并同步验证 **macOS AArch64（Apple Silicon）** 与 **Linux x86-64（GNU）**。平台相关实现收敛在 `wings-vm` 内部（kqueue / epoll+eventfd 唤醒、pthread 可用栈边界核对、Mach / `/proc` 内存观测与动态库格式预检），上层 IR 与语言语义保持平台无关。
