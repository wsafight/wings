# ADR-0048：受限 loopback TCP Counter 服务

- 日期：2026-11-13
- 状态：已采纳（真实 I/O 验证切片）
- 前置：[ADR-0047](0047-cross-thread-ingress-requests.md)
- 后续：[ADR-0082](0082-bounded-tcp-segments.md) 修正仅配置时检查缓冲的假设，限制 macOS 连接 MSS 并在每次读写后核对实际缓冲，补充固定诊断计数。

## 决定与理由

以真实 socket 验证外部协议、工作线程和受管 actor 的请求/回复链路，不把发送端原语当作服务已经可用。先交付可单独运行的 `tcp_counter` 宿主示例和源码 `examples/tcp-counter.wings`，不是稳定公共网络 API 或生产网络 VM。

- 仅绑定 IPv4 loopback 的临时端口，不允许外部地址。一个适配线程顺序处理至多 1..=8 个连接，每连接至多 1..=64 帧；最多一个接受的 socket 和一个已发未完成请求，不创建每连接线程、无界任务或缓冲。连接积压由固定 listen backlog 限制，不能当作可读业务队列。
- listener/适配控制及积压连接的 512 KiB 逻辑预算、256 KiB 线程栈及页余量在创建前向 tenant/VM 预留；每次 accept 前预留 256 KiB 连接/内核缓冲逻辑预算，失败或连接结束归还。listener 和接受的 socket 均显式设置 16 KiB 收发缓冲，以 getsockopt 验证每项不超过 64 KiB（允许平台取整/加倍），不满足则拒绝；仅使用固定 32 B 用户态收发缓冲。backlog 不是精确的内核队列硬限额，所预留字节不是 kernel 实际物理内存，也不覆盖 allocator/线程实现的全部内部开销，保持 M1 尚未完成的边界。
- 接入 [BEAM v1](../beam-workloads.md) 的 32 B big-endian 逻辑请求：sequence 非零、operation=1/2、value=i64、reserved=0。operation=1 执行 checked add，2 在更新候选后失败。成功回复 operation=3、value 为已提交计数器；错误回复 operation=4、value 的 1/2/3/4 表示 TimedOut/Cancelled/Failed/Rejected，reserved=0。错误扩展仅属于此服务协议，不倒写成 BEAM oracle 已支持的协议。
- 解析完帧后才调用固定 ingress `Apply(delta: Int, fail: Bool) -> Int`。50 ms 请求 deadline，发布成功后不重试；超时不撤回消息或已生效副作用，远端超时后状态可能继续更新。完成后再回复，断连/写失败不回滚状态。handler 失败退出，后续请求明确拒绝，示例不隐式重启。
- accept 阶段最多 2 秒，单帧读取与写出分别使用 100 ms 绝对截止时间，部分读取不能刷新 deadline。EOF、非法帧和 I/O 错误关闭当前连接，进入下一个有界连接；不让慢客户端无限占住线程。accept 使用非阻塞 listener 加有限 poll，业务 worker 不读 socket、不等待外部结果；原线程按预算 tick/OS wait，actor 不跨线程。
- 正常结束或错误都设置停止标志、join 适配线程，再清理 actor/模块；异常路径也不遗留 detached 线程。读/写、结果等待和 accept 的时限限制 Drop 等待，不承诺硬实时。报告连接/请求/拒绝/错误计数及清理后账户，用于正确性审计，不输出性能排名。

## 验证与边界

独立受限 OS 子进程验证真实 TCP 正常/分片/流水线请求、成功 FIFO、checked overflow、候选失败不提交、失败后拒绝、非法字段、短帧/EOF、读超时、连接与帧上限，以及线程/FD/消息/请求最终清理。测试绑定临时 loopback 端口，不扫描端口、不访问外部网络，不注入宿主 OOM/崩溃。

该示例逻辑 state 仍只是 Int，尚不等价于 BEAM v1 的完整 256 B/4 KiB/64 KiB 状态或批量 32 并发窗口。单个顺序适配线程不是高并发 reactor；不能用本示例称 M:N、完整外部 I/O 或同业务性能验收通过。通用业务值、双平台最新源码及同负载吞吐/整机内存门禁继续保留。

验证命令沿用 workspace check/test/clippy/fmt、Rust 1.85.0 all-targets 检查、debug/release 构建与 `scripts/verify_scalar_aot.py`；示例单测另执行 `cargo test --locked -p wings-vm --example tcp_counter` 及其 `--release` 版本，双平台 CI 已加入这些检查。2026-11-13 在 macOS AArch64 全部通过，TCP 专项回归另在 debug/release 各重复三次；Linux x86-64 尚未验证最新切片，不宣称双平台门禁完成。
