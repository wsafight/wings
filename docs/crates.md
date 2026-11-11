# Crate 拆分与开发工具

[文档目录](../docs.md) · [架构设计](architecture.md) · [架构决策](adr/README.md)

以下是 Cargo workspace 的目标拆分，按需逐步实现。拆分以依赖方向和使用场景为准，不把 GC、调度器等每个模块都做成独立包。

成员目录统一放在 `crates/<crate-name>/`。同时提供库与命令的包分别使用 `src/lib.rs` 和 `src/main.rs`，命令行入口只做参数、日志与退出状态适配，业务逻辑放在库中。

## 命名与层次

**`wings-vm`** 是虚拟机 crate 和运行命令的拟用名称，负责装载、微进程、内存、调度、消息和监督。包内包含库与薄的命令行入口，库便于测试和组织实现，首版不承诺稳定的嵌入式 SDK。

编译命令为 `wings-compile`，语言服务器为 `wings-lsp`。`wings-lang` 是语言名，不再设一个职责模糊的同名聚合 crate。完整理由见 [ADR-0001](adr/0001-vm-boundary-and-naming.md)。当前名称查询与发布安排见[包发布](publishing.md)，商标核查仍未完成。

## 计划中的包

已有源码、语法、语义、编译命令和 LSP 五个前端包。当前另有 `wings-abi`、`wings-ir`、`wings-artifact`、`wings-codegen` 与 `wings-vm`，仅实现有实际调用方的标量 AOT 切片；十个包均已发布 `0.0.2`。`wings-compile build` 可生成可信本机产物，VM 只直接调用无循环、无业务堆分配的入口；完整受管 IR、ABI、装载权限、调度与 GC 尚未实现。下表仍描述目标职责，不代表已经全部实现。

未发布源码在原有边界内增加执行控制：`wings-abi` 定义 v3 context、慢路径签名与状态码，`wings-codegen` 固定插入原子检查，`wings-vm` 内部 `execution` 模块提供同线程活栈恢复、每次调用总预算与共享控制令牌。仅 VM 新增 `corosensei 0.3.3` 的 `default-stack` 依赖，不启用 unwind，不引入编译器依赖，也不新增 crate。源码变更不覆盖 crates.io `0.0.2`，细节见 [ADR-0011](adr/0011-scalar-stackful-resume.md)。

| Crate | 形态 | 负责内容 | 不负责 |
|---|---|---|---|
| `wings-source` | lib | 源文件 ID、UTF-8 字节范围、行索引、公共诊断结构 | LSP 协议、语法、机器码 |
| `wings-syntax` | lib | lexer、容错解析、保留原始文本的语法树、AST 访问接口 | 名称解析、类型检查、代码生成 |
| `wings-sema` | lib | 模块与名称解析、类型和副作用检查、类型化 HIR、符号查询 | Cranelift、程序执行、LSP 传输 |
| `wings-ir` | lib | 受管执行 IR、控制流、safepoint 与根描述、IR 校验 | 前端语法、OS I/O、机器码后端 |
| `wings-abi` | lib | 受管值与错误的 ABI 定义、内建标识、目标调用约定与兼容版本 | Rust 容器跨边界传递、编译器或 VM 实现 |
| `wings-artifact` | lib | AOT 产物清单、版本化元数据的编码与有界读取、目标与 ABI 校验 | 装载执行、代码生成、稳定性尚未确定的业务 schema |
| `wings-codegen` | lib | 受管 IR 到 Cranelift 的 lowering、目标对象与根元数据生成 | 源码解析、编辑器查询、启动 VM |
| `wings-compile` | lib + bin | 编译编排、HIR 到受管 IR 的 lowering、链接与产物组装、编译命令入口 | 执行用户程序、编辑器协议 |
| `wings-vm` | lib + bin | 产物装载、微进程、堆与 GC、调度、消息、额度、监督、I/O 和启动入口 | 源码分析、机器码生成、编辑器功能 |
| `wings-lsp` | lib + bin | LSP 服务、文档同步、诊断与编辑器查询适配 | 另写一套解析器、编译机器码、执行用户程序 |

`wings-sema` 的 HIR 表达业务语义，供编译与编辑器查询共用；`wings-ir` 表达调度点、根和受管调用等执行细节。LSP 不需要把程序降到执行 IR 才能回答悬停或跳转。

当前标量路径暂由编译驱动将通过语义检查的 AST 降为带类型的直线寄存器 IR；后端不依赖语法或语义库。尚无独立 HIR、控制流或根描述，不将当前标量 IR 宣称为完整受管 IR。扩展堆与控制流前须按 [ADR-0009](adr/0009-scalar-aot-prototype.md) 补齐契约。

`wings-artifact` 的装载前校验与 `wings-vm` 的可执行映射分开。前者检查目标、版本、长度和结构，后者仍须验证来源、权限与受管入口；通过格式校验不代表代码可信。对象文件格式、最终打包和签名方式在 M0 确定。

## 依赖方向

下表中的依赖均指 workspace 内部的直接依赖；外部库与版本在实现时确定。

| Crate | 可依赖的内部包 |
|---|---|
| `wings-source` | 无 |
| `wings-syntax` | `wings-source` |
| `wings-sema` | `wings-source`、`wings-syntax` |
| `wings-abi` | 无 |
| `wings-ir` | `wings-source`、`wings-abi` |
| `wings-artifact` | `wings-abi` |
| `wings-codegen` | `wings-source`、`wings-ir`、`wings-abi`、`wings-artifact` |
| `wings-compile` | `wings-source`、`wings-syntax`、`wings-sema`、`wings-ir`、`wings-abi`、`wings-artifact`、`wings-codegen` |
| `wings-vm` | `wings-abi`、`wings-artifact` |
| `wings-lsp` | `wings-source`、`wings-syntax`、`wings-sema` |

- 编译器与 VM 通过 ABI 定义和产物格式协作，不能互相依赖实现 crate。
- LSP 只依赖源码、语法和语义分析；它的构建与测试不应引入 Cranelift、VM 或汇编切换桩，也不通过后台启动编译命令完成普通查询。
- 公共诊断留在 `wings-source`，LSP 类型和 JSON-RPC 留在 `wings-lsp`，避免编辑器协议反向进入语言内核。
- `wings-abi` 的 Rust 定义用于共享约定，不代表 Rust 内存布局天然稳定。跨产物边界的表示须明确布局，元数据使用显式编码，不转存 Rust 对象。

## 包内先保留的模块

`wings-vm` 内先按 `process`、`heap`、`gc`、`scheduler`、`mailbox`、`resource`、`supervision`、`loader`、`io` 和 `platform` 组织。macOS 与 Linux 的 I/O、映射和切换实现放在平台模块，不预先拆出平台 crate。

`wings-compile` 内分开编译驱动、lowering、链接与命令行适配。`wings-codegen` 隔离重型后端依赖，以便前端和 LSP 单独开发。测试夹具先跟随各自 crate，不提前建立只有转发接口的公共测试包。

只有出现多个实际调用方、独立测试或明确的构建隔离收益时，再拆分 GC、分配器、codec、平台层或增量分析包。

## LSP 包

`wings-lsp` 作为独立语言服务器发布，首版用 stdio 传输 JSON-RPC。stdout 只输出协议消息，日志写到 stderr；编辑器插件只是启动与配置适配，不替代语言服务器。协议库的选型留到实现时决定。

### 首批能力

- 文档打开、修改、关闭，以及按文档版本维护的内存快照。未保存内容优先于磁盘文件。
- 容错解析与语义诊断。输入不完整时仍尽量保留可用语法结构，不能因缺少一个结束符就丢弃整份文件的查询能力。
- 符号跳转、悬停和基础补全，复用 `wings-sema` 的符号与类型信息。
- 项目文件与模块依赖查询，只重算改动影响到的内容；M0 先定义查询接口，不预定某个增量计算框架。

引用查找、重命名和语义高亮在基础查询稳定后补充。只声明已经实现的 capability，不用空响应伪装支持。重命名先检查符号身份和冲突，再返回编辑列表，不直接改写用户文件。

### 一致性与响应

- 编译器和 LSP 共用同一套解析与语义规则。LSP 可为不完整输入恢复，但不能另定义一套“编辑器专用语言”。
- 内部位置使用 UTF-8 字节范围；协议层按协商的位置编码转换，未协商时使用 LSP 默认的 UTF-16。行索引负责转换，并测试中文、emoji、组合字符和 CRLF。
- 查询绑定文档版本与不可变分析快照。新版本到达或请求取消后丢弃过时结果，不覆盖新诊断；分析任务不能阻塞协议收发。
- 文件大小、分析工作量和并发查询设限，长分析可取消。LSP 不装载 native 内建，也不运行用户程序来获得类型或补全信息。

## 落地顺序

- **M0**：确定上述依赖边界，打通最小编译和双平台装载路径；前端同时验证容错语法树、公共诊断和可查询语义接口。
- **M1**：按需要建立 workspace 包，交付 `wings-vm`、`wings-compile` 和首版 `wings-lsp`。LSP 包含文档同步、诊断、跳转、悬停和基础补全。
- **M2 及以后**：根据实际编辑负载完善增量失效与取消，再增加引用查找、重命名和语义高亮。包数量根据代码与构建数据调整，不为凑齐目录创建空壳。

具体验收见[评测与验收](evaluation.md)，采用这套拆分的理由见 [ADR-0006](adr/0006-workspace-and-lsp.md)。
