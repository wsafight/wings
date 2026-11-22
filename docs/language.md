# 语言设计（`wings-lang`）

[返回概览](../README.md) · [架构设计](architecture.md) · [运行时](runtime.md)

`wings-lang` 是面向有状态微进程的静态类型语言：业务值全部不可变，进程之间仅通过异步消息交互，无共享可变内存，也无阻塞式 `receive` 或同步 `await`。

> **演进说明**：项目处于早期阶段，语法与内置库直接围绕清晰简洁的现代写法演进，不承诺保留早期过渡语法兼容。

## 完整示例：多 Actor 会话服务

以下示例（源自 [`examples/session-service.wings`](../examples/session-service.wings)）展示了共享 `struct`、可空 `pid?<Actor>` 状态、`spawn`、`monitor`、`send_after` 与异步 `.ask` 回调：

```wings
struct SessionCommand {
  session_id: int,
  prompt: str,
}

struct SessionReply {
  session_id: int,
  turn: int,
  output: str,
}

struct SessionStatus {
  alive: bool,
  turns: int,
  restarts: int,
  ticks: int,
  last_reply: str,
}

actor SessionWorker {
  state turns = 0

  on step(cmd: SessionCommand) -> SessionReply {
    if cmd.prompt == "boom" {
      let crash = 1 / 0
    }
    turns += 1
    SessionReply {
      session_id: cmd.session_id,
      turn: turns,
      output: f"s{cmd.session_id}#{turns}:{cmd.prompt}",
    }
  }
}

actor SessionManager {
  state worker: pid?<SessionWorker> = null
  state worker_mon = 0
  state turns = 0
  state restarts = 0
  state ticks = 0
  state last_reply = "idle"

  on boot -> bool {
    if worker.is_null() {
      let child = spawn(SessionWorker)
      worker = child
      worker_mon = monitor(child, on_worker_down)
      let _t = send_after(10, heartbeat(1))
    }
    true
  }

  on heartbeat(delta: int) {
    ticks += delta
  }

  on dispatch(cmd: SessionCommand) -> bool {
    if let w = worker {
      w.ask(step(cmd), timeout: 1000, then: on_step_done)
      true
    } else {
      false
    }
  }

  on reply on_step_done(id: req, status: int, reply: SessionReply) {
    if status == 0 {
      turns += 1
      last_reply = reply.output
    }
  }

  on on_worker_down(mon_id: int, reason: int) {
    if mon_id == worker_mon {
      restarts += 1
      let replacement = spawn(SessionWorker)
      worker = replacement
      worker_mon = monitor(replacement, on_worker_down)
    }
  }

  on status -> SessionStatus => SessionStatus {
    alive: worker.is_some(),
    turns: turns,
    restarts: restarts,
    ticks: ticks,
    last_reply: last_reply,
  }
}
```

## 类型系统

| 类型 | 说明 | 边界约束 |
|---|---|---|
| `int` | 64 位有符号整数（`i64`） | 加/减/乘/取负/除法溢出或除零均触发受管失败，不静默回绕 |
| `bool` | 布尔值 `true` / `false` | 支持 `!`、短路 `&&` / `||`、`==` / `!=` |
| `unit` / `()` | 单元值 `()` | 无返回值单向 handler 的块结果类型 |
| `str` | 不可变 UTF-8 字符串 | 单值 `0..=64 KiB`，支持字面量转义、插值 `f"..."`、`++`、内容相等比较、按 Unicode 标量值 `.len()` / `.at(i)` / `.slice(start, len)` |
| `bytes` | 不可变字节序列 | 单值 `0..=64 KiB`，支持 `bytes.fill(len, byte)`、`.len()`、`.get(i)`、`.set(i, b)`、64 位大小端读写、切片与拼接 |
| `[int]` | 不可变持久化整数链表 | 支持 `[a, b, ..tail]`、`.is_empty()`、`.head()`、`.tail()`、`.prepend(v)`；跨消息/回复最多 64 个元素 |
| `struct` | 顶层共享或 Actor 内具名不可变记录 | 名义类型等价；每 Actor 最多 32 种 schema、每种最多 8 个字段；支持具名构造与 `Struct { field: v, ..base }` 更新 |
| `pid<Actor>` / `pid?<Actor>` | 静态类型化进程引用 / 可空进程引用 | 支持 `null`、`.is_null()`、`.is_some()` 及 `if let p = opt_pid { ... }` 解包 |
| `req` | 异步请求关联句柄 | 用于 `.ask` 返回值与 `on reply` 完成回调匹配 |

## 状态、函数与控制流

- **多 `state` 声明**：每个 `actor` 最多声明 8 个状态字段（支持从初始表达式推导类型）。handler 执行期间对状态变量的赋值只更新候选状态；**只有 handler 正常返回时才原子提交状态与回复**，算术错误、配额耗尽或预算超时均保留旧状态。
- **表达式与语句**：换行或 `;` 分隔语句；支持 `let` 绑定、结构体字段路径更新、`if / else if / else`、`while` 循环、管道 `x |> fn_call(arg)` 与复合赋值（`+=`、`-=`、`*=`、`/=`、`%=`、`++=`）。
- **受管函数与有界递归**：Actor 内 `fn` 只访问参数、局部绑定与其他 `fn`（不直接读写 `state`），支持直接递归与互递归。每次调用前校验剩余原生栈空间与执行步数预算，并维护跨帧精确 GC 根链。

## 消息、定时器与监控语义

- **单向消息与请求入口**：
  - `on msg(args...) { ... }`（无 `->`）仅接收单向投递；
  - `on msg(args...) -> ReplyType { ... }`（含 `-> ()`）接收请求并返回回复，也可通过 `on msg -> T => expr` 简写。
- **跨 Actor 交互**：
  - `spawn(TargetActor)` 创建子进程并返回 `pid<TargetActor>`（Actor 内置 `self` 为当前进程引用）。
  - `target ! msg(args...)` 或 `target.tell(msg(args...))` 发送异步单向消息。
  - `target.ask(msg(args...), timeout: ms, then: on_done)` 发起非阻塞请求，目标完成、失败或超时后向当前 Actor 投递 `on reply on_done(id: req, status: int, value: ReplyType)` 事件。
- **延时消息与监控**：
  - `send_after(delay_ms, msg(args...))` 在指定毫秒后向 `self` 投递单向消息。
  - `monitor(target_pid, on_down_handler)` 监控目标 Actor 生命周期；若目标已退出或后续异常/正常终止，向当前 Actor 投递一次 `on_down_handler(mon_id: int, reason: int)` 通知。
- **副作用边界**：`spawn`、`!` / `.tell`、`.ask`、`send_after`、`monitor` 在调用点立即生效并扣费；后续若当前 handler 发生受管失败，已发出的消息或创建的子进程不会撤回（不承诺跨进程事务）。
