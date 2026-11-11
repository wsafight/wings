# 包发布

[文档目录](../docs.md) · [项目概览](../README.md) · [Crate 拆分](crates.md)

## 当前状态

源码仓库为 <https://github.com/wsafight/wings>。workspace 当前统一为 `0.0.2`，全部十个已有实现的包已发布：前端、LSP、标量 ABI、IR、产物、Cranelift 后端与可信本机执行原型。原前端 `0.0.1` 保持不变，不覆盖已发布版本。不承诺稳定 API，也不宣称完整微进程 VM、抢占、GC 或性能目标已实现。

`0.0.2` 发布收尾后专注 M0 功能开发，暂不安排新版本。后续代码与文档仍经 PR 合并，不因合并自动上传包，也不为同步发布记录再次发版。

维护者已确认使用 `Apache-2.0` 许可证，全文见仓库根目录 [LICENSE](../LICENSE)，各包继承 workspace 的许可证元数据，仅允许向 crates.io 发布。crates.io 发布成功后不能覆盖同一版本，因此先做打包检查。

crates.io 没有独立的项目创建或名称预留步骤，首次成功发布才会注册包名。不发布空包占位，也不建立 `wings` 或 `wings-lang` 聚合包。

## 合并与发布流程

`main` 已启用分支保护：代码与文档变更必须经 PR 合并，要求分支与 main 保持同步，并通过 `native (macos-15, arm64)`、`native (ubuntu-24.04, x86_64)` 和 `msrv` 三项检查。管理员同样受限，禁止强推和删除 main，合并前解决讨论。当前不要求额外审批人数，PR 门禁与独立人工审批是两回事。

包发布是用户明确要求后独立手动执行的操作，不是 PR 事件触发的自动发布。先合并通过 CI 的发布元数据，再从已合并的干净源码执行 dry-run、归档核对与上传；发布凭证不写入工作流、仓库或日志。任何包失败都先核对注册表版本和失败原因，保留部分发布记录，不重复上传已有版本或跳过验证。

## 名称检查

2026-11-12 发布后通过 crates.io API 核对；十个包的 `0.0.2` 均可查询，所有者为 `wsafight`，且未撤回：

| 名称 | 查询结果 | 本项目安排 |
|---|---|---|
| `wings-source` | 已发布 `0.0.2` | 源码位置与诊断实现 |
| `wings-syntax` | 已发布 `0.0.2` | 词法与容错解析实现 |
| `wings-sema` | 已发布 `0.0.2` | 名称与局部类型检查实现 |
| `wings-abi` | 已发布 `0.0.2` | 标量原型 ABI，不是完整受管 ABI |
| `wings-ir` | 已发布 `0.0.2` | 有界直线标量 IR 与校验 |
| `wings-artifact` | 已发布 `0.0.2` | 有界产物清单与目标校验 |
| `wings-codegen` | 已发布 `0.0.2` | Cranelift AOT 与系统链接 |
| `wings-compile` | 已发布 `0.0.2` | 增加可信本机标量 AOT 构建 |
| `wings-lsp` | 已发布 `0.0.2` | stdio 语言服务器 |
| `wings-vm` | 已发布 `0.0.2` | 可信本机标量执行原型，不是安全沙箱 |
| `wings` | 其他项目已注册 | 不发布这个名称 |
| `wings-lang` | 未注册 | 仅作为当前语言名，不发布同名 crate |

未注册只代表查询当时可用，不保证发布时仍可用，也不代表商标或语言名称没有冲突。已有 [`wings`](https://github.com/DouglasDwyer/wings) 是 Rust WASM 插件系统，与本项目无关；对外使用 Wings VM 区分，不发布同名 `wings` 包。命名决定见 [ADR-0008](adr/0008-wings-naming.md)。

## 发布顺序

`0.0.2` 统一按依赖顺序发布，全部包均有实现，不发布聚合包：

1. `wings-source`
2. `wings-syntax`
3. `wings-sema`
4. `wings-abi`
5. `wings-ir`
6. `wings-artifact`
7. `wings-codegen`
8. `wings-compile`
9. `wings-lsp`
10. `wings-vm`

内部依赖同时指定本地路径和 `0.0.2` 版本。打包发布时使用 crates.io 上的版本，不能只有 `path`；每次发布后等待索引可见，再检查并发布依赖它的包。

先运行 workspace 检查、测试、Clippy、格式检查和示例检查。每个包先执行 `cargo publish -p <包名> --dry-run --locked --registry crates-io`，检查归档内容、许可证、元数据和独立构建，再去掉 `--dry-run`。依赖尚未发布时，上层包的完整打包构建可能失败，不能用跳过验证当作发布验收。

本机 Cargo 配置使用镜像索引；实际发布前，命令级覆盖为 `CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/`，避免把发布请求发向镜像。不将本机代理或凭证写入仓库。发布凭证至少需要相应包的发布权限，失败后先检查错误与包版本，不盲目重发。

## 发布后核对

核对 crates.io 的名称、版本、所有者、许可证和源码地址，以及依赖是否都来自预期注册表。确认安装命令和已实现功能一致；未经验证的 VM、性能与平台能力不写入发布说明。

## 0.0.2 发布记录

2026-11-12，全部十个包依照上述顺序完成 dry-run、归档检查、独立构建与实际上传。归档对应 [PR #1](https://github.com/wsafight/wings/pull/1) 合并后的干净源码提交 `58dc9073ae17162375de0b666764170a19938750`。已检查各包许可证、仓库地址、源码提交和内部依赖版本；crates.io API 复核十个版本均为 `0.0.2`、所有者为 `wsafight`、许可证为 `Apache-2.0`，且未撤回，内部依赖均为注册表上的 `^0.0.2`。

[发布 PR 检查](https://github.com/wsafight/wings/actions/runs/43091081968)和[合并后 main 检查](https://github.com/wsafight/wings/actions/runs/43092139855)均通过 macOS AArch64、Linux x86-64 原生执行与 Rust 1.85 构建。端到端脚本覆盖 Counter 连续状态、Bool/Unit、整数溢出、除零、输入和产物拒绝；不包含抢占、GC、mailbox 或 BEAM 对照。`cargo test` 当前没有 Rust 测试用例，不能把零用例通过写成行为覆盖。

另从 crates.io 将三个命令安装到 macOS AArch64 临时目录，已验证示例检查、命令帮助、Counter 编译及七条消息的执行结果，以及 LSP 初始化、版本 `0.0.2` 和正常关闭。GitHub 发布入口为 [v0.0.2](https://github.com/wsafight/wings/releases/tag/v0.0.2)。

从 crates.io 安装三个工具后可检查可信本机 AOT；示例路径相对于本仓库，输出目录须不存在：

```sh
CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/ cargo install wings-compile wings-vm wings-lsp --version 0.0.2 --locked --registry crates-io
wings-compile check examples/counter.wings
wings-compile build examples/counter.wings -o target/installed-counter-aot
wings-vm run --trusted target/installed-counter-aot < examples/counter.messages.jsonl
wings-lsp --help
```

`--trusted` 仅用于自己构建且与清单匹配的原生代码，清单校验不是安全认证。需要 Rust 1.85 或更新版本，AOT 构建还需系统 C 链接工具链。不提供预构建二进制，不将原型成功执行视为 M0 完成。

## 0.0.1 发布记录

2026-11-12，以下包按依赖顺序完成 dry-run、独立打包构建与实际发布，所有者均为 `wsafight`，许可证均为 `Apache-2.0`：

- [`wings-source`](https://crates.io/crates/wings-source/0.0.1)
- [`wings-syntax`](https://crates.io/crates/wings-syntax/0.0.1)
- [`wings-sema`](https://crates.io/crates/wings-sema/0.0.1)
- [`wings-compile`](https://crates.io/crates/wings-compile/0.0.1)
- [`wings-lsp`](https://crates.io/crates/wings-lsp/0.0.1)

包归档对应源码提交 `247cf72ad24f78d7451955a90acdaf359361900e`。已检查各归档内的许可证、仓库地址、源码提交及依赖版本，并通过 crates.io API 复核版本、所有者、许可证、未撤回状态和依赖。

macOS AArch64 上已通过 workspace 检查、Clippy、格式检查、示例静态检查、命令帮助和 LSP 初始化、文档诊断及退出的冒烟验证。`cargo test --workspace --locked` 执行成功，但当前没有自动化测试用例，不把零用例通过写成行为测试覆盖。Linux 验证、AOT、VM 执行和性能基线仍未完成。

另从 crates.io 将两个命令安装到临时目录，验证示例检查、LSP 帮助、初始化和正常关闭。安装命令如下；示例路径相对于本仓库：

```sh
CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/ cargo install wings-compile wings-lsp --version 0.0.1 --locked --registry crates-io
wings-compile check examples/counter.wings
wings-lsp --help
```

版本为早期前端预览，不承诺稳定 API，也不包含 VM 或预构建二进制。GitHub 发布入口为 [v0.0.1](https://github.com/wsafight/wings/releases/tag/v0.0.1)。
