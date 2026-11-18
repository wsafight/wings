# ADR-0140：完整 Bytes 状态的受限 TCP 同业务对照

- 日期：2026-11-19
- 状态：已采纳（真实服务正确性前置）
- 延续：[ADR-0048](0048-bounded-loopback-service.md)、[ADR-0059](0059-byte-state-counter-equivalence.md)

## 决定

沿用现有 tcp_counter 的单线程 loopback 适配器和固定 32 B 帧，新增显式 `--state-bytes 256|4096|65536` 模式与独立 tcp-bytes-counter.wings。旧 Int 模式、8 连接/64 帧、mailbox 1、ingress 2、worker request 4、50ms 请求、100ms 绝对读写、2s accept、5s 握手及原 32 MiB 资源限额均不变。新模式使用计费堆和不可变 Bytes 保存完整状态：前八字节 signed i64 big-endian 计数，随后八字节 identity=1，其余为零；每次更新构造完整候选，checked overflow 或候选后失败不提交。

配置和服务后的健康完整状态读取均通过原有限请求执行，三秒内部期限、4096 总预算、64 步片。成功读取的 Bytes 回复逐字节校验长度/计数/身份/零填充，最后释放回复和 actor 后核对启动基线。失败退出仅有现有 32 B 已提交前缀快照，明确报告 `full_state_verified=false`，不把前缀冒充完整状态导出。

显式 `--compare-beam` 启动 OTP 29 的同协议真实 TCP 服务：一个有界 adapter 进程、一个保留完整不可变 binary 的 Counter，沿用有限发送前 gate，checked i64/候选失败语义和逐条错误回复。两端分别接受同一 Python 驱动生成的分片/流水线、跨连接、上/下溢出、候选失败及非法字段输入，逐条对照独立模型并核对实际提交状态；健康状态完整检查，失败前缀检查。默认完整 AOT 不自动执行 OTP；无 OTP 的显式比较必须失败，不能回退到模型。

## 边界与验收

这是短时正确性协议，不是长测/开环/公平性、通用 reactor 或性能门禁。不从固定发送序列推断跨客户端全序，也不把单发送者串行 ingress 当并发吞吐。两端数据逻辑等价不意味着 Rust/Erlang 信封、socket 缓冲、scheduler 或内存物理成本相同；Wings 原 socket/MSS 检查保持，BEAM 报告实际 socket 设置。只接受观测到零超时的正确性比较，不凭此宣称超时/取消或过载协议完全等价。

子进程继续原 CPU/墙钟/core 限制，保留失败双流和运行输入。完整 workspace/AOT、MSRV、Linux 交叉及原 TCP 回归继续执行；没有原生 Linux 或同机 CPU 条件/长测/完整内存口径时，双平台和 BEAM 吞吐/整机内存及 MVP 门禁仍未完成。
