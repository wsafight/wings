# ADR-0135：计费模块路径暂存边界

- 日期：2026-11-18
- 状态：已采纳（资源账本修正；验证结果另记）
- 延续：[ADR-0044](0044-native-module-leases.md)、[ADR-0128](0128-native-module-image-admission.md)、[ADR-0129](0129-native-module-dependency-boundary.md)

## 问题

真实 SDK 探针在 VM 仅余原 2 MiB 模块控制预算时，向 `load_accounted` 借入一个 3 MiB 不存在路径。控制准入成功，但 macOS 原生路径转换前发生 3145729 B 的单次 Rust 暂存分配；ASCII、非 UTF-8 和含 NUL 输入均复现。源路径由可信宿主已持有，不计入本次新分配，但加载器的额外副本不能因此逃逸。失败后账本退回正确，不代表分配前的容量已经足够。

## 决定

保留原 VM 类型检查与 2 MiB 控制准入的顺序、上限和账本寿命。对计费路径，在调用 `canonicalize` 前先以 `OsStr::as_encoded_bytes` 无分配检查原始路径至多 16 KiB；超限返回固定文本 `artifact directory path exceeds 16 KiB`，不复制/格式化来源。非 UTF-8 路径按原生编码字节计长，不做有损转码。

原生 canonicalize 返回后再次检查解析结果至多 16 KiB，成功才拼接库文件名、读取清单及进入装载器。后一检查防止继续放大解析结果；它不把已发生的 canonicalize/libc 内部存储说成完整分配前计费。工具入口 `load_trusted` 保留原路径行为，源码 ABI v22、4352 B 和文件/映像预算不变。CLI 的原 16 KiB 参数合计约束不放宽，SDK 路径上限独立计数。

## 验证与边界

固定 3 MiB/16 KiB/多一字节/空路径，各自在只余控制预算与 VM 满额下运行；Rust 全局分配器只计新分配/重分配请求的次数、总请求大小和最大单次请求，不声称活字节或 RSS。源与报告在观测区外构造，固定十二个场景都在原资源限制的 OS 子进程执行，不制造 OOM。保留修改前 3 MiB 放大与修复后结果，核对固定错误、原满额先拒绝以及每次退账。

正常产物、模块绑定/映像/依赖拒绝及默认 CLI 继续验证；MSRV 与原生平台/交叉检查分别记录。原生 canonicalize 输出、libc/装载器、allocator 元数据和完整 VM 分配账本仍须独立验收；本切片不称为完整内存隔离、原生双平台、BEAM 双目标或 MVP 通过。

回退会恢复已复现的超大来源副本，不因配额账目最终归零而认定可安全回退。验证状态见[记录](../validation-module-paths-2026-11-18.md)。
