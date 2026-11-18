# TCP 与让出入口启动复验（2026-11-18）

基于 `a7812ec`，继续调查[原子进展验证](validation-atomic-progress-2026-11-18.md)中 Debug 完整 AOT 的 TCP 启动失败。只有诊断和结果记录，没有修改 VM、编译器、测试门槛、macOS 安全设置或文件扩展属性，也没有停止用户应用。macOS 26.6.2 AArch64、M3 Pro、36 GiB、Rust 1.99.1；没有原生 Linux。

## 原失败与复验顺序

原 Debug 完整 AOT 在 TCP normal 场景的 5 秒握手失败，elapsed=5.012s、清理前活跃、清理 exit=-9、双流前缀均空。Release 完整 AOT 顺序通过。原日志分别保留为 `/tmp/wings-atomic-progress-full-aot-{debug,release}-final-20261118.log`。

随后冻结 Debug/Release TCP 可执行副本，新编译同一份 `examples/tcp-counter.wings`，顺序运行 Debug/Release/Debug/Release 各一轮原 `verify_tcp_service` 全部场景。每轮仍九个真实服务启动，覆盖正常四请求、8×64 帧上限、除零/溢出、无效帧、半帧/绝对读期限、帧上限、断连和零连接，并保留四组无效参数拒绝。原请求/适配线程资源、CPU 20s、启动握手 5s、连接/结果期限不变，不重试单个失败场景。

四轮均通过，共 36 次握手。每轮最大握手时间分别为 1.345663/0.431266/0.003141/0.005928 秒，整轮 3.785809/2.919922/2.416899/2.466190 秒。这些包含宿主和原生启动，且共享主机/文件缓存，不能用来计算 VM 吞吐、对照构建性能或断言热缓存解决问题。

接着原限 Debug 完整 AOT 复验越过 TCP，但在后续 `observed_yield` 的独立十秒入口超时。其 CPU 5s、内部三秒业务期限未改；elapsed=10.001109s、清理前仍活、清理 exit=-9、输入 0 字节、双流观测 0 字节。原 `verify_multi_counter` 的有限失败 JSON 完整保留在 `/tmp/wings-atomic-progress-full-aot-debug-repeat-20261118.log`。它不是本轮 Debug 完整套件通过，也没有因 TCP 专项成功抹去首次失败。

再冻结 Debug/Release `observed_yield` 副本，新编译 `examples/beam-fairness.wings`，原 CPU 5s/墙钟 10s/core 0 下各执行一次。两者 exit 0，进程等待时间 1.522922/0.614344 秒；返回原本的两条本地/跨线程请求结果、state 与清理核对且 `fairness_gate_proven=false`。这是定向复验，不覆盖完整 fairness 或此前十秒失败。

## 原生采样

TCP 与 `observed_yield` 诊断均仅在启动后一秒尚未完成握手/退出时，对自己的测试子进程启动 `sample PID 1 10 -mayDie`；不改变业务截止时间，业务仍由原入口收割。采样工具单独 CPU 5s/墙钟 3s/core 0/文件 8 MiB，在结束后 join；进程已完成时不启动采样。采样会有扰动，不自动作为默认回归的一部分。

只有各自首个 Debug 进程触发采样，两次 `sample` exit 0。TCP 的主线程 19 个样本中，16 个在 `ScalarModule::load_accounted → dlopen → mapSegments → fcntl/__fcntl`，其后同一进程完成握手和原业务；`observed_yield` 的 36 个主线程样本中，34 个在模块 `dlopen`，其中 33 个走文件映射的原生加载路径，其后同一进程正常退出。它们说明被采样的这两次短暂等待发生在模块加载附近，不是无限 handler 或 mailbox 不进展的现场证据。

此前[空 harness/Counter 采样](validation-atomic-progress-2026-11-18.md#启动采样旁证)主要停在 `_dyld_start`，本次则已进入 main 后装载模块；不能合并为同一原因，更不能把 `__fcntl` 直接归因为安全扫描、磁盘或某个系统服务。真正五秒/十秒超时的原进程已经按契约清理，没有原现场栈。新产物和副本路径与失败时临时目录不同，复制、编译、缓存和取样都有潜在影响。

## 可复核证据

- `target/tcp-startup-live-20261118/report.json`：四轮/逐握手状态与时钟、冻结二进制、产物、原生采样和九个文件 SHA；stdout 摘要另保留 `/tmp/wings-tcp-startup-live-20261118.log`。
- `target/observed-yield-live-20261118/report.json`：两种构建原限执行、完整 stdout/stderr、可执行副本、原生产物、Debug 采样及 SHA；控制日志 `/tmp/wings-observed-yield-live-20261118.log`。
- 全部指纹与上述明确数量/退出状态已独立核对，最终证据清单为 `target/startup-followup-evidence-20261118/report.json`。TCP 副本与原 `target/{debug,release}/examples/tcp_counter` 的 SHA 一致；不将不同构建的不同二进制混为同一个。

诊断脚本为本轮工具调用，没有新增自动重试或默认采样器。可复现业务入口仍是 `scripts/verify_scalar_aot.py` 的 TCP 场景、`scripts/verify_fairness.py` 的原观察入口；新增受控采样须保留原失败与全部限制。

## 结论与下一步

启动失败仍未定位、未修复。本轮避免把无输出等同于受管调度死锁，但不能排除未采样到的 runtime 问题。后续需要在不延长原截止时间的条件下捕获真正超时进程的栈和阶段；不能以持续重跑直至全绿作为修复。最新原生双平台、完整账本、公平/服务 I/O 长测和同机同业务 BEAM 吞吐/整机内存双门禁仍未完成，MVP 未完成。
