# 包发布

[文档目录](../docs.md) · [项目概览](../README.md) · [Crate 拆分](crates.md)

## 当前状态

源码仓库为 <https://github.com/wsafight/wings>。workspace 的初始版本统一为 `0.0.1`。已发布版本只有前端和 LSP，没有可运行的 VM，也不生成机器码；当前源码新增的标量 AOT 原型尚未发布，新建后端与执行包保持 `publish = false`。不承诺稳定 API，本轮不重复发布或覆盖已发布的 `0.0.1`。

维护者已确认使用 `Apache-2.0` 许可证，全文见仓库根目录 [LICENSE](../LICENSE)，各包继承 workspace 的许可证元数据，仅允许向 crates.io 发布。crates.io 发布成功后不能覆盖同一版本，因此先做打包检查。

crates.io 没有独立的项目创建或名称预留步骤，首次成功发布才会注册包名。不发布空壳 VM 或同名聚合包占位；五个已有功能的包已完成首个版本发布。

## 名称检查

2026-11-12 通过 crates.io API 查询并在发布后复核：

| 名称 | 查询结果 | 本项目安排 |
|---|---|---|
| `wings-source` | 已发布 `0.0.1` | 已有源码位置与诊断实现 |
| `wings-syntax` | 已发布 `0.0.1` | 已有词法与容错解析实现 |
| `wings-sema` | 已发布 `0.0.1` | 已有名称与局部类型检查实现 |
| `wings-compile` | 已发布 `0.0.1` | 已有静态检查命令，不生成机器码 |
| `wings-lsp` | 已发布 `0.0.1` | 已有 stdio 语言服务器 |
| `wings-vm` | 未注册 | 已有未发布的标量原型；完成相应验收后再决定发布 |
| `wings` | 其他项目已注册 | 不发布这个名称 |
| `wings-lang` | 未注册 | 仅作为当前语言名，不发布同名 crate |

未注册只代表查询当时可用，不保证发布时仍可用，也不代表商标或语言名称没有冲突。已有 [`wings`](https://github.com/DouglasDwyer/wings) 是 Rust WASM 插件系统，与本项目无关；对外使用 Wings VM 区分，不发布同名 `wings` 包。命名决定见 [ADR-0008](adr/0008-wings-naming.md)。

## 发布顺序

已发布的前端 `0.0.1` 按以下顺序交付；后续版本须先确定新增内部依赖的验收与发布安排，不能直接重复此清单：

1. `wings-source`
2. `wings-syntax`
3. `wings-sema`
4. `wings-compile`
5. `wings-lsp`

内部依赖同时指定本地路径和 `0.0.1` 版本。打包发布时使用 crates.io 上的版本，不能只有 `path`；每次发布后等待索引可见，再检查并发布依赖它的包。

先运行 workspace 检查、测试、Clippy、格式检查和示例检查。每个包先执行 `cargo publish -p <包名> --dry-run --locked --registry crates-io`，检查归档内容、许可证、元数据和独立构建，再去掉 `--dry-run`。依赖尚未发布时，上层包的完整打包构建可能失败，不能用跳过验证当作发布验收。

本机 Cargo 配置使用镜像索引；实际发布前，命令级覆盖为 `CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/`，避免把发布请求发向镜像。不将本机代理或凭证写入仓库。发布凭证至少需要相应包的发布权限，失败后先检查错误与包版本，不盲目重发。

## 发布后核对

核对 crates.io 的名称、版本、所有者、许可证和源码地址，以及依赖是否都来自预期注册表。确认安装命令和已实现功能一致；未经验证的 VM、性能与平台能力不写入发布说明。

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
