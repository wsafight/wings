# ADR-0138：宿主线程可用栈范围的启动检查

- 日期：2026-11-18
- 状态：已采纳（宿主栈资源边界切片）
- 延续：[ADR-0054](0054-bounded-multi-worker-scheduler.md)、[ADR-0118](0118-native-sync-storage-admission.md)

## 缺口与决定

Scheduler worker 与 DeadlineTimer 在 spawn 前按显式请求的 2 MiB/256 KiB 栈及既有余量预留，但此前没有在新线程启动时核对原生实际可用栈。标准库或 libc 可因最小栈/TLS 要求调整请求；不把 Builder::stack_size 当作实际范围已被验证，也不因为本机恰好相等而推断所有平台/运行环境相同。

新增内部平台查询：macOS 用当前 pthread 的 stackaddr/stacksize，Linux 用 pthread_getattr_np/attr_getstack，销毁成功初始化的 attributes。核对非零且无溢出的地址范围、本线程局部标记在范围内、报告的可用字节不超过原已预留栈桶：worker 为 2 MiB+64 KiB，timer 为 256 KiB+两个系统页。查询失败、无效范围或更大栈都在宣布就绪前失败，不提高预留、不执行受管业务，也不忽略未知结果。Linux 查询可能有 libc 内部暂存，仍不是零原生分配承诺。

初次实现误将显式请求本身当作该上限，本机原生回归和 Scheduler 正常恢复因此失败。保留原可执行和双流；进一步实测两类线程分别报告 274432/2109440 B，均比请求多 12288 B，仍落在原有余量内。修正为原计费桶而非增加任何额度；不将未解释的 12 KiB 直接命名为 guard/TLS，也不把桶内相容误写成所有控制成本已精确容纳。

Scheduler 沿用现有启动结果通道；timer 在原 Control 的 mutex/condvar 内增加一次性启动结果，父线程等待明确成功后才返回可用对象。失败先正常 join，再由原票据退账；不新增线程、channel 或热路径查询。生产回调固定为原生查询，测试可以注入错误，验证同样的失败清理而不是触发宿主栈溢出。

## 边界与验证

这道检查发生在线程已经由 OS 建立之后，不能阻止平台先瞬时分配更大的栈；它拒绝该实例进入业务，不构成全部原生分配前准入。只验证 pthread 报告的可用范围，不等于完整 mapping、guard、TLS、线程控制块、allocator 元数据、缓存或 RSS 计量。原先预留的页/控制余量保持，但不因本检查而称精确覆盖这些项目；TCP 示例适配线程也不在本切片范围。

保持 ABI v22、4352 B 执行控制、业务栈池、actor/mailbox/request/worker 上限、量子和全部 CPU/墙钟期限。确定性回归覆盖零/溢出/超上限/标记不在范围内，真实 256 KiB/2 MiB 线程分别核对原生范围，timer/Scheduler 注入失败检查 join 和精确退账。两构建完整 workspace/AOT、取消/公平进展/恢复及工程/MSRV/交叉检查继续执行，保留全部首次失败；原生 Linux/双平台和 BEAM/MVP 门禁不因此关闭。
