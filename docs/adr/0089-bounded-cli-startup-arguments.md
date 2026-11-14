# ADR-0089：CLI 启动参数的有界读取与准入

- 日期：2026-11-14
- 状态：已采纳（CLI 启动资源切片）
- 部分替代：[ADR-0088](0088-owned-cli-stdio.md) 尚未覆盖的启动参数分配。
- 沿用：原命令格式、选项数值范围、16 MiB 三级限额、业务 I/O、ABI v15 与运行预算。

## 决定与理由

原 main 使用 `std::env::args().skip(1).collect()`。当前 Unix 标准库在产生迭代器前已将全部 argv 复制为 Vec/OsString，再由 CLI 收集为 String；改为 args_os 或在 collect 前 take 仍不能避免最初复制。非 UTF-8 参数还会在 String 转换时 panic，早于正常 CLI 错误处理。

- 正常 Rust main/运行时保持；CLI 在模块装载和启动任何 worker 前，借用内核/加载器提供的原 argv，不调用会复制全部参数的标准库参数迭代器。macOS 使用 libc 的 `_NSGetArgc/_NSGetArgv`；当前已验证的 Linux glibc 路径用 init-array 回调只保存 argc/argv，回调不分配、不解析、不执行用户代码。不得跨过 std 运行时初始化、自定义 main 或静默回退到未计费参数迭代器。
- 先建立原计费 VM/tenant/process，再为启动参数预留 20 KiB，之后才读取参数内容或复制路径。16 KiB 为除 argv[0] 外的总参数字节上限，包含各自 NUL；4 KiB 为固定视图、OsString、控制和诊断余量。argv[0] 不解析或复制；最多接受原来的七个参数，超数量先拒绝，不读取尾部字符串。所有容量从原 16 MiB 支付，不另设工具根。
- 原生视图只读，逐字节扫描在总上限内结束，固定数组保存至多七个借用切片。只为已通过命令和选项检查的目录创建 OsString，不构建参数 String/Vec 列表；路径按 Unix 字节保留，选项和数字按原 ASCII 拼写验证。非 UTF-8 选项返回用法/数值错误，非 UTF-8 路径交文件系统处理，不 panic。
- 帮助仍只有单独的 `--help/-h`，原 run/trusted、两个可交换选项、重复拒绝、步数 u64 和堆容量 1..=1024 保留。超过总字节限制返回固定诊断与退出码 2，不包含原参数。解析失败不装载模块、不创建业务 I/O、不执行 initializer。
- 参数对象在整个 run 期间持有预留；目录先销毁，预留后归还，避免模块路径借用结束前释放账目。帮助和错误也按明确作用域清理。平台入口仅在 CLI 使用，不向 VM 库或语言前端扩散；不增加依赖或改变语言拼写。

原 argv、环境、Rust 启动和主线程栈属于 OS/可信启动边界；本切片限制并计费 CLI 新增的用户态参数副本，不伪称这些已有存储消失。20 KiB 是保守逻辑容量，不是 RSS。帮助/stderr、allocator、动态装载器及全部宿主账本仍须分别收敛。Linux 非 glibc 不是当前已验证目标，不提供隐式不同的复制路径。

## 验证

先保存非 UTF-8 参数的真实 CLI panic。检查 process 层级、三级零/少一字节/精确准入、拒绝前不访问参数、固定存储上界、原正常/重复/非法选项、数字边界、原生数量与 NUL/字节边界、非 UTF-8 目录/选项、预留覆盖运行与销毁回滚。真实默认/堆 CLI 检查非 UTF-8 参数正常失败、超过参数数目及 16 KiB 上限、边界内参数到达正常路径，并继续完整 JSON、I/O 与业务回归。

运行 workspace check、all-target clippy、fmt、Rust 1.85.0 all-target check、debug/release all-targets/doc-tests、完整受限 AOT、显式 BEAM 正确性、依赖/unwind/文档检查与可用 Linux 功能回归。原生双平台 CI、完整业务值/账本及 BEAM 吞吐/整机内存门禁保持。
