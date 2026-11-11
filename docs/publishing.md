# 包发布

[文档目录](../docs.md) · [项目概览](../README.md) · [Crate 拆分](crates.md)

## 当前状态

源码仓库为 <https://github.com/wsafight/wings>。workspace 的初始版本统一为 `0.0.1`，当前只有前端和 LSP，没有可运行的 VM。该版本不承诺稳定 API，也不宣称可以生成机器码。

维护者已确认使用 `Apache-2.0` 许可证，全文见仓库根目录 [LICENSE](../LICENSE)，各包继承 workspace 的许可证元数据，仅允许向 crates.io 发布。crates.io 发布成功后不能覆盖同一版本，因此先做打包检查。

crates.io 没有独立的项目创建或名称预留步骤，首次成功发布才会注册包名。不发布空壳 VM 或同名聚合包占位，已有功能的五个包可以作为首个版本发布。

## 名称检查

2026-11-12 发布前通过 crates.io API 查询：

| 名称 | 查询结果 | 本项目安排 |
|---|---|---|
| `wings-source` | 未注册 | 已有源码位置与诊断实现 |
| `wings-syntax` | 未注册 | 已有词法与容错解析实现 |
| `wings-sema` | 未注册 | 已有名称与局部类型检查实现 |
| `wings-compile` | 未注册 | 已有静态检查命令，不生成机器码 |
| `wings-lsp` | 未注册 | 已有 stdio 语言服务器 |
| `wings-vm` | 未注册 | 实现后再发布，不新建空包 |
| `wings` | 其他项目已注册 | 不发布这个名称 |
| `wings-lang` | 未注册 | 仅作为当前语言名，不发布同名 crate |

未注册只代表查询当时可用，不保证发布时仍可用，也不代表商标或语言名称没有冲突。已有 [`wings`](https://github.com/DouglasDwyer/wings) 是 Rust WASM 插件系统，与本项目无关；对外使用 Wings VM 区分，不发布同名 `wings` 包。命名决定见 [ADR-0008](adr/0008-wings-naming.md)。

## 发布顺序

完成验证并提交源码后，按依赖顺序操作：

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
