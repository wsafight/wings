# 清单读取边界验证（2026-11-18）

按 [ADR-0139](adr/0139-regular-file-manifest-loading.md) 修复共享 Manifest::read 的特殊文件打开边界，基于 `10f6794`。ABI v22、4352 B 执行控制、64 KiB 清单/64 MiB 库上限、2 MiB 模块控制、原有配额和 CPU/墙钟限制不变。

## 修改前证据

冻结原 Debug VM，使用 ABI v22 的真实 Counter 产物，分别把清单替换为普通文件、FIFO、目录及普通目标符号链接。子进程 CPU 2 秒、墙钟 3 秒、core 0、文件 1 MiB；FIFO 无写端，不在开发进程中打开。第一轮普通/符号链接已越过加载，但使用了错误的 CLI 字段 `arguments`，以参数错误退出，不能当成功执行证据；随后改用原协议 `args`，普通/符号链接均得到 `{"reply":7}`，退出 0。

两轮 FIFO 都在原三秒期限无双流输出、清理 exit -9；第二轮对确认仍存活的原子进程做一次受限原生 sample，86 个样本均落在 Manifest::read → File::open → open。sample 自身退出 0；这是清单 FIFO 的直接阻塞证据，不能归因原有 TCP/dlopen 启动超时。目录原先以 OS read 错误退出。

`target/manifest-boundary-before-20261118/` 保留原可执行、副本 SHA、输入库/清单 SHA、每轮双流、初始 `report.json`、更正请求的 `confirmed-report.json` 和 `fifo-open.sample.txt`。原 VM SHA256 为 `b6608c5f2dd6b46942a2e06a4c6b8d547e4abb0260bb7e187a4dfe8c169791fb`，未覆盖首轮请求错误。

## 实现与回归范围

共享 artifact 入口先检查解析目标的普通文件/64 KiB 长度，再打开并复查句柄；按句柄长度 fallible 精确预留、read_exact 及一字节 EOF 探测，保留 JSON 预检/语义校验。普通文件符号链接仍接受；目录/FIFO/socket 及特殊符号目标失败。路径在两次检查间不变是前提，不承诺恶意换文件防护或文件系统等待上界。

新增四项 Rust 测试覆盖目录/socket、符号目标、0/1/65536 字节精确读取、增长/截断/读错误；原精确清单边界和语义测试保留。新增真实 AOT fixture 核对 11 种文件/目标：普通、精确 64 KiB、超长、空、目录、FIFO、socket、普通/FIFO/目录/悬空符号链接。每种先验证满额账户优先拒绝，再在同 VM 两轮校验计费及工具入口；有效输入实际执行 Inc/Get，失败/释放后恢复根 bootstrap。CLI 对相同 11 种输入独立执行或拒绝。该回归接入原 module image 验证，因此由既有完整 AOT/双平台 CI 入口执行；本轮没有推送或调用远端 CI。

## 验证状态

本机 macOS AArch64。初次 artifact 定向测试和 Debug 11 场景通过，报告 `target/manifest-loading-debug-20261118/report.json`。该轮 Python 本身成功，外层 zsh 试图赋值只读变量 status 后退出 1；保留 `/tmp/wings-manifest-focused-aot-debug-20261118.log`，不把外层错误伪装成业务失败或删除。

最终 Debug/Release workspace 各 107 harness/717 项通过，见 `target/manifest-workspace-{debug,release}-20261118/report.json`；比基线增加四项测试及一个真实 AOT fixture 的空测试入口。两构建独立 module image/manifest 专项通过，冻结可执行和逐输入 CLI 双流/指纹见 `target/manifest-module-{debug,release}-20261118/`，其中 `cases/manifest-report.json` 各保留 11 类输入、12 次独立命令和根账本回到 232 B 的实际结果；原映像/依赖/符号与模块租约回归也通过。专项没有代替完整 AOT。

workspace check、all-target clippy `-D warnings`、fmt、MSRV 1.85 all-target、Linux x86-64 all-target 交叉检查、两构建 doctest、八项 workspace/六项 AOT 运行器及八项原子进展模型自检通过。逐项命令/退出/日志指纹见 `target/manifest-validation-20261118/report.json`，228 项执行输入指纹见同目录 `inputs.json`；Linux 仍不是原生运行。234 个文档/Python 文件的相对链接/围栏/AST 与依赖隔离、禁 unwind 守卫通过，见 `target/manifest-static-20261118/`，未读取用户 `docs/proposals/`。

Debug 完整 AOT 已越过新增清单/模块专项，在后续 TCP normal 原五秒握手期限再次失败：elapsed=5.007s、清理前活跃、exit=-9、双流前缀为空，完整日志 `/tmp/wings-manifest-full-aot-debug-20261118.log`。没有为本切片重跑至全绿；清单 FIFO 修复不是这条原有失败的根因解释。逐构建完整 AOT 结果与日志 SHA 保留于 `target/manifest-validation-20261118/full-aot-reports.json`。

随后顺序 Release 完整 AOT 通过，日志 `/tmp/wings-manifest-full-aot-release-20261118.log`；不覆盖 Debug 失败。两种构建的专项、workspace 和完整 AOT 均在输入源码不再变化后执行，228 项输入指纹完成后再次逐一核对，无变化。没有放宽任何原有限额/时限，没有并行构建干扰这些 AOT 运行。

## 复现入口

```sh
cargo build --locked --workspace --bins --examples
cargo build --release --locked --workspace --bins --examples
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_module_image.py
python3 scripts/verify_workspace.py --output target/manifest-workspace-debug-new
python3 scripts/verify_workspace.py --profile release --output target/manifest-workspace-release-new
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

完整分配/原生账本、既有启动超时根因、公平/过载/服务长测、原生 Linux/最新双平台及同机同业务 BEAM 吞吐/整机内存双门禁仍待完成，MVP 未完成。
