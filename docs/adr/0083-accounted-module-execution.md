# ADR-0083：计费模块禁止降级到未计费执行池

- 日期：2026-11-14
- 状态：已采纳（模块执行资源边界修正）
- 后续：[ADR-0084](0084-accounted-root-module-admission.md) 补计费根对未计费工具模块的反向准入检查。
- 部分替代：[ADR-0044](0044-native-module-leases.md) 仅校验已给定账户的 VM 身份，以及 [ADR-0053](0053-detached-process-images.md) 标量镜像可接回未计费池的安排。
- 沿用：[ADR-0080](0080-central-execution-admission.md) 的控制准入与显式池保留、原 4352 字节、ABI v15 和资源/时间上限。

## 问题与决定

`load_accounted` 的模块绑定 VM，但 `validate_pool` 只在池有账户时验证身份。标量模块仍能 `instantiate()`，或在实例/镜像中显式换成未计费池，让后续栈与控制避开已经选择的资源树。保留独立工具入口不应允许计费模块在执行时自行降级。

- 带模块计费租约的初始化、受控/owned task 执行与镜像接回必须使用本 VM 的 process 绑定计费池；未计费池、仅绑定 tenant 的池及跨 VM 池均在入口执行或移动 heap/Bytes/镜像前拒绝。拒绝保持实例可用、旧提交状态、镜像与账户/栈状态不变。
- 池已带账户时先保留原 VM 身份校验，跨 VM 仍返回原错误分类；再要求 process 绑定。不因新增拒绝条件遮住已有跨 VM 诊断。
- `instantiate()` 在创建默认 TLS 工具池前拒绝计费模块，要求调用者显式配置 process 池；不自动创建隐藏账户、扩大资源额度或回退到另一个根。`instantiate_in_pool`、`instantiate_with_heap` 及 worker/Scheduler 继续使用显式树，正常调用沿原核心控制准入。
- 无模块计费租约的 `load_trusted` 及独立可信工具 fixture 保持旧入口行为；它们仍不构成完整 VM 账本。计费模块的标量显式池选择仍可在本 VM 的不同 process 池间选择；带堆实例与镜像继续要求原 process 账户，不将本次模块边界修正扩为新的标量所有权模型。
- 模块逻辑预算与生命周期不变。工具测试改用真实显式池后须核对栈/控制新增的实际预留，不将旧仅模块字节断言保留为完整执行成本。

## 验证

先用可信 Rust ABI fixture 复现默认计费初始化未拒绝，再验证入口调用计数为零、未计费/未绑定池/跨 VM 拒绝、受控与 owned task 拒绝、镜像失败后可接回及后续状态正常。真实 AOT 的 `module_accounting` 覆盖相同路径，并在 process/tenant/VM 各自满额时验证不能改用未计费池绕过；清理保持原模块/栈/控制与根基线。

运行 workspace check、debug/release all-targets/doc-tests、完整受限 AOT、显式 BEAM 正确性、clippy、fmt、MSRV、依赖/unwind 和可用 Linux 功能回归。保持原所有预算/容量/时限；完整业务值、宿主适配层、所有资源账本、公平性、原生双平台及 BEAM 吞吐/整机内存门禁仍未完成。
