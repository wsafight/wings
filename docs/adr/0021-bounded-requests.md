# ADR-0021：有界宿主请求表与超时终结

- 日期：2026-11-12
- 状态：已采纳（单 worker 标量请求切片）
- 补充：[有限 mailbox](0020-single-worker-mailbox.md)

## 决定

- `ScalarWorker` 增加固定容量 pending 表，默认与进程槽数相同，可在构造时显式限定 1..=256 槽。表容量在分配前归 tenant 计费，不因请求增长。RequestId 包含 worker、槽号和 generation，完成取走后递增，耗尽退休。
- `request` 只接受有回复入口（包括 `-> Unit`），校验模块、参数和单调时钟 deadline，在消息和 requests 三级预留、可用 pending 槽及 mailbox 都满足后才同时入队/登记。任一步失败不留下半请求或消息，未完成和已完成未取走都占 pending 容量。
- 请求不阻塞 worker/actor。成功 handler 提交状态后才将 Reply 写入 pending，Unit 按现有标量值 0 表示，schema 可由调用者原 HandlerId 解释。`take_reply` 非阻塞，未终结返回 None，已终结取走并归还 requests 额度；重复或 stale ID 拒绝。
- 每次 tick 和 take_reply 前按当前单调时钟扫描有界表，deadline 已到即终结 TimedOut。取消只终结 Cancelled，不撤回消息或远端副作用、不自动重试；迟到成功/失败不能覆盖已经选定的终结结果。生成代码的总步数预算仍与 request deadline 不同。
- 调用者取走已超时/取消结果后可以复用槽位；晚到回复必须匹配完整 RequestId，不能写入复用的新请求。处理完成但尚未取走的结果不丢弃，pending 满时显式拒绝新请求。
- actor 算术/预算失败或宿主 stop 将该 actor 尚未终结的请求标为 Failed，并正常清理活栈和队列；已经终结的 timeout/cancel 保留。worker Drop 才释放全部结果和容量。请求票据与 pending 项同寿命；退出后仍保留的请求额度属于该 actor 的存活账户，直到结果取走才归零。

## 验证与边界

受限 debug/release 子进程验证返回值及 state、连续/FIFO 请求、Unit、表满及 requests/messages 额度失败零变化、零 timeout、挂起期间 timeout/cancel、迟到回复、取走后槽位复用、重复及跨 worker ID 拒绝、失败/stop 终结及 Drop 归零。普通 send 保持只能用于单向 handler。

这是宿主异步 API，不是 actor 内同步 call/await，没有自动定时唤醒；无 tick/poll 时 deadline 不主动发送通知。计时线程、OS reactor、语言请求与跨 actor pending 表仍待接入，不能据此声称真实 I/O、完整 OTP 或 MVP 完成。
