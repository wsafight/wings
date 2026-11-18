# 2026-11-18 模块路径暂存验证

对应 [ADR-0135](adr/0135-bounded-accounted-module-paths.md)。本机 macOS 26.6.2 AArch64、Rust 1.99.1；不改原 2 MiB 模块控制、文件/映像预算、ABI v22 或 4352 B。

## 修改前证据

加载实现来自 `74678b9`，探针单独添加，不改 VM。仅余 2097152 B 控制额度与 VM 全满两种条件，各执行 ASCII/非 UTF-8/NUL 三种固定 3 MiB 输入、16 KiB 多一字节、恰好 16 KiB 以及空路径，共十二场景。源存储和报告均在计数区外创建；原 CPU 20s/墙钟 30s/core 0 的独立子进程运行，未注入 OOM。

| 路径/余控制额度 | Rust 分配次数 | 总请求 B | 最大单次 B |
|---|---:|---:|---:|
| ASCII 3 MiB/2 MiB | 4 | 3145801 | 3145729 |
| 非 UTF-8 3 MiB/2 MiB | 4 | 3145801 | 3145729 |
| NUL 3 MiB/2 MiB | 2 | 3145771 | 3145729 |
| 16385 B/2 MiB | 4 | 16458 | 16386 |
| 16384 B/2 MiB | 4 | 16457 | 16385 |
| 空/2 MiB | 3 | 100 | 50 |
| 上述任一/全满 | 1 | 60 | 60 |

三种 3 MiB 输入都在仅准入原 2 MiB 控制后产生 3145729 B Rust 暂存副本，退账正确也不能抵消这项分配前缺口。统计是成功分配/重分配请求大小，不含 libc 分配、allocator 元数据，不是总活字节或 RSS。

原证据在 `target/module-path-baseline-20261118/`，保存原 stdout/stderr、探针源码、修改前模块源码、二进制指纹和来源提交；原二进制后来被同名构建覆盖，未保留可执行副本，报告明确记录此限制。完整加载源码仍在前一 CPU 长测冻结副本中，探针 SHA 与原运行回执一致；日志 `/tmp/wings-module-path-baseline-20261118.log`。未提高额度以使旧路径通过。

## 修复与验证范围

计费路径在原控制准入后、canonicalize 之前无分配检查来源至多 16 KiB，解析结果再检查后才拼接后续路径。超限返回固定文本，VM 全满仍先返回原资源拒绝；可信工具加载行为保持。十二场景回归加入完整 AOT。

Debug/Release 原生探针各十二场景通过。ASCII/非 UTF-8/NUL 3 MiB 与 16385 B 来源，在只余原控制额度时均只有一次 38 B 固定错误分配；原 VM 全满仍只有一次 60 B 原资源拒绝，精确 16384 B 与空来源仍保留原生路径错误及修改前相同的分配请求数/大小。所有场景退账回原基线。Debug/Release 各 105 harness、702 项测试在原受限 OS 子进程通过；新增探针是无 libtest 项目的独立可执行文件，不将其十二场景重复计为 Rust 测试项。

修复后目录为 `target/module-path-fixed-{debug,release}-20261118/`，保存本次可执行副本、原 stdout/stderr 和文件指纹；全量测试清单在 `target/module-path-workspace-{debug,release}-20261118/`。定向日志 `/tmp/wings-module-path-fixed-20261118.log`。

顺序使用预构建示例的 Debug/Release 完整 AOT 均通过，日志为 `/tmp/wings-module-path-full-aot-{debug,release}-20261118.log`；原启动超时仍保留，不因本次成功而关闭。`cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、MSRV 1.85 all-targets 和 Linux x86-64 all-targets 交叉检查通过，对应 `/tmp/wings-module-path-{check,clippy,fmt,msrv,linux-cross}-20261118.log`。Linux 检查不执行产物，不是原生平台通过。

```sh
cargo build --release --locked -p wings-vm --example module_path_accounting
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 -c 'import sys; sys.path.insert(0, "scripts"); import verify_scalar_aot as aot; print(aot.example("module_path_accounting").stdout, end="")'
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

上述验证入口为实际 probe/示例设置原 OS 子进程上限。原生 canonicalize 内部与解析结果分配前容量、libc/装载器/allocator 元数据、完整账本、启动根因、原生双平台和 BEAM 双目标仍未完成，MVP 未完成。
