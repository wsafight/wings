# 包发布

[English](../publishing.md) | **简体中文** · [返回概览](../../README.zh-CN.md) · [架构设计](architecture.md) · [协作约定](../../AGENTS.md)

## 发布策略与兼容性声明

- 源码仓库：<https://github.com/wsafight/wings>，统一采用 `Apache-2.0` 许可证（见根目录 [LICENSE](../../LICENSE)），仅向 crates.io 发布。
- **早期阶段不承诺兼容**：在 `0.x` / `0.0.x` 阶段，新版本允许直接包含破坏性的语法、ABI、产物格式与 Rust API 变更，不为旧版本保留兼容垫片。
- crates.io 已存在第三方同名 `wings` 包（WASM 插件库，与本项目无关）；本项目固定发布下述 10 个具名 Crate，不发布 `wings` 或 `wings-lang` 聚合占位包。

## 分支保护与手动发布流程

- 日常 PR 使用 `incremental` CI 检查。切换本流程时，仓库管理员需将分支保护中的旧 native/MSRV/site 必需检查改为 `incremental`；修改 workflow 文件不会自动更新 GitHub 分支保护。
- 发布前针对确切发布提交手动触发 `.github/workflows/release.yml`，要求 `native (macos-15, arm64)`、`native (ubuntu-24.04, x86_64)`、`msrv`（Rust `1.85.0`）与 `site` 全部通过；验证性能声明时另行手动运行 `benchmark.yml`。增量缓存结果不得替代发布证据。
- 包发布仅在维护者明确要求后从已合并 `main` 的干净源码手动执行，不由 PR 自动触发。crates.io 不允许覆盖同版本号，上传前必须逐包执行 `--dry-run` 核对归档内容与构建。

## 拓扑发布顺序

内部依赖同时声明 `path` 与精确版本号，按以下无环拓扑顺序依次执行 `cargo publish -p <crate> --locked --registry crates-io`（本机配置镜像索引时需加 `CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/`）：

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

## 历史发布记录

- **`0.0.3`（2026-11-22）**：全量升级发布全部 10 个包，涵盖现代 `wings-lang` v2 语法、精确 CFG 引用根与有界递归、O(1) `slot + generation` 寻址、无锁快速邮箱、M:N 抢占式 `Scheduler`、`send_after` / `monitor`、多 Actor `build-app` 与 `wings-vm serve`（`stdio` / `--tcp` / `--http`）。安装命令：
  `cargo install wings-compile wings-vm wings-lsp --version 0.0.3 --locked`
- **`0.0.2`（2026-11-12）**：首次全量发布上述全部 10 个包（对应提交 `58dc907`，[GitHub Release v0.0.2](https://github.com/wsafight/wings/releases/tag/v0.0.2)），覆盖早期前端、`wings-lsp` 与单 Actor 标量 AOT 原型。
- **`0.0.1`（2026-11-12）**：首次发布前端与语言服务器 5 个包（`wings-source`、`wings-syntax`、`wings-sema`、`wings-compile`、`wings-lsp`，对应提交 `247cf72`，[GitHub Release v0.0.1](https://github.com/wsafight/wings/releases/tag/v0.0.1)）。
