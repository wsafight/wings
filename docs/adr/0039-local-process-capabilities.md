# ADR-0039：同 worker 创建与语言进程能力

- 日期：2026-11-13
- 状态：已采纳（M0/M1 跨进程副作用切片）
- 补充：[ADR-0038](0038-self-send-effects.md)

## 决定与理由

在自发送交接上开放 `self_pid() -> Pid`、`spawn_self() -> Pid` 和 `send(pid, Message, args...) -> Unit`。先限同 worker、相同静态模块，不把整数当完整身份，不在尚无模块协议与计费转移契约时开放任意创建与投递。

- Pid 是不可伪造的语言类型，当前仅作 handler 局部、内联 fn 参数/结果和条件值；禁止 state、消息字段、回复、整数转换及比较。它不是宿主 generation Pid 的整数编码。每次成功创建获得全局单调且不复用的能力号，保存在固定 actor 槽；查找最多扫描 256 槽，并校验当前 worker 和模块。进程退出即失效，槽位复用不恢复旧能力，计数耗尽拒绝创建。可信 native 仍在可信边界内。
- `spawn_self` 创建本模块全新实例，继承调用者的进程资源上限、mailbox 容量和显式堆容量，使用独立进程账户；不继承已提交/候选 state、消息或父子监督关系。创建在调用点完成，失败返回 SpawnRejected，已创建子进程不因父 handler 后续失败而退出。initializer 仍走既有有界计费栈和预算；容量/初始化失败完整回滚。
- `send` 静态目标仍为本模块单向 handler，参数仅 Int/Bool/Unit；运行时再次校验能力、模块、kind、schema 和资源。发送当前总预算作为新消息的独立上限，FIFO 与拒绝/清理规则沿用 ADR-0038。跨 worker、失效或不同模块目标返回 SendRejected，不重新解释为当前槽位。消息不携带 Pid，故本切片尚无双向语言请求或父地址传递。
- 三个操作只在 handler 内允许，fn/initializer 禁止；独立实例与 CLI 无 effect 驱动，返回 InvalidControl。每个 effect 仍只交出一次，worker 当片完成后保存确认，下一片恢复；创建/入队与后续成功不是同一保证。
- ABI v9：context 仍为 88 字节，EffectCall 改为 `(context, request, output) -> status`。request 是固定 C 布局 operation/handler/target/arguments/count，回调将请求与最多 64 参数按值复制交出；确认包含 status 与标量结果，只在恢复原栈后写 output。worker 不保存请求、输出或根指针。编译器在 effect 前维护根和预算，错误解链且不提交候选。旧 v8 产物重编译。
- 能力号与 child 配置是固定 actor 元数据，包含在 size_of 准入中，无增长能力表；创建/消息使用既有三级账户。宿主可通过无分配 `pids()` 迭代当前实例用于诊断/stop，不执行业务。

## 验证与重审

验证 Pid 不能由整数、state、消息/回复或比较伪造；静态及动态 kind/schema；同模块两个实例、创建时新 state、发送只执行一次、父失败后子仍存在；满进程表/额度/初始化失败无半创建；过期能力与复用槽位、跨 worker/模块拒绝；Drop/stop 不撤回或重放已创建子项；effect 前后强制 GC 与三层清理守恒。debug/release、MSRV、双平台分别运行，不将本机通过写成双平台结果。

后续开放 Pid 消息/state、不同模块 spawn、异步 request/reply、监督或跨 worker 迁移时，须补身份传递、目标协议与计费/失败边界。本切片不是完整消息系统或 MVP。
