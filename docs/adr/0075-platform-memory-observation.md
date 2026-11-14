# ADR-0075：长测的平台分口径内存观测

- 日期：2026-11-14
- 状态：已采纳（测量入口补全，不是内存门禁通过）
- 扩展：[ADR-0069](0069-streamed-open-loop-measurement.md)

## 决定

v6 长测原来只按 `ps` 对进程树加驱动求和 RSS，无法提供 Linux PSS 或 macOS physical footprint。增加无第三方依赖的 C11 原生只读采样器 `measure_memory.c`，由 Python 在实验冻结目录编译，源文件、编译命令和 binary SHA-256 一并冻结；不依赖解释器手写 OS 结构布局，不把 RSS 改名为物理内存。默认完整 AOT 只执行有界模型/真实系统调用验证，不自动长测。

- Linux 每进程读取 `smaps_rollup` 的 RSS、PSS、private/shared clean/dirty、Swap、SwapPss，各字段保持独立。`stat` 的 starttime 在读取前后核对，PID 复用或读失败不返回零。主机 `meminfo` 只记 SwapTotal/SwapFree，不能把主机 swap 归因给 VM。
- macOS 使用 `proc_pid_rusage(RUSAGE_INFO_V0)` 的 resident_size、phys_footprint 和 start/exit 标识；使用 SDK 原生结构，不推算物理占用。host_statistics64/sysctl 另记主机 compressor 物理页、压缩前逻辑页、已换出的 compressor 页和 swap 使用。`compressor_swapped_page_bytes` 是 SDK 的 swapped_count 乘页面大小，不是解压后的逻辑字节，也不等于磁盘实际使用的 swap_used_bytes。这些主机值不是每进程压缩/swap，更不能与 footprint 相加或与 Linux PSS 混比。
- 每次先/后有界 census，固定最多 256 个驱动及后代；排除当次 `ps` 和 probe，记录原始 parent、PID/start_id、各自时间窗。C 单次最多 256 PID、Python 每个 helper 最多 2 秒，输出分别有界。采样器另报自己的 user/system CPU 与 max RSS，保留调用 wall time；不声称已测全部观察器开销。
- 两次 census 拓扑变化、某进程消失/无权限/复用或主机读取错误，标记样本不完整，各平台总数为 null，不静默丢成员或把部分和当完整总数。同 PID 跨样本身份变化保留并标不完整。raw JSONL 保存完整原始记录，不用不完整样本更新平台 peak；报告 complete/total、host errors、identity changes。
- `ps` RSS 仍用于原 1 GiB 观测安全停止，保留 census helper RSS，并保守补入其与 probe peak 的较大值，不因平台总数排除 helper 而放宽安全上限；它不是同时刻精确峰值或 OS 硬内存配额。平台原生字段各自求和并列报告 sampled peak，非原子、非真实峰值。没有证据覆盖所有时刻/短命后代，不能把 census 稳定当作没有竞态的证明。身份历史固定最多 4096 PID，超限失败，不无限保留。编译/监测辅助成本与整机共享库/内核成本仍须后续验收。

原业务/路由/容量、30 秒预热、60 秒采样、五次配对重复、最多八点扫描、100ms deadline、迟到保留和所有 CPU/墙钟/磁盘上限不变。每秒沿原采样点启动一个批量 helper，不逐 PID 启动进程；只读自己实验树，不扫描无关进程的 smaps，不改系统配置或请求 task port 权限。

## 验证与替代方案

模型覆盖树的后代闭包/观察器排除/容量拒绝、缺失成员/拓扑变化的 null 总数、重复/非法 PID 和 JSON、缺失/越界/类型错误字段、主机错误与各平台口径。真实 OS 验证 helper 编译、缺失 PID 不记零、父/子/孙纳入、原生字段及 observer 成本。`verify_measurement` 接入，独立 Linux 客体用 GCC 原生构建 C helper 并执行，不只交叉编译。

调用 `footprint`/`vmmap` 获取更细的 task 内存可能需要权限且成本更高，不作为每秒默认采样；手写 ctypes 版本结构易与 SDK 漂移，不采用。若将来需要 per-process compressed/swap、整体物理页去重、内核成本或更高频采样，重新定义口径并测试观察器偏差，不能把本片 host 数据充作该证据。完整内存验收、CPU 硬配额、长时公平性和原生双平台 BEAM 双门禁仍未完成。
