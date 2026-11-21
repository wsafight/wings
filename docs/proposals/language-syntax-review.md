# Wings 语法评审稿：基础类型大小写与最小一致性规则

> **待用户评审，未采纳、不改变当前语法。**
>
> 本稿只提出方案，不授权修改编译器、LSP、示例、现行契约或 ABI。下文明确区分当前实现、推荐变更和以后可能设计的能力；使用小写类型的代码目前不能直接编译。

- 复核日期：2026-11-15；依据 `feat/mvp` 当前前端、语义检查、AOT lowering、LSP 与仓库示例；当前源码 ABI v15，不代表 crates.io `0.0.2` 的能力。
- 实现基线：语言 `request` 已支持 Int/Bool/Unit/RecordInt/IntList 回复；RecordInt 随 `2d59f98`、有界 IntList 随 `d164e01` 本地提交。IntList 已通过 macOS debug/release 与 Linux 隔离仿真功能验证，原生 Linux 和完整 MVP 门禁仍未完成，见[验证记录](../validation-2026-11-15.md)。
- 起因：[`examples/records.wings`](../../examples/records.wings) 中的 `Int` 能否像 `process` 一样小写。
- 推荐范围：首先只讨论类型拼写、名称约定及迁移，不把通用记录、泛型、模块或控制流重构绑进这次修改。
- 当前契约仍以[语言文档](../language.md)和已采纳的 [ADR](../adr/README.md) 为准；本文不替代它们。

## 1. 先给结论

**推荐：关键词小写，基础标量类型用 `int`、`bool`、`unit`；具名类型、process 名和消息名保留大写开头，普通值与函数用小写下划线。**

建议的目标外观如下，**这是评审示例，目前小写类型尚不支持**：

```text
process Records {
  fn twice(value: int) -> int { value + value }

  state count: int = 0

  on Work(value: int) -> int {
    let first: RecordInt = record_int(twice(value))
    collect()
    count = read_int(first)
    count
  }

  on Reset { count = 0 }
  on Confirm -> unit {}
}
```

**现在不能把类型标注中的 `Int/Bool/Unit` 直接写成 `int/bool/unit`；语言设计上可以支持，但须先批准方案并同步检查器、lowering 和 LSP。** 当前小写名称能通过词法和类型标注的语法解析，随后由语义检查报未知类型 `T002`，不是词法错误。

`process` 是声明关键词，词法阶段就识别为专用 token；当前 `Int`、建议的 `int` 都是普通 Identifier，由类型位置的语义规则解释。不要求关键词与类型名采用同一大小写，也不需要为小写类型增加关键词。保留这一区别，可以实现小写写法而不让 `let int = 1` 意外变成词法错误。

选择该方案的理由：它直接回应用户对 `int` 的偏好；保留 `RecordInt`、`IntList`、`Bytes`、`Pid`、`RequestId` 作为具名类型的视觉区分；不改变状态、消息、GC、调用帧或数值语义。它不是唯一正确的风格，下面列出另外两种一致方案供选择。

## 2. 已核对的当前实现

### 2.1 核对入口与准确边界

| 入口 | 当前事实 |
|---|---|
| [lexer](../../crates/wings-syntax/src/lexer.rs) | 关键词区分大小写；类型拼写均被词法分析为普通 `Identifier` |
| [AST](../../crates/wings-syntax/src/ast.rs) 与 [parser](../../crates/wings-syntax/src/parser.rs) | 类型标注保存单个 `Name`，没有类型实参或具名记录声明节点 |
| [语义检查器](../../crates/wings-sema/src/lib.rs) | `declared_type` 精确匹配八个预定义类型，`Type::Display` 输出当前大写拼写 |
| [AOT lowering](../../crates/wings-compile/src/lower.rs) | `value_type` 再次匹配 state、消息字段/回复和 fn 参数/结果的源码类型名；仅修改检查器会使这些小写标注 check 成功、build 失败，局部 let 标注则在检查后按表达式类型降低 |
| [IR](../../crates/wings-ir/src/lib.rs) 与 [ABI](../../crates/wings-abi/src/lib.rs) | 使用类型枚举与受管操作，不根据源语言类型的大小写执行 |
| [LSP](../../crates/wings-lsp/src/lib.rs) | 类型补全和内建的补全 `detail` 有固定字符串，符号悬停依赖语义类型显示；没有独立的 `signatureHelp` 能力 |
| [前端测试](../../crates/wings-syntax/tests/frontend.rs)、[语义测试](../../crates/wings-sema/tests/analysis.rs)、[LSP 测试](../../crates/wings-lsp/tests/protocol.rs) | 已有语法恢复、名称遮蔽、类型错误、单向消息与 Unit 回复区分等回归，可承接拼写迁移测试 |

本文据当前工作区实现核对，不把主题文档中的未来方向算作现有能力，也不把本地提交写成已发布能力。当前未知类型诊断已列全八个预定义类型，包括 `Bytes`、`Pid`、`RequestId`。

### 2.2 八个实际可识别的类型

| 当前拼写 | 当前含义和允许位置 | 推荐拼写 |
|---|---|---|
| `Int` | 有符号 64 位整数；state、参数、局部、fn 结果及消息回复等标量位置 | `int` |
| `Bool` | 布尔，值为 `true`、`false`；标量位置 | `bool` |
| `Unit` | 单一值 `()`，不是无返回协议；标量位置 | `unit` |
| `RecordInt` | 单 Int 字段的不可变受管记录；state、局部和 fn 参数/结果；语言消息参数及回复均支持内容复制，完成入口第三参数须匹配 RecordInt | 保留 `RecordInt` |
| `IntList` | 不可变整数列表；state、局部和 fn 参数/结果；语言消息参数支持有界复制，每条消息的所有列表参数合计最多 64 个整数元素；支持至多 64 元素的内容复制回复及精确匹配的完成入口 | 保留 `IntList` |
| `Bytes` | 0..=65536 字节的计费不可变值；state、局部和 fn 参数/结果；语言消息参数支持内容复制，单条消息所有 Bytes 参数合计最多 65536 字节；不是字符串，回复仍拒绝 | 保留 `Bytes` |
| `Pid` | 局部进程能力，可经 fn 传递；无 state、消息字段或回复支持，不支持比较 | 保留 `Pid` |
| `RequestId` | 局部请求能力，可经 fn 传递；无 state 或普通消息/回复支持，只有指定三参数单向完成入口可接收它，不支持比较 | 保留 `RequestId` |

`RecordInt`、`IntList`、`Bytes` 是当前受限原型的类型，不是假装已经存在的用户记录、泛型容器或字符串。三者的语言消息复制不共享源堆引用；类型化宿主 `send_values`/`request_values` 也可传入独立的记录内容、列表和字节副本，原始整数接口、JSON 和 ingress 输入仍只接收标量，不接受原堆 handle 或内部编码。RecordInt 回复复制唯一 i64 内容，IntList 回复使用独立计费结果和完成导入，Bytes 回复仍拒绝。`Pid`、`RequestId` 是不透明能力，不因为底层用标量槽承载就变成可做算术或可伪造的整数。

`Type::Function`、`Type::Message` 是分析模型中的签名表示，不是可以在源码写成 `Function`、`Message` 的通用类型；内建签名里的 `Message` 表示静态入口名位置，也不表示语言已经有消息值。

当前没有 `String`、`Float`、`List<T>`、用户定义记录类型、类型别名或联合类型。`int`、`bool`、`unit` 现阶段会作为名称解析，但在类型位置报未知类型；`INT`、`Bool` 和 `bool` 也不会自动归一为一种拼写。

### 2.3 当前名称与关键词

当前硬关键词集合为：

```text
process state on fn let if else while true false
```

- 它们不能直接用作普通声明名；大小写精确匹配，`Process` 不是 `process`。
- 标识符首字符为 `_` 或 Unicode 字母，后续允许 `_` 或 Unicode 字母/数字；目前不应声称只支持 ASCII。
- 消息名已经有语义约束：首字符必须为大写字母，不只是风格建议。`Get`、`Événement` 可用；`get`、`_Get`、`读取` 不通过该规则。
- process、fn、state、参数、局部的大小写没有同样的统一强制规则；不能把建议的 `UpperCamelCase`、`snake_case` 写成已有编译限制。
- 类型名称在类型位置单独识别，普通值绑定并不会定义或替换类型。
- 内建函数名称不是词法关键词。用户 fn 不得使用已有内建函数名；普通绑定可遮蔽内建，遮蔽后该绑定不能当函数调用。局部/参数也不能借遮蔽把静态消息目标变成消息值。
- 同一作用域重复绑定、handler 内参数/局部遮蔽 state 等已有检查应继续保留；fn 的独立作用域不能访问 state，不因参数与 state 同名就新增拒绝规则。不同声明种类目前使用的名称表并不等于一套已完成的模块/类型命名空间设计。

## 3. 大小写方案对照

| 方案 | 示例 | 优点 | 代价与边界 |
|---|---|---|---|
| A：全部类型大写，保持现状 | `Int`、`Bool`、`Unit`、`IntList`、`Bytes`、`Pid` | 所有类型视觉统一，零迁移成本 | 不回应用户希望 `int` 小写的偏好；关键词小写不构成必须改类型的技术理由 |
| **B：基础标量小写，具名类型大写，推荐** | `int`、`bool`、`unit`、`IntList`、`Bytes`、`Pid` | 最小修改；基础类型与具名抽象有清楚边界；未来 `List<int>` 外观可读 | 必须解释八个预定义类型不全是小写；需要迁移三种拼写 |
| C：所有预定义类型小写 | `int`、`bool`、`unit`、`record_int`、`int_list`、`bytes`、`pid`、`request_id` | “内建小写、自定义大写”规则直接 | 所有八种类型迁移；`record_int` 同时成为类型和构造内建名称；以后标准库类型是否算内建又需划线 |

不推荐把整个语言改为大小写不敏感。大小写不敏感会影响普通名称冲突、符号查询、Unicode、外部消息协议和迁移，远超本次需求。

方案 B 的区分是**语言层面的基础类型与具名类型约定**，不是按是否分配堆内存划线。`unit` 不等于 `void`，`Pid` 也不因为“不在堆上”就自动属于小写基础类型。

## 4. 推荐命名规则与冲突处理

### 4.1 建议规则

| 类别 | 推荐写法 | 本次是否强制新增检查 |
|---|---|---|
| 关键词与布尔字面量 | `process`、`fn`、`true` | 保留现有精确匹配 |
| 三个基础类型 | `int`、`bool`、`unit` | 经批准后作为规范拼写识别 |
| 其他预定义具名类型 | `RecordInt`、`IntList`、`Bytes`、`Pid`、`RequestId` | 保持现状 |
| 用户具名类型，未来能力 | `Order`、`OrderState` | 本次不实现类型声明，也不添加检查 |
| process 名 | `Counter`、`OrderBook` | 文档风格建议，不把现有合法名称突然判错 |
| 消息/完成事件名 | `Add`、`Get`、`Completed` | 保留已有大写首字符约束；不再额外强制完整 CamelCase |
| state、参数、局部、fn、内建函数 | `count`、`request_id`、`read_int` | 风格建议；不引入批量重命名或 lint 框架 |

继续区分大小写。`int` 是规范基础类型，`Int` 仅在获批的兼容窗口内为别名；`INT`、`iNt` 不接受。不通过字符串转小写实现别名解析。

### 4.2 关键词、类型名、值名分别处理

推荐 `int`、`bool`、`unit` 仍是普通 Identifier，在**类型位置**解析为基础类型，不新增三个词法 TokenKind：

```text
process Counter {
  state count: int = 0
  on Add(int: int) -> int { count + int }
}
```

这是建议保持可解析的示例，不是推荐把参数叫 `int`。类型位置与值位置明确，不需要为了拼写变更禁止此类已有值名。

未来引入类型声明时，应禁止用基础类型名或仍在有效期的类型别名声明新类型，避免 `record int` 一类冲突；本次没有类型声明，不提前增加 `record` 或类型命名空间实现。process、消息和普通值不能通过同名覆盖类型位置的预定义含义。

保留普通绑定遮蔽内建时不可调用的既有规则。例如当前 `let collect = 1; collect()` 会报错；这次不顺手改成所有内建名全局保留，也不发明转义调用语法。

`record`、`match`、`type`、`module`、`import`、`return`、`break`、`continue`、`async`、`await` 当前都不在上述硬关键词集合中。不为了未来可能的语法立刻把它们全部保留；以后真的引入能力时，再决定上下文关键词、冲突诊断和兼容窗口。

继续保留现有 Unicode 标识符行为和消息名检查；本次不引入大小写折叠、Unicode 正规化或 ASCII-only 限制。中文消息名受当前大写首字符规则限制，是否放宽应独立评审，不和 `Int` 改名混合处理。

## 5. 各类语法的现状与建议

### 5.1 process、state 与 on：保留骨架

保留 `process Name { ... }`、具名 `state name: Type = expression`、统一 `on` 和 `{}`。不增加 `actor` 别名，不回到 `init` / `handle_cast` / `handle_call` 回调，不让运行时内部 Rust 类型出现在业务语法里。

当前每个 process 恰好一个 state；前端可以解析、检查同文件多个 process，但 AOT 每份文件只接受一个 process。类型拼写修改不开放多 state、跨文件或跨模块。将来复合状态可通过通用记录单独设计，不能为了示例好看先把多个 state 写成可用。

state 初值可以调用本 process 的 fn 和允许的记录/列表/Bytes/GC 内建，但不能引用 state 自身、handler 参数或局部，也不能执行进程/消息内建。初值不是仅限字面量的常量表达式；执行仍受预算、栈与堆配置约束。

消息字段和回复继续显式标类型；局部 `let` 可以推导。无参数入口保留 `on Get` 与 `on Get()` 两种既有形式；推荐示例使用简短的 `on Get`，暂不强制统一，不改调用协议。

### 5.2 unit 绝不等于单向消息

当前合法写法：

```text
on Reset { count = 0 }
on Confirm -> Unit {}
```

推荐拼写：

```text
on Reset { count = 0 }
on Confirm -> unit {}
```

两者在迁移前后都必须保持以下差异：

| 入口 | 协议含义 | 块要求 |
|---|---|---|
| 无 `->` | 单向消息，无自动回复 | 结果为 Unit/unit |
| `-> Unit` 或建议的 `-> unit` | 有一次确认回复的请求 | 结果为 Unit/unit，即 `()` |

不能因返回类型没有有效业务字段就删除 `-> unit`，也不能把有值尾表达式自动丢弃来通过单向入口检查。IR 的 `reply: None` 和 `reply: Some(Unit)`、manifest、运行时路由及 LSP 悬停都应继续区分。

当前语言内建 `request` 已支持 Int/Bool/Unit/RecordInt/IntList 回复；`-> Unit` 可作为请求目标，不能作为单向发送目标。IntList 回复单次最多 64 元素；这些能力不依赖小写迁移，后续新增回复类型仍需独立的运行时与语义验证。

### 5.3 函数：使用 fn，不引入 function 关键词

当前 `fn name(parameter: Type) -> Type { ... }` 需显式参数类型、返回类型和参数括号；无参也写 `fn name()`。函数位于 process 内，只能访问参数/局部和更早声明的函数，不能访问 state、递归、执行 while 或消息副作用。当前可调用记录/列表/Bytes 与 collect 内建，不是普通 native FFI。

“更早声明”只约束 fn 之间的相对顺序：检查器先按顺序检查该 process 的全部 fn，再检查 state 初值与 handler 体。因此 state 初值和 handler 可以调用在它们之后书写的 fn，handler 也可以静态指定后面声明的消息入口；不能把它扩大成所有声明都禁止前向引用。`self_pid`、`spawn_self`、`send_self`、`send`、`request`、`cancel_request` 均只允许在 handler 内调用，fn 不能靠只读取身份或包装这些调用绕过限制。

推荐仅修改类型拼写：

```text
fn twice(value: int) -> int { value + value }
fn keep(values: IntList) -> IntList { values }
```

不同时引入返回类型推导、函数值、lambda、重载、动态派发、递归、无序前向引用或 `return`。已有逐帧栈检查、根链与静态函数数量/深度限制不变。

### 5.4 控制流与表达式：不借机改变求值模型

| 方面 | 当前支持 | 本次建议 |
|---|---|---|
| 局部绑定 | 不可变 `let`，可省略局部类型；仅 state 可赋值 | 保留；不新增 `var`、`mut` 或可变字段 |
| 块与分隔 | 换行或 `;` 分隔语句；无分号末尾表达式是块结果；空块与赋值结果为 Unit | 保留尾表达式；不强制所有行加分号 |
| 条件 | `if condition { expression } else { expression }`；Bool 条件、两臂同类型；当前臂只接受一个表达式 | 只改 Bool 拼写，不把多语句分支、缺省 else 或任意引用合流写成已实现 |
| 循环 | handler 内 state while，Bool 条件，Unit 体；局部绑定不逃逸循环体 | 保留回边强制检查；不新增 `for`、`loop`、`break`、`continue` |
| 字面量 | 十进制整数、`true`、`false`、`()` | 不因为写成 `int` 就新增浮点、字符串、整数后缀或任意精度 |
| 运算 | 取负、`* /`、`+ -`、同类型标量 `== !=`、Int 的 `< <= > >=` | 保留优先级和检查；不新增 `%`、`!`、`&&`、`||`、隐式转换或数学式链式比较 |
| 调用 | 具名函数/受限内建，参数从左到右求值 | 不新增方法调用、字段访问或用户定义运算符 |
| 注释与多行 | `#` 行注释；普通表达式完整后的换行结束语句，括号内可跨行 | 保留；不新增另一套注释或自动分号规则 |

`int` 仍是 i64，不是平台字长，也不是“不检查溢出的高性能整数”。算术溢出、除零、空列表访问等受管失败，候选 state/自动回复的提交条件、safepoint、取消和预算都不因命名改变。

需要保留的解析细节：

- 当前 parser 会把负号后紧随的整数 token 合并检查，已直接接受 `-9223372036854775808`；不必改写为 `-9223372036854775807 - 1`。正数 `9223372036854775808` 或更小于 i64 下界的负字面量报 `S004`，与运行时算术溢出不同。
- 二元运算从高到低为 `* /`、`+ -`、所有比较，均左结合；单目取负优先于二元运算。`a < b < c` 会按 `(a < b) < c` 解析，再因 Bool 参与整数比较报类型错误，不是两个比较隐式合取。
- 调用名与其 `(` 之间不能换行；调用括号内、普通括号内、if 条件/分支和 while 条件允许跨行，普通表达式完整后的换行则终止该表达式。保留已有区别，不把“小写迁移”扩展为任意位置自动续行。

### 5.5 record、容器与泛型：提出方向，不抢跑实现

当前没有 `record` 声明、字段选择、记录字面量、泛型函数或泛型容器类型。`RecordInt` 不是 `record Int` 的缩写，`IntList` 也不意味着检查器已经接受 `List<Int>`。

**建议未来方向，尚未采纳、不可编译：**

```text
record OrderState {
  count: int
  active: bool
}
```

如果后续业务值切片需要具名记录，可优先评审这一种简单声明外形：类型名大写、字段名小写、字段类型显式；值不可变，仍只替换 state 绑定。此处不规定构造、字段访问、更新、嵌套/递归布局或消息编码，不能据此实现一半语法而声称通用记录可用。

如果以后真的实现容器类型参数，偏向 `List<int>` 这样的 `UpperCamelCase<type>` 外观，而不是把当前 `IntList` 偷换为它的别名。理由是能表达嵌套类型并保持 B 方案的命名边界；但 `<`/`>` 已是比较符，类型上下文解析、错误恢复、类型表示、布局、GC 遍历与单态化策略都需独立设计。

本次明确不做：`List<int>`、`Record<int>`、`Pid<Counter>`、`RequestId<int>`、`Option<int>`、`Result<int, Error>` 或类型别名。尤其不能靠加尖括号给 Pid/request 补出尚不存在的跨模块静态协议。

字符串是后续真实业务值的重要候选，但当前没有字符串字面量或类型；不能因为 `int` 小写就默认先预留 `string`、确定其编码或承诺下一步全量实现。

### 5.6 内建与异步交互：名称不变

以下为当前内建签名的**建议新类型显示**，不是新 API；`Message`、`Completed`、`args...` 是静态入口/变长实参位置说明，不是新增源码类型语法：

| 内建 | 建议显示 |
|---|---|
| `record_int` | `(int) -> RecordInt` |
| `read_int` | `(RecordInt) -> int` |
| `collect` | `() -> unit` |
| `empty_ints` | `() -> IntList` |
| `prepend_int` | `(int, IntList) -> IntList` |
| `head_int` | `(IntList) -> int` |
| `tail_ints` | `(IntList) -> IntList` |
| `is_empty_ints` | `(IntList) -> bool` |
| `fill_bytes` | `(int, int) -> Bytes` |
| `bytes_len` | `(Bytes) -> int` |
| `byte_at` | `(Bytes, int) -> int` |
| `replace_byte` | `(Bytes, int, int) -> Bytes` |
| `read_i64_be` | `(Bytes, int) -> int` |
| `replace_i64_be` | `(Bytes, int, int) -> Bytes` |
| `send_self` | `(Message, args...) -> unit` |
| `self_pid` | `() -> Pid` |
| `spawn_self` | `() -> Pid` |
| `send` | `(Pid, Message, args...) -> unit` |
| `request` | `(Pid, Message, Completed, int, args...) -> RequestId` |
| `cancel_request` | `(RequestId) -> unit` |

其中 `read_i64_be`、`replace_i64_be` 随本地提交 `86cd40e` 在 ABI v14 时引入，当前源码已为 ABI v15；它们读取/复制替换连续八字节的有符号 i64 大端值，允许未对齐 offset；替换仍复制完整不可变对象、计费并分块检查。这里仅同步实现事实和建议显示，不表示已经发布或完成全部验证。

拼写不改变执行前提：受管堆值需要显式配置计费堆；进程/消息内建需要有相应 runtime 回调的 worker 或 Scheduler，不能因静态检查通过就声称普通 CLI 同步调用也能驱动消息协议。

消息/完成事件仍为静态具名目标。发送、创建、请求在调用点生效，失败不撤回既有副作用；state 与自动回复仅正常返回后提交。语言没有阻塞 `receive`、`await` 或同步 `call`，此次不提议改变。

当前整数请求的建议完成入口仅做拼写变化：

```text
on Completed(id: RequestId, status: int, value: int) {
  count = value
}
```

这仍是受控系统完成入口，普通发送不能伪造；不把 status 整数协议重构为联合类型，也不改变既有 RecordInt/有界 IntList/合计至多 64 KiB 的 Bytes 消息复制与跨 worker 投递边界。Int/Bool/Unit/RecordInt/IntList 请求及精确匹配的完成入口已支持；Bytes 回复和持久 RequestId 仍未开放。

`Completed` 不是关键词或固定系统名称；它可以换成其他合法消息名，并由 `request` 的第三个静态实参指定。完成入口必须无回复类型且参数依次为 RequestId、Int、T，T 精确匹配目标回复类型；已提交实现支持 Int/Bool/Unit/RecordInt/IntList。参数名不固定；不能只靠把普通入口命名为 `Completed` 获得系统事件身份。

## 6. 兼容、迁移与版本取舍

### 6.1 别名策略对照

| 策略 | 结果 | 建议 |
|---|---|---|
| 立即只接受小写 | 规则最少，旧 `Int/Bool/Unit` 源码立即失败 | 仅在用户明确愿意一次性断开源码兼容、同步迁移全部调用方时采用 |
| 临时精确别名 | 新写法规范化为小写，旧三种拼写仍可编译，之后在明确版本移除 | **推荐**，但兼容截止点也需要确认 |
| 永久接受两套 | 历史源码永不因这三种拼写失败 | 不推荐；永久双拼写增加教学、工具显示和未来类型名冲突成本 |
| 任意大小写兼容 | `Int`、`INT`、`iNt` 都接受 | 不采用；不把拼写迁移扩展成大小写不敏感语言 |

推荐兼容流程：

1. 用户批准具体规则后，在开发分支新增 `int/bool/unit`，`Int/Bool/Unit` 仅作为三对精确别名，映射至同一语义类型；其他五种类型保持原名。
2. 新文档、示例、补全和悬停使用规范小写。常规补全不同时塞入两套同义标签；旧源码继续可以检查和构建。
3. 到下一次**明确公布的源码不兼容版本**，重新取得移除别名的确认，先提供迁移说明，再删除三种旧拼写。若没有指定版本，不偷偷按提交数或日期删除。

兼容期先不为旧写法新增告警：当前诊断结构只有错误，AOT lowering 会拒绝任意非空诊断集合；直接把“弃用警告”塞进去反而破坏兼容。要做 severity 或代码修复，应独立实现并验证，不作为本次必需项。

### 6.2 迁移必须按类型位置，不全局替换

- 只替换 state 类型、消息参数/回复类型、fn 参数/返回类型以及显式局部类型标注中的三个名称。
- 不改 process、消息、fn 或普通值名；不改注释、字符串形式的外部协议、JSON manifest、Rust 枚举、目录名和已采纳历史 ADR 中的当时写法。
- 类型查找与 lowering 使用同一规范化规则；迁移后既要 `check` 也要 `build`，不能以静态通过代替运行验证。
- 没有迁移工具时先人工修改仓库示例与测试；需要工具再做基于 AST/span 的定向编辑，不用全仓库 `Int` 文本替换。

### 6.3 源码版本与二进制 ABI 分开

当前工具没有本稿可直接使用的语言 edition 选择机制，不编造 `--edition`、`syntax_version` 或新的配置键。

仅改源码类型别名/规范显示，在类型枚举、布局、manifest schema 和受管调用约定不变的前提下，**没有理由仅为拼写提升当前 ABI v15**。Rust 内部 `Type::Int`、`ValueType::Int`、序列化标签和运行时错误编码无需跟着改名。若实现过程实际改变了 ABI 或 manifest，则应拆出变更、按相应契约单独评审。

当前源码与已发布 `0.0.2` 本来就有 ABI 差异；旧安装版也不会因为仓库文档改成小写就获得新解析能力。迁移说明应写明需要支持该源码语法的编译器；与 runtime 的兼容仍按现有 ABI 检查，而非靠类型拼写猜版本。本文不请求发布任何包。

## 7. 经批准后的实现影响与验收

下表是后续任务清单，不是本文已完成的修改。

| 层次 | 必要工作 | 不应附带的改动 |
|---|---|---|
| Lexer/parser/AST | 保持类型为 Identifier/Name；增加规范小写及别名的解析回归，保证 span、错误恢复不退化 | 不新增基础类型关键词、通用 TypeExpr 或泛型 parser |
| 语义检查 | 在 `Checker::declared_type` 精确识别规范名与获批别名；同步 `Type::fmt`、未知类型提示及硬编码的请求诊断；现有合法位置/外部边界不变 | 不把 lowercase 当作新类型、不扩大现有消息类型边界或新增隐式转换 |
| AOT lowering | 同步 `value_type` 对所有类型标注的匹配；优先复用前端的单一名称识别入口或已解析类型，避免两份集合再次漂移 | 不让 VM 依赖前端，不为两个匹配函数引入新的聚合 crate |
| IR/codegen/ABI | 验证两种拼写降成相同类型/操作、回复协议和根计划；理论上无需语义修改 | 不改 `ValueType` 序列化名称，不改变帧布局、safepoint 或 GC |
| LSP | 类型补全、内建签名和悬停统一小写基础类型，符号名和位置保持原样；旧源码仍能查询 | 不宣称已有格式化、重命名、语义 token 或 code action |
| 示例与主题文档 | 迁移现行示例/契约中的类型拼写，标注需要的新工具链；检查安装版示例的版本适用性 | 不回写历史决策理由，不把计划中的能力写成可编译示例 |
| ADR/版本说明 | 若决定改变现行语言拼写规则，新增相应选择记录，保持历史 ADR 与索引可追溯；声明别名窗口 | 不把本草案直接升格为已采纳 ADR，不借此自动发布 |

诊断不能只依赖 `Type::fmt` 自动更新：[语义检查器](../../crates/wings-sema/src/lib.rs) 的 `T002` 支持类型清单，以及当前工作区 `T018` 的 `request requires an Int, Bool, Unit, RecordInt or IntList reply handler`、`request completion requires one-way (RequestId, Int, reply type)` 都是硬编码文本；诊断已同步接受的类型集合，不能只修改类型显示而遗漏这些文本。批准 B 后只把其中三种基础类型的说明统一为规范小写，保留其他具名类型、诊断码与 span；未知名称仍按用户输入原样引用，不把普通符号名中的 `Int` 改掉。相关断言在[语义测试](../../crates/wings-sema/tests/analysis.rs)、[lowering 单元测试](../../crates/wings-compile/src/lower.rs)、[编译器集成测试](../../crates/wings-compile/tests/compiler.rs)和 [LSP 协议测试](../../crates/wings-lsp/tests/protocol.rs) 中按含义更新；兼容期有意保留旧拼写的用例不能一并删除。

LSP 现有内建类型不是可跳转到源码声明的用户类型。本次至少保证参数、局部、state 和函数等符号的悬停显示一致，不把“更新类型名称”说成实现了内建类型定义跳转。

还需处理合法同名值的补全冲突：当前补全不按类型/值位置过滤，先插入关键词、类型和内建，再用可见符号按同一 `label` 覆盖。迁移后 `let int: int = 1` 会使局部项覆盖类型项，不能仅把补全数组里的 `Int` 改成 `int` 就视为工具支持完成。后续实现应让类型位置仍能取得规范类型项、值位置仍能识别真实绑定，可采用上下文过滤或可辨认的不同种类条目；不靠禁止/重命名合法值来消除冲突，也不要求借此实现完整上下文补全框架。

### 7.1 针对性验收矩阵

1. **八类型集合**：三个规范小写类型通过；`RecordInt/IntList/Bytes/Pid/RequestId` 保持合法位置和原名。兼容模式下旧三名通过；`INT`、`iNt`、`recordint` 等仍拒绝。
2. **全部类型标注位置**：分别覆盖 state、局部 let、消息字段、消息回复、fn 参数、fn 结果；同时覆盖省略局部类型，以及同一程序中新旧拼写混用的赋值、调用和返回，不出现 check/build 规则不一致；AST 保留真实拼写和 span，语义类型一致。
3. **名称冲突**：`let int: int = 1` 一类值/类型同名不被误伤；既有关键词仍拒绝作普通名；内建遮蔽、重复声明、静态消息目标遮蔽及 Unicode/span 行为保留。
4. **协议差异**：无 `->` 与 `-> unit` 在语义签名、IR、manifest、LSP 和宿主调用中仍不同；单向入口尾值非 unit 仍失败。语言 request 仍拒绝不支持的回复种类。
5. **类型边界**：保持 RecordInt/有界 IntList/合计至多 64 KiB 的 Bytes 语言消息复制，以及已支持的 Int/Bool/Unit/RecordInt/IntList 请求与精确匹配的完成入口；有界列表回复的结果所有权、预算与完成导入仍须保留回归，拼写迁移不得将其恢复为一律拒绝。Bytes 回复仍拒绝；不因拼写变化允许 Pid/RequestId 进入 state/普通回复，指定完成入口的形状和不可伪造规则保留。
6. **语义等价**：取同一份仅在类型标注拼写不同的程序，比较归一后的 IR 类型/操作、manifest 协议和实际状态/回复/失败结果。不要求两个本机产物字节完全相同。
7. **受管运行**：用记录、列表、Bytes（含大端读写）、fn、while、send、request 现有回归覆盖取消、预算、GC 根、正常返回提交与失败不撤回副作用，不能只跑 counter 的成功路径。
8. **编辑器**：规范补全不重复提供类型别名、内建签名一致、旧拼写源码的悬停显示规范类型且保留真实符号名；覆盖同名 `int/bool/unit` 值的类型/值位置补全，不能因按 label 去重丢掉类型项或真实绑定；UTF-16、未完成类型、旧版本更新与诊断恢复不退化。
9. **兼容退出**：如果批准直接断开兼容或后续移除别名，为旧三种类型提供精确位置及建议替换的诊断；不通过大小写模糊匹配意外接受其他名称。
10. **诊断显示**：未知类型的支持清单、类型不匹配、请求回复及完成入口形状错误都显示规范类型名；兼容期旧源码发生错误时也如此，但未知名称、真实符号名和定位不被规范化改写。

### 7.2 后续实现的验证命令

本次只写草案，不运行或宣称通过以下代码验证。若批准实现，先跑最小范围，再跑仓库门禁：

```sh
cargo test --locked -p wings-syntax -p wings-sema -p wings-compile -p wings-lsp
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
python3 scripts/verify_scalar_aot.py
cargo test --release --workspace --locked
cargo build --release --locked -p wings-compile -p wings-vm --bins --examples
WINGS_PROFILE=release python3 scripts/verify_scalar_aot.py
git diff --check
```

文档另查相对链接、目录、围栏和版本适用性。AOT 验证沿用脚本已有的有资源限制 OS 子进程，不在宿主注入崩溃或无界资源耗尽。最新实现仍须分别通过 macOS AArch64 与 Linux x86-64；一次本机通过不是双平台通过，更不是同负载 BEAM 性能/内存达标。

## 8. 建议分阶段推进

| 阶段 | 产物 | 是否现在执行 |
|---|---|---|
| 0：本稿评审 | 确认大小写方案、别名窗口、约定是否强制 | 只提供本稿，等待用户意见 |
| 1：最小类型拼写切片 | 若批准，统一 sema/lowering/LSP，迁移示例，补边界与兼容回归，更新契约与选择记录 | 未批准前不做；不要求主线 MVP 开发暂停 |
| 2：业务值需求驱动设计 | 只有真实业务负载需要时，再独立评审字符串、具名记录及其根/消息/计费契约 | 不与阶段 1 捆绑 |
| 3：可选语言扩展 | 泛型、联合类型、模式匹配、模块等逐项论证 | 不作为此次改名或 MVP 完成的默认前提 |

当前路线图仍是[先补执行、资源与业务验收边界](../roadmap.md)。类型改名不完成 M0，也不能代替公平调度、真实外部事件、恢复、双平台和同业务 BEAM 吞吐/内存门禁。主线实现应继续推进，不等本稿未决的可选语法。

## 9. 请用户确认

1. **类型大小写**：是否采用推荐 B，即只把 `Int/Bool/Unit` 改成 `int/bool/unit`，其他五种具名类型保留？还是希望 A 保持现状或 C 全部预定义类型小写？
2. **源码兼容**：是否接受临时三对精确别名？推荐窗口截止于下一次明确公布并确认的源码不兼容版本；如果希望立即删除旧拼写，请明确选择硬切换。
3. **命名强制程度**：是否同意 process/type/消息大写、值/fn 小写下划线先作为风格约定，除消息现有大写约束外不新增拒绝规则？
4. **范围**：是否同意这轮只做类型拼写，不改变 `process/state/on/fn`、Unit 请求与单向语义，不顺带实现 record、泛型或新的控制流？

可后续再定、不阻塞最小改名的问题：中文消息名是否放宽、通用记录的构造/访问外形、列表与字符串正式命名、未来版本/edition 机制。它们不应在批准小写类型时被默认为已经同意。

## 10. 本次改动边界

本次只审核并更新 `docs/proposals/language-syntax-review.md` 中的实现事实及相应迁移/验收边界，保留其未跟踪、待用户评审地位。未修改语言实现、现行示例、现行主题契约、ADR 或文档入口，未暂存或提交；仓库中其他未提交改动属于并行的主线工作，不是本稿的修改。
