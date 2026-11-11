# ADR-0005：首要平台与 BEAM 对照

- 日期：2026-11-12
- 状态：已采纳（设计）

## 背景

Wings 的目标是在相同业务负载下比 BEAM 更快、更省内存。macOS 既是开发环境，也是实际运行目标，不能等到 Linux 实现完成后才验证 Darwin ABI 和装载方式。

## 决定

- macOS AArch64（Apple Silicon）与 Linux x86-64 并列首要运行目标，M0 起分别验证完整编译、链接、装载、切换恢复和 I/O 唤醒。
- 语言、IR 和资源契约共用；Darwin ABI/Mach-O、System V ABI/ELF、平台切换与 I/O 分别实现和验收。Linux AArch64、macOS x86-64 留待后续扩展。
- BEAM 为性能和内存主基线，kameo/Tokio、Wasmtime 为辅助对照。相同代表性负载下，持续吞吐高于 BEAM、整机 RSS 低于 BEAM，并满足相同尾延迟要求，才算核心目标达成。
- 各平台同机、同系统比较，保留 BEAM 正常优化；披露真实页大小、CPU/内存限制及 macOS 热状态、压缩内存等影响。
- RSS、Linux PSS 和 macOS physical footprint 分别报告，不混为一个统计口径，也不拼接不同平台的最优值。

## 替代方案

- Linux 优先，macOS 只做前端工具开发：不满足首要运行目标，不采用。
- 用单项微基准或跨机器结果宣称领先：不能证明同负载下的性能与内存收益，不作为验收方式。

## 影响

双平台工作从最初阶段就有 ABI、装载和测量成本。当前没有 VM 实现或基准数据，领先 BEAM 仍是目标；指标与实验配置以评测文档为准。

## 重审条件

支持范围、主基线或代表性负载发生实质变化时，用新 ADR 记录原因，更新实验协议并重跑对照。

## 相关文档

- [架构设计](../architecture.md)
- [评测与验收](../evaluation.md)
- [风险与里程碑](../roadmap.md)
