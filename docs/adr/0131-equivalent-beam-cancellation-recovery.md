# ADR-0131：BEAM 有限请求的取消与监督恢复对照

- 日期：2026-11-18
- 状态：已采纳（恢复正确性对照；完整故障与性能验收独立）
- 补充：[ADR-0121](0121-persistent-cancellation-recovery.md) 的相同恢复业务；不改变 Wings 执行、ABI v22、资源额度或原限。

## 请求与消息

新增 OTP 29 的独立 BEAM 运行端，以真实两个发送进程复现 4 KiB 不可变 Bytes、mailbox 16、共享结果容量 17。沿用 [ADR-0067](0067-beam-bounded-admission.md) 的 CAS 有限准入，actor 开始处理才归还邮箱容量；Hold 是单向消息，占住实际 handler，后续十六条 Add 留在队列。

请求适配器是单个持续 `gen_server`，最多持有十七条真实票据；票据同时记录 caller 消费和 publisher 完成。每发送者前两条使用已经到期的截止时间，第三、四条显式取消并消费，余下四条取消与停止竞争。取消/超时只决定结果，不删除 actor 已接受的消息；caller 消费后的票据仍保留，直到实际回复或 actor 的 monitor DOWN。消费一次后再次读取返回缺失。第十七条向满目标发出的请求须被拒绝，前后实际队列数、票据数相同；此时另一个 mailbox 1 的健康 actor 仍能利用第十七个结果槽提交 +7。

适配器实现的是本对照所需的有限请求契约，不声称 `gen_server:call` 原生提供相同取消或计费。其进程、票据、monitor 和监督开销全部留在 BEAM OS 作业内，不从进程树内存扣除。BEAM 不存在 Wings 的三级账本；逻辑留存/栈字段为 null，仅记录实际协议容量与进程/port 数，不伪造跨 VM 的字节对等。

## 监督与失败

持久 root supervisor 和请求适配器跨轮复用。每个目标 child 有独立的单 child OTP supervisor，`one_for_one`、强度一次/60s，健康 sibling 由 root 单独监督。这样目标强度耗尽只结束该目标的监督分支；不能直接把 Wings 按 child 的强度套成所有业务 child 共用的 OTP 强度。每轮新 child 显式创建新的监督分支和身份，不把它说成自动重启。

显式停止 Hold 后，该 child 保持 stopped 且不自动重启。随后显式启动零初值目标，两条已取消的 Fail 仍须进入真实 handler：建立候选 99，实际触发除零，报告完整旧 4 KiB 零状态，并由 monitor 确认 `badarith` 退出。第一次由 OTP 自动创建新进程和全新零状态；第二次超限使目标 supervisor 退出。健康 actor 的完整 4 KiB 状态仍为 +7。结束时清空真实票据并确认所有 actor/发送者终止；本轮与下一轮目标活着时分别验证旧 Pid、入口和监督 child ID 失效。

## 验证与限制

`verify_recovery_soak.py --compare-beam` 在相同一/双 worker 或 BEAM scheduler 下各运行三轮；默认完整 AOT 继续不要求安装 OTP，但同时验证两种运行端的重放模型。BEAM 的 scheduler 总数及 online 数都必须与参数一致，启动环境清理继承的 Erlang 配置后仅显式设置 `+S N:N`。

`measure_recovery_soak.py --compare-beam` 才运行两端各配置 900 轮，仍为每 100ms 一轮、30s 预热/60s 采样，不跳迟发轮。每轮三秒，整体阶段 120s；短回归 CPU 5s/墙钟 10s，长测 CPU 240s/墙钟 130s/core 0，64 KiB 固定写缓冲/64 MiB trace 和原 1 GiB RSS 安全停止线保持。BEAM 业务/适配器进程各有 262144 words 的 heap 限制，并保留 VM process/port 上限 1024。只有冻结源码/运行端执行，原始逐条结果独立重放，失败保留 stdout/stderr、部分 trace 和 invalid manifest，不用重试抹去。

本轮覆盖超时/取消不撤回、stop 竞争、失败状态不提交、全新恢复及强度上限。它没有过载或服务 I/O 竞争、完整公平性、CPU 带宽硬配额、完整平台内存/监测偏差或原生双平台结论。协议通过不是吞吐或整机内存门禁通过；MVP 状态保持未完成。实际记录见[验证记录](../validation-beam-recovery-2026-11-18.md)。
