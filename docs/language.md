# wings-lang

`examples/tcp-counter.wings` 的 `Apply(Int, Bool) -> Int` 由 [ADR-0048](adr/0048-bounded-loopback-service.md) 的宿主适配器接收真实 loopback 请求；源码只含已支持的状态、checked 算术、条件和请求入口，不增加语言 socket/阻塞 I/O 能力。

[ADR-0047](adr/0047-cross-thread-ingress-requests.md) 仅扩展宿主标量请求入口，保留无 `->` 的单向消息与 `-> Unit` 的请求确认区别；外部线程可以等待结果，业务 handler 仍不支持阻塞 receive/await/call。

[ADR-0046](adr/0046-bounded-cross-thread-ingress.md) 的跨线程 ingress 是宿主入口，只向原线程 actor 投递 Int/Bool/Unit；ADR-0047 增加宿主请求模式，均不增加语言语法或引用消息能力。

宿主资源账本按 [ADR-0045](adr/0045-concurrent-resource-admission.md) 支持跨线程准入和归还，不改变语言语义：业务状态不共享可变数据，actor 与挂起活栈仍在原线程执行，尚无 M:N 迁移。

模块装载寿命由宿主代码租约管理，语言不提供 unload/热更新。heap CLI 和显式 `load_accounted` 使用 [ADR-0044](adr/0044-native-module-leases.md) 的 VM 级模块预算；静态 AOT 不得保留脱离实例的 native 回调或后台线程。

`collect()` 及分配触发的 GC 采用 [ADR-0043](adr/0043-chunked-runtime-gc.md) 的固定工作块：每 16 个节点/根扣一个 runtime 步数，并可在原栈暂停、恢复或取消。总预算包含这些工作，不再只计 IR 指令；中止不提交候选 state/自动回复，已发生消息副作用不回滚。

宿主可用 worker `wait` 推进空闲时的 deadline/完成事件准入，不必 sleep 轮询；仍不在 wait 内执行 handler，也无自动外部 I/O 事件泵，见 [ADR-0041](adr/0041-worker-deadline-parking.md)。

`request(pid, Message, Completed, timeout_ms, args...)` 异步返回局部 RequestId，完成事件为 `on Completed(id: RequestId, status: Int, value: Int)`；status 为 Reply/TimedOut/Cancelled/Failed 的 0/1/2/3，仅接受 Int 回复。`cancel_request(id)` 不撤回目标消息，静态/额度/队满契约见 [ADR-0040](adr/0040-language-request-events.md)。

`self_pid()` / `spawn_self()` 返回局部 Pid，`send(pid, Message, args...)` 向同 worker 同模块的单向入口投递；仅 handler 可调用。Pid 暂不允许 state、消息、回复或比较。创建继承资源配置、不继承状态，细节见 [ADR-0039](adr/0039-local-process-capabilities.md)。

handler 内 `send_self(Message, args...)` 以本 process 单向入口为静态目标，调用点入队、返回 Unit；初始化/fn 禁止，只能由 worker 驱动。失败不撤回此前发送，队满/额度拒绝受管失败，见 [ADR-0038](adr/0038-self-send-effects.md)。同 worker 同模块创建/发送由 ADR-0039 扩展，异步整数请求事件由 ADR-0040 扩展。

`IntList` 支持 `empty_ints()`、`prepend_int(Int, IntList)`、`head_int(IntList)`、`tail_ints(IntList)` 和 `is_empty_ints(IntList)`；可作 state/局部/fn 值，不可作消息参数/回复。列表不可变，空 head/tail 受管失败，节点及共享尾计费/根边界见 [ADR-0037](adr/0037-immutable-int-lists.md)。

process 内可按 [ADR-0042](adr/0042-managed-function-frames.md) 声明有显式参数/返回类型的 `fn`：只访问参数/局部和更早函数，不访问 state，禁止递归/while。私有受管调用支持记录/列表参数与返回、collect，逐帧保留精确根；每函数最多 4096 IR 操作、全程序最多 65536 操作，聚合根和剩余栈也须准入。

持久记录状态扩展见 [ADR-0027](adr/0027-persistent-record-state.md)：可声明 `state value: RecordInt = record_int(0)`，替换绑定及 `read_int(value)`；消息字段和回复仍禁止 RecordInt。执行必须显式配置计费堆，不接受外部 handle。

条件表达式 `if flag { expression } else { expression }` 要求 Bool 条件与同类型结果，支持标量、RecordInt 及 IntList，只求值选中臂。臂内暂不支持语句；可嵌套及用于普通表达式位置。见 [ADR-0029](adr/0029-scalar-conditional-expressions.md) 与 [ADR-0033](adr/0033-record-conditional-roots.md)。

标量比较按 [ADR-0030](adr/0030-scalar-comparisons.md)：`==`/`!=` 接受同类型 Int/Bool/Unit，`<`/`<=`/`>`/`>=` 仅接受有符号 Int，结果为 Bool；优先级低于算术，不支持数学式链式比较或 Ref 比较。

handler 可写 `while condition { statements }`，Bool 条件、Unit 体，let 不可变且体内局部不逃逸；每轮仅更新候选 state，正常返回才提交。支持标量、RecordInt 与 IntList state，尚无 break/continue 或局部循环携带值，见 [ADR-0031](adr/0031-scalar-state-loops.md) 与 [ADR-0034](adr/0034-record-state-loops.md)。

[文档目录](../docs.md) · [项目概览](../README.md)

Wings 用 `process` 声明进程类型，用 `on` 声明消息入口。参数和回复类型显式标注，局部绑定可以推导类型。下面的示例可通过检查；当前源码的标量 AOT 原型还可在可信本机路径编译并执行，不代表完整微进程 VM 已实现：

```text
process Counter {
  state count: Int = 0

  on Inc(by: Int) -> Int {
    let next = count + by
    count = next
    next
  }

  on Reset {
    count = 0
  }

  on Set(value: Int) {
    count = value
  }

  on Get -> Int {
    count
  }

  on Confirm -> Unit {}
}
```

## 声明、消息与返回值

- `process Counter` 声明类型，不会立即创建进程。当前每个进程恰好声明一个具名 state，初始化表达式不能引用状态、参数或 handler 局部变量。
- `on Inc(by: Int) -> Int` 接收请求，末尾表达式是回复值。`-> Unit` 仍然要求一次确认回复，不能与单向消息混用。
- 没有 `->` 的入口只接收单向消息，块结果必须是 `Unit`。不通过返回值隐式推断是否需要回复，也不静默丢弃非 `Unit` 的尾表达式。
- 消息名以大写字母开头，同一进程内不重名，不按参数或回复类型重载。无参数时可以省略 `()`。
- handler 声明同时定义该进程的消息协议：消息名、具名字段、字段类型及可选回复类型。业务代码不必再写一份等价的消息枚举。跨模块消息命名、导出和系统事件类型仍待设计。
- 不按消息来源再分一类入口；普通消息、回复、超时和退出事件都走消息分派。系统事件的具体模式尚未实现，不能与业务消息仅凭同名构造器混淆。

类型名使用 `Int`、`Bool`、`Unit`。当前 `Int` 使用有符号 64 位，`Bool` 值为 `true`、`false`，`Unit` 的值为 `()`。算术只接受 `Int`，不做隐式类型转换。加、减、乘和取负发生溢出时受管失败；除法向零截断，除零及最小整数除以 `-1` 也失败，不回绕、不饱和、不跨 ABI unwind。当前字面量由前端按整数 token 检查，最小值可通过表达式 `-9223372036854775807 - 1` 表示。

## 语句与表达式

块使用 `{}`。完整语句之间用换行或 `;` 分隔；没有 `;` 的末尾表达式作为块结果。赋值、`let` 和空块的结果都是 `Unit`；在末尾表达式后加 `;` 会丢弃其值。

```text
process Flag {
  state enabled: Bool = false

  on Set(value: Bool) -> Bool {
    let previous: Bool = enabled
    enabled = value
    previous
  }
}
```

`let` 绑定不可变，可以省略类型标注，不能重新赋值。当前同一 handler 内禁止重名，也不允许参数或局部变量遮蔽状态名。局部变量从初始化表达式之后可见，不能提前引用或引用自身。

四则运算按通常优先级结合，支持括号和取负。未加括号时，完整表达式后的换行结束该语句；需要跨行时用括号，或把运算符放在上一行末尾。`#` 到行末是注释，空白和注释保留在 token 流中。

## 状态与失败

### 不可变记录

当前受限记录类型为 `RecordInt`，可作为局部或 state 值。`record_int(Int)` 创建单 Int 字段记录，`read_int(RecordInt)` 读取该字段，`collect()` 返回 Unit 并显式触发本进程 GC。这些内建只能按固定名称调用，参数从左到右求值；不是通用函数或 native FFI。普通绑定可遮蔽内建名称，遮蔽后的名字不再可调用。记录不可变，不能对局部重新赋值或修改字段；state 只可替换绑定。

```text
process Records {
  state count: Int = 0
  on Work(value: Int) -> Int {
    let saved: RecordInt = record_int(value)
    collect()
    count = read_int(saved)
    count
  }
}
```

该子集可检查并 AOT 编译运行，CLI 需显式传 `--heap-records`；宿主/worker 分别用 `instantiate_with_heap`/`spawn_with_heap`。消息字段和回复的 `RecordInt` 类型仍静态拒绝。堆容量固定，分配不足从 compiler 活跃根和宿主已提交状态根收集一次，仍不足则受管失败且不提交状态/成功回复。候选状态保活到出口，旧状态直到正常返回并验证输出后才替换；单槽堆不能在替换前回收旧状态，二槽堆可在后续调用收集旧对象。原生栈不保守扫描，不因局部名字仍在作用域就保留已无后续用途的记录。嵌套内建和括号内换行受既有表达式限制约束；不允许任意用户函数调用。契约见 [ADR-0026](adr/0026-language-temporary-records.md) 和 [ADR-0027](adr/0027-persistent-record-state.md)，持久状态示例为 `examples/record-state.wings`。

业务值不可变，状态绑定可以替换。`count = next` 表示替换本次 handler 的候选状态，不代表对象字段可变，也不暴露共享可变引用。后续语句读取更新后的候选值。

执行契约如下；标量 AOT 原型已实现候选状态与正常返回提交，完整 VM 的消息投递、退出和监督尚未实现：

1. handler 开始时，以当前已提交状态作为候选状态。
2. 赋值只更新候选状态；没有赋值就保持原值。
3. 正常结束时提交候选状态，再投递请求入口的自动回复。
4. 受管失败时不提交候选状态，也不发送成功回复；进程退出或监督恢复按运行时规则处理，不保证进程继续使用旧状态运行。

状态常驻 arena，不在每条消息前后序列化。结构共享只限进程内部；消息复制和冬眠仍需遍历数据，其成本取决于编码后大小及共享结构的处理方式。候选状态和临时值也必须计入资源预算。

## 异步交互与副作用

当前同 worker、同模块的最小调用方向如下；handler 内可运行完整示例见 `examples/request-events.wings`：

```text
send(counter, Reset)
let id = request(counter, Inc, Completed, 1000, 2)
```

`send` 面向单向入口，`request` 当前仅面向 `-> Int` 入口；Completed 必须是 `(RequestId, Int, Int)` 的单向系统完成入口，不可由普通发送伪造。发送前检查静态协议，装载/动态边界再次校验。准入失败为受管 RequestRejected，不伪装成功；未来可恢复错误值另行设计。

`request` 成功返回局部 `RequestId`，不返回业务结果，handler 随后正常返回；回复/超时/取消/对端退出通过完成事件给出 id、status 和 value。每个请求只选一次结果，迟到回复不覆盖；队满时保留结果及已预留消息，空位可用后入队。当前 id 不能保存到 state 或转成整数，关联 state 是后续能力。取消不撤回目标消息或副作用。

首版不支持函数内部阻塞 `receive`、`await` 或同步 `call`。当前 deadline 由 tick 扫描，自动 timer/OS 事件泵尚未集成；没有 tick 就不主动投递。未来等待语法须另行设计可序列化续体，不能复用 Idle 冬眠假设。异步 I/O 通过事件衔接，不能阻塞调度线程；请求/事件预留受三级资源约束。

受管入口的逻辑签名为 `handle(context, state_handle, message_handle) -> HandlerResult`，这不是普通 Rust ABI。context 只提供受控的 runtime 内建和进程资源访问。

`send`、`request` 和 `spawn` 在调用点完成额度准入并返回结果，副作用随即生效，不等 handler 返回后再统一执行 effects 列表。handler 随后失败，也不会撤回已经发送的消息或创建的进程。新 state 和自动 reply 只在正常返回时提交；提交后 reply 投递失败不回滚 state，调用方通过超时或退出事件结束等待。

编译器必须把 runtime 调用建模为具有副作用的受管调用，并在可能分配、让出或 GC 的位置维护活跃根。候选状态与回复都是根维护的一部分；内部 `HandlerResult` 由编译器构造，不要求业务代码手写协议 tuple。该模型不承诺事务或 exactly-once，业务重试需要自己的幂等键。

## 语法取舍

采用简单的静态类型和表达式，消息入口直接写进语言，不复制 Actor 库的回调样板：

| 参考 | 采用 | 不采用 |
|---|---|---|
| Erlang / Elixir | 消息隔离、监督与按消息分派 | 三类回调入口、位置返回 tuple，以及 Elixir 语法兼容 |
| Gleam | 显式边界类型、局部推导、带数据的消息与模式匹配方向 | 手动连接每个 handler 的回复通道与状态返回值 |
| Pony | 直接声明进程和消息入口 | 把完整引用能力系统带入最小语言 |
| Rust | 参数与回复签名、联合类型和穷尽匹配方向 | 业务层所有权、生命周期和借用标注 |
| Go | 少量规则、明确控制流 | 每个进程手写接收循环和阻塞 `select` |

后续普通代码按动态派发、`match`、通用记录和联合类型推进；目前已有有界非递归 fn 调用帧、`let`、标量/RecordInt `if` 与 state while 和记录内建，不支持递归或函数值。不引入宏、隐式转换、用户自定义运算符或监督 DSL。监督树先用显式配置，`link`/`monitor` 走受控内建。具体理由见 [ADR-0007](adr/0007-message-handlers-and-state.md)。

## 当前原型支持范围

`examples/counter.wings` 可通过 `wings-compile check` 检查，也可用 `wings-compile build` 生成可信本机标量产物。crates.io `0.0.2` 已包含此构建入口，旧版 `0.0.1` 仍只提供检查器。

未发布源码插入取消、让出和预算检查。总预算耗尽、取消、入口剩余栈不足或记录堆耗尽属于受管失败，候选状态与自动回复仅正常返回后提交；时间片耗尽可由宿主在同线程恢复原栈，不重放 handler，也不表示语言支持 `await` 或阻塞接收。当前 ABI v10 不兼容已发布 `0.0.2` 及旧源码 v2–v9 产物，需重新编译；原栈恢复见 [ADR-0011](adr/0011-scalar-stackful-resume.md)，栈边界见 [ADR-0024](adr/0024-scalar-stack-boundary.md)，compiler 根与持久状态分别见 [ADR-0025](adr/0025-aot-record-roots.md) 和 [ADR-0027](adr/0027-persistent-record-state.md)。

当前子集包含：

- `process`、一个具名 state、统一的 `on`、具名类型化参数和可选回复类型。
- `Int`、`Bool`、`Unit`、RecordInt/IntList 引用类型，整数、布尔与 `()` 字面量，括号、取负和四则运算。
- 同类型标量相等与有符号 Int 顺序比较，结果为 Bool，不比较引用。
- 状态赋值、不可变 `let`、可选局部类型标注、多语句块和尾表达式。
- `RecordInt` 局部/state、受限记录内建、同类型标量/RecordInt `if` 与唯一 state 的 while；无任意引用 phi。
- handler 内携带唯一标量/RecordInt state 的 `while`，Bool 条件、Unit 体；块最多 64 层，总预算和每回边检查不可绕过。
- 名称解析、重复声明、赋值目标、初始值、消息参数运算和回复类型检查；单向消息与 `-> Unit` 在语义模型中分开保存。
- process 内最多 64 个 fn、64 个显式类型参数、32 层声明顺序调用；使用私有受管帧，无 state 访问/赋值或函数内 while，每路径最多 64 个聚合根，各帧独立检查剩余栈。
- 不可变 IntList 的单节点构造/读取和传递 GC 根，条件/循环/fn 支持精确 ListRef 类型，不与 RecordInt 混用。
- handler 中 `send_self` 静态检查单向目标、标量参数和遮蔽；worker 调用点准入入队，队满/额度拒绝受管失败，无 worker 不可调用。
- 局部 Pid、`self_pid` / `spawn_self` 与同 worker 同模块 `send`；fn 可传递 Pid，但不得在函数内创建/发送，无 Pid state/消息/回复或比较。
- 局部 RequestId、异步 Int 请求和三参数完成事件；完成事件不可由普通发送/CLI 伪造，取消只能终结当前发起方仍占 pending 的请求，无阻塞等待。

尚未实现模块、动态派发/递归、任意引用 phi、通用记录、联合类型、完整模式匹配、跨模块消息、通用异步请求或外部系统事件。CLI 装载一个 process，可显式配置记录堆保存局部与状态值；宿主 worker 已有有限消息、请求、资源账户和监督切片，自发送和同模块创建/发送只验证调用点副作用，不代表完整语言交互或恢复契约已实现。驱动协议及可信边界见[架构原型说明](architecture.md#标量-aot-原型)。

解析保留空白和注释 token，错误恢复继续识别后续 handler；原型限制每份源码 1 MiB、单个表达式 256 次解析调用，每次最多输出 128 条诊断，超限报错或截断诊断。这些是工具原型限制，不是最终语言资源契约。旧原型语法已移除，不维护两套解析规则。

## 编辑器支持

`wings-lsp` 共用 `wings-syntax` 的容错解析和 `wings-sema` 的符号、类型信息，首批支持诊断、跳转、悬停和基础补全。未保存或不完整的源码可以继续分析，但编译与编辑器采用同一套语言规则，不执行程序获得查询结果。文档版本、位置编码和依赖方案见 [Crate 拆分](crates.md)。

当前服务支持 stdio、UTF-16 位置、整份文档更新和单文件查询，丢弃旧版本文档更新；分析同步执行，未实现跨文件项目模型、细粒度增量和运行中取消，不声明相关 capability。后续工作按 crate 方案推进。

## 相关文档

- [运行时](runtime.md)说明消息、资源额度、超时和监督。
- [架构设计](architecture.md)说明编译后端与受管 ABI。
- [架构决策](adr/0006-workspace-and-lsp.md)说明编译器与 LSP 共用前端的原因。
- [激进优化](optimizations.md)说明类型特化、拆箱、逃逸分析和专用消息布局；优化不得改变值语义与副作用时机。
