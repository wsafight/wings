# ADR-0081：CLI 默认标量路径使用计费执行入口

- 日期：2026-11-14
- 状态：已采纳（CLI 资源集成切片）
- 后续：[ADR-0086](0086-accounted-cli-json.md) 将请求行缓冲、JSON 暂存和取消控制接入原资源树，以固定 visitor 代替通用 Value。
- 部分替代：[ADR-0044](0044-native-module-leases.md)、[ADR-0079](0079-accounted-vm-bootstrap.md) 中标量 CLI 默认仍走未计费工具入口的安排。
- 沿用：[ADR-0080](0080-central-execution-admission.md) 的执行控制准入、显式池保留，以及堆 CLI 原 16 MiB 三级字节上限。

## 问题与决定

`wings-vm run --trusted` 无 `--heap-records` 时仍调用 `ScalarProcess::load_trusted`，绕过计费根、模块加载预算、子账户与显式栈池。CLI 是否计费不应由业务是否分配堆对象决定。

- CLI 在两种模式下使用同一条 VM → tenant → process 计费路径，原堆模式的 16 MiB 字节上限、单进程和零消息/请求额度不变，并扩至默认标量模式。模块使用 `load_accounted`，在清单解析和原生装载前检查原文件页数加 2 MiB 的保守逻辑预算；默认模式使用 `instantiate_in_pool`，堆模式继续使用 `instantiate_with_heap`。
- 初始化及后续调用保留单槽、零缓存的同 process 计费池，通过执行核心预留控制容量。任一级额度不足或初始化/调用失败沿现有错误通道退出，正常返回才提交状态与输出；不改变 JSON 协议、总步数、强制检查点或语言类型拼写。
- 16 MiB 是 CLI 当前固定逻辑额度，不是 RSS 上限、OS 沙箱或完整 VM 配置接口。JSON/标准 I/O 适配层、宿主控制对象与 allocator/动态装载器内部开销仍未完整计费；独立库 `load_trusted`、默认未计费工具池和基准模型保留原边界，不能据此宣布所有分配闭环。

## 验证

先用受限 CLI 子进程复现：16 MiB 的稀疏伪库配坏清单，原默认路径先解析清单，新默认路径与堆模式均先报告 VM 资源不足且无业务输出。该文件小于原 64 MiB 库上限，加载预算大于原 CLI 16 MiB 额度；不执行伪原生代码、不制造宿主内存耗尽，也不放宽额度以通过测试。

完整原限 AOT 同时运行默认标量与堆模式，覆盖正常状态/输出、预算/初始化/算术失败、坏清单/库/符号与 CLI 输入拒绝；现有计费根、模块租约、直接执行及退出清理回归保持。执行 workspace check、debug/release all-targets/doc-tests、clippy、fmt、MSRV、依赖/unwind 守卫和可用 Linux 功能验证。原生双平台、公平性、完整业务值/账本及 BEAM 吞吐/整机内存门禁仍独立验收。
