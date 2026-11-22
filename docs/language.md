# Language Design (`wings-lang`)

**English** | [简体中文](zh/language.md) · [Back to Overview](../README.md) · [Architecture](architecture.md) · [Runtime](runtime.md)

`wings-lang` is a statically typed language tailored for stateful microprocesses: all business values are immutable, actors interact exclusively via asynchronous messages, there is no shared mutable memory, and handlers never block on selective `receive` or synchronous `await`.

> **Evolution Note**: During `0.0.x`, syntax and built-ins evolve directly toward clarity and conciseness without retaining legacy transitional forms.

## Complete Example: Multi-Actor Session Service

The following example (from [`examples/session-service.wings`](../examples/session-service.wings)) demonstrates shared `struct` definitions, nullable `pid?<Actor>` state, `spawn`, `monitor`, `send_after`, and asynchronous `.ask` reply callbacks:

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

## Type System

| Type | Description | Bounds & Semantics |
|---|---|---|
| `int` | 64-bit signed integer (`i64`) | Checked add/sub/mul/neg/div/rem; overflow or division by zero triggers a managed fault rather than wrapping |
| `bool` | Boolean `true` / `false` | Supports `!`, short-circuit `&&` / `\|\|`, and `==` / `!=` |
| `unit` / `()` | Unit value `()` | Result type of one-way handlers and unit expressions |
| `str` | Immutable UTF-8 string | `0..=64 KiB` per value; supports escapes, `f"..."` interpolation, `++`, equality, and Unicode scalar `.len()` / `.at(i)` / `.slice(start, len)` |
| `bytes` | Immutable byte buffer | `0..=64 KiB` per value; supports `bytes.fill(len, byte)`, `.len()`, `.get(i)`, `.set(i, b)`, 64-bit endian reads/writes, slicing, and concatenation |
| `[int]` | Immutable persistent `i64` list | Supports `[a, b, ..tail]`, `.is_empty()`, `.head()`, `.tail()`, `.prepend(v)`; up to 64 elements across message/reply boundaries |
| `struct` | Top-level shared or actor-local record | Nominal typing; up to 32 schemas per actor and 8 fields per schema; supports named literals and `Struct { field: v, ..base }` functional updates |
| `pid<Actor>` / `pid?<Actor>` | Statically typed / nullable process handle | Supports `null`, `.is_null()`, `.is_some()`, and `if let p = opt_pid { ... }` narrowing |
| `req` | Async request correlation token | Returned by `.ask` and matched inside `on reply` completion handlers |

## State, Functions & Control Flow

- **Multiple `state` Bindings**: Each `actor` may declare up to 8 state fields (with type inference from initializers). Assignments during a handler mutate candidate state only; **state updates and replies commit atomically only on normal handler return**, preserving prior committed state on arithmetic faults, quota exhaustion, or budget cancellation.
- **Statements & Expressions**: Statements separate via newlines or `;`; supports `let` bindings, nested struct field updates, `if / else if / else`, `while` loops, pipe expressions `x |> fn_call(arg)`, and compound assignments (`+=`, `-=`, `*=`, `/=`, `%=`, `++=`).
- **Managed Functions & Bounded Recursion**: Actor-local `fn` items operate on parameters, local bindings, and sibling `fn` calls (without implicitly mutating actor `state`), supporting both direct and mutual recursion. Every call validates remaining native stack space and execution step budgets while maintaining exact cross-frame GC roots.

## Messaging, Timers & Monitoring

- **One-Way Casts vs. Typed Requests**:
  - `on msg(args...) { ... }` (without `->`) accepts asynchronous one-way casts;
  - `on msg(args...) -> ReplyType { ... }` (including `-> ()`) handles request-reply calls, with `on msg -> T => expr` shorthand for single-expression handlers.
- **Cross-Actor Operations**:
  - `spawn(TargetActor)` creates a child actor and returns `pid<TargetActor>` (`self` refers to the current actor).
  - `target ! msg(args...)` or `target.tell(msg(args...))` enqueues a non-blocking one-way message.
  - `target.ask(msg(args...), timeout: ms, then: on_done)` issues a non-blocking request and delivers `on reply on_done(id: req, status: int, value: ReplyType)` upon completion, failure, or timeout.
- **Timers & Lifecycle Monitors**:
  - `send_after(delay_ms, msg(args...))` schedules a delayed one-way message to `self`.
  - `monitor(target_pid, on_down_handler)` watches a target actor and delivers `on_down_handler(mon_id: int, reason: int)` once if the target has already exited or terminates later.
- **Effect Commit Boundary**: `spawn`, `!` / `.tell`, `.ask`, `send_after`, and `monitor` take effect and charge quotas immediately at the call site; if the handler subsequently faults, already-dispatched messages and spawned children are not rolled back (no cross-process distributed transactions).
