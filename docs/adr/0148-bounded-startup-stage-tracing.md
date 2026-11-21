# ADR-0148：有界启动阶段与超时栈关联

- 日期：2026-11-20
- 状态：已采纳（默认关闭的诊断入口；启动根因未定）
- 延续：[ADR-0142](0142-opt-in-timeout-stack-sampling.md)、[ADR-0143](0143-shared-invoke-timeout-sampling.md)、[ADR-0144](0144-command-timeout-stack-sampling.md)

## 背景

真实原限超时采样曾定位到 `_dyld_start` 和 `dlopen/mapSegments/fcntl`，但失败后的单次栈快照不能恢复超时前历史。TCP readiness 和多 actor 最终 JSON 之前没有增量业务输出，空 stdout/stderr 因而不能区分进程启动、模块路径/文件读取、映像预检、动态装载和后续执行。继续调整 socket、期限或重试都不能补足这段证据。

## 决定

增加默认关闭的 `WINGS_STARTUP_TRACE_DIR`，只在可信诊断场景使用：

- 调用进程首次进入模块加载时，在调用者预先创建的目录内以 `create_new` 建立 `startup-<pid>-<unix-ns>.log`。每次加载取得进程内单调分配的非零 load ID；每条记录为 `pid load-id relative-monotonic-ns stage`，并发加载不得只靠 PID 合并。相对时钟从诊断初始化开始，不冒充父进程 spawn 或 `_dyld_start` 时刻。
- 模块加载依次记录 `module_load_begin`、`module_path_ready`、`module_metadata_ready`、`module_manifest_ready`、`module_image_begin`、`module_image_ready`、`module_dlopen_begin`、`module_dlopen_ready`、`module_symbols_ready`。映像两项只出现在 accounted 加载；trusted 加载没有该预检。
- 每进程最多保留 64 条普通记录，随后写一条 `startup_trace_truncated` 并关闭诊断句柄；文件至多 8192 B。阶段名是至多 40 B 的固定小写 ASCII/下划线，单条记录使用 128 B 固定栈缓冲，不为每次记录分配 Rust 堆存储。
- 初始化、格式化或写入失败只关闭诊断，不改变模块加载结果，不向业务 stdout/stderr 增加内容。目录不存在时不自动创建。环境变量必须在进程启动前设置；首次初始化后再修改环境不会重开轨迹。
- 默认未设置时不创建文件或记录；仍有每次进入标记函数的 OnceLock 分支和首次环境检查，不宣称绝对零开销。诊断文件、描述符和初始化分配不计入受管 process 账本，启用本入口的运行不能作为未扰动性能测量。

原 5 秒 TCP readiness、10 秒共享入口、30 秒通用命令及业务期限、ABI v22、4352 B 控制预留、资源额度和清理顺序不变。本入口不提前 readiness，也不把迟到输出改判成功。

## 证据解释

阶段轨迹必须与 ADR-0142–0144 的同 PID 原生采样共同使用。最后一条 ready 只表示该 load ID 的此前步骤已完成，不能单独证明当前阻塞点；只有一个 load ID 的完整 `module_dlopen_begin` 记录没有对应 ready、其他加载不存在同样未完成的候选，且同 PID 栈位于 `dlopen` 时，才构成该次装载等待的直接证据。多个未完成加载仍不能仅凭 PID 分清具体线程。部分写失败可能留下不完整末行，初始化/写失败后的事件也会缺失。没有轨迹文件可能来自尚未进入模块加载、诊断初始化失败或文件被清理，不能单独断言进程停在 `_dyld_start`。

本切片是超时历史取证，不是启动根因修复，也不覆盖 Scheduler、listener 或业务内部的完整阶段轨迹。实际验证见[记录](../validation-startup-stage-tracing-2026-11-20.md)；完整受管执行、资源账本、公平/故障长测、原生双平台与 BEAM/MVP 门禁保持开放。
