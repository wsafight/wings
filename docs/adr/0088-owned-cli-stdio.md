# ADR-0088：CLI 业务标准 I/O 的独立预留与确定清理

- 日期：2026-11-14
- 状态：已采纳（CLI I/O 资源切片）
- 部分替代：[ADR-0086](0086-accounted-cli-json.md) 尚未纳入业务生命周期的 stdin/stdout 全局缓冲路径。
- 沿用：原 16 MiB 三级额度、264 KiB 请求适配器、64 KiB 行限制、逐回复 flush、业务步数与 ABI v15。
- 后续：[ADR-0089](0089-bounded-cli-startup-arguments.md) 收敛自有启动参数副本，原 I/O 生命周期与预留保留。
- 后续：[ADR-0090](0090-bounded-cli-reporting.md) 收敛帮助/stderr 的控制输出生命周期，业务 I/O 不变。

## 决定与理由

原 CLI 在 initializer 后调用全局 stdin/stdout 并锁定读写。释放锁不会销毁全局缓冲，process 账户没有对应预留，无法随着业务运行清理。独立标准库探针在当前 macOS 构建上观察到锁释放后仍保留 9344 B allocator 请求；该数字不是完整 CLI 开销或 RSS，也不硬编码为其他平台的成本。

- 在复制描述符、创建 I/O 缓冲和进入 initializer 前，向同一 process 账户预留 12 KiB。8 KiB 用于独立输入缓冲，4 KiB 包含固定 128 B 输出缓冲、读写包装、两个复制描述符及控制余量。成本沿原 tenant/VM 逐级准入，不能借工具根或提高原 16 MiB 限额。
- 业务读写使用标准库 BufReader/BufWriter；复制继承的 stdin/stdout 描述符时设置 close-on-exec，独立持有和关闭副本，不改变原描述符的所有权。任一复制失败即停止启动，销毁已创建的副本并归还预留，不执行 initializer。
- 正常回复仍逐行写入并显式 flush。I/O 对象退出时丢弃未 flush 的输出缓冲，不在 Drop 中隐式重试输出失败；此前完成 flush 的回复不撤回，handler 的已提交状态和副作用也不因输出失败回滚。输入缓冲、输出缓冲和副本均先销毁，再归还 I/O 预留。
- 标量与堆模式共用该路径；不初始化业务 stdin/stdout 的标准库全局缓冲，不自研 I/O 缓冲器或 JSON 编码器，不增加第三方包。

这只收敛业务 stdin/stdout 的用户态生命周期与逻辑预算。启动参数、帮助输出、stderr 诊断、allocator/主线程栈、动态装载器及继承描述符背后的内核队列仍不等于完整宿主账本或 OS 硬配额；不把它写成所有分配闭环。语言语法、消息语义、运行时 ABI 和性能验收门槛不变。

## 验证

保留旧全局锁最小探针，记录释放锁后仍保留的 allocator 请求。新增 process 层级检查、三级零/少一字节/精确准入、准入前不复制描述符、首个/第二个复制失败回滚、固定缓冲复用、flush 成功输出、退出丢弃未 flush 内容及副本关闭/原描述符保留测试。真实 CLI 的默认/堆模式继续覆盖正常回复、最大行、已有成功回复保留与读写错误；不扩大原容量、CPU/墙钟或阶段截止时间。

运行 workspace check、all-target clippy、fmt、Rust 1.85.0 all-target check、debug/release all-targets/doc-tests、完整受限 AOT、显式 BEAM 正确性、依赖/unwind、文档检查和可用 Linux 功能回归。原生双平台 CI、完整资源账本及同负载 BEAM 吞吐/整机内存仍独立验收。
