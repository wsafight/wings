# ADR-0059：固定字节 Counter 的 Wings/BEAM 正确性对照

- 日期：2026-11-13
- 状态：已采纳（同协议正确性集成切片）
- 扩展：[ADR-0057](0057-accounted-immutable-bytes.md)、[ADR-0058](0058-byte-i64-big-endian-access.md)

## 决定与理由

按 [BEAM 协议 v1](../beam-workloads.md) 接入真实 Wings 端，不再以标量 Counter、整数列表或编码原语代替完整字节 state。先验证同业务结果和资源清理，再安排多 actor、开环到达与性能采样，不降低 MVP 双目标。

- Wings 程序保留 256/4096/65536 字节不可变 Bytes：signed i64 big-endian 计数器、u64 big-endian actor ID 1、其余零。宿主在业务窗口前通过计费 Configure 初始化；每个 Apply 检查加法，构造完整候选后才判断注入失败，正常返回才提交和自动回复。溢出优先，不重启或重放。
- 一个单 worker actor，mailbox/request 表最多 32，先发布整批再 tick/收齐，最多 256 请求。四个 Int 参数对应 32 B 逻辑请求，回复按关联表构造相同 32 B 逻辑记录；不声称物理消息与 BEAM binary 信封相同。驱动按 Delivered 事件检查实际顺序，不选择性取后续回复掩盖乱序。
- 为独立核对失败前最后提交值，增加固定 `ByteStateSnapshot`：总长度和最多 32 B 前缀，按值复制，不携带 handle/堆/账户租约。单 worker 仅在无活跃 invocation 时允许宿主读取；失败诊断在清理前复制已提交 Bytes 根，不读取失败候选。不是完整状态导出、持久快照或可靠日志，也不扩展 ABI；多 worker 诊断不在本切片自动改变。
- 独立 Python checked 模型、OTP 29 actor 和 Wings AOT 三端核对结果。外层 JSON 统一严格验证重复字段/类型/大小等；Wings 子进程只读有界内部二进制桥接，全部输入在加载可信产物前验证，不把它写成通用 VM 服务协议。
- 桥接头为四个 big-endian 64 位字：version、state_bytes、initial 的 i64 编码、count；后接 count 条 32 B 请求。只允许连续 sequence、operation 1/2、reserved 0，无尾随字节。最大输入 8224 B，非法输入退出 2；成功 0、预期业务失败 1、验证/运行错误 3。状态只在正常业务返回时更新，JSON 输出不包含吞吐/内存结论。

## 资源、失败与验证

driver 固定一 actor、32 个请求/消息槽、四个堆槽、32 MiB VM/tenant/process 上限；模块、栈、堆、节点与消息沿原三级准入。输入、结果及 JSON 是有界外部测试驱动内存，不冒充全部受管账本。actor 阶段共享 3 秒 deadline，Python 子进程 CPU 5 秒/墙钟 10 秒/禁 core，编译沿既有 CPU 20 秒/墙钟 30 秒，超时杀整个子进程组并回收。退出后核对 VM 逻辑计费归零，不当作 RSS 为零。

检查空/最大请求数、三种 state、跨窗口 FIFO、i64 两端、成功与溢出/注入失败优先级、故障后未执行队尾、候选不提交、旧状态诊断无资源租约、短/多余/极端桥接输入及额度清理。执行 workspace、debug/release、MSRV、受限 AOT 和平台回归。BEAM 缺失时明确失败，不用模型结果代替真实 BEAM；单独 Wings+模型回归也须明确命名。

## 对照与重审

不从宿主计算“最后提交值”冒充运行时 state，不捕获 actor 故障后继续处理，不开放任意引用整数或无界完整快照。诊断前缀可能包含业务数据，仅可信宿主显式读取；将来接入外部诊断必须另定权限和脱敏。多 actor/开环负载、热点、心跳、公平性与原生双平台吞吐/整机内存双门禁仍须独立实现和验收。
