# 完整 Bytes TCP 同业务验证（2026-11-19）

按 [ADR-0140](adr/0140-bytes-tcp-service-equivalence.md) 推进真实服务对照，基于 `3f3ba69`。没有改变 ABI v22、4352 B 执行控制、32 MiB VM 上限、原 8 连接/64 帧/mailbox 1/ingress 2/request 4、50ms 请求、100ms 读写、2s accept、5s 握手或 CPU 20s/core 0 等原期限。

## 功能与协议

tcp_counter 旧 Int 模式不变，显式 `--state-bytes 256|4096|65536` 使用独立 `examples/tcp-bytes-counter.wings`。完整 state 前 8 B 为 signed i64 BE 计数，随后 identity=1，剩余零填充；每次 checked +delta 后构造候选，overflow 或候选后失败不提交。配置与健康最终 Read -> Bytes 用原有限请求和三秒内部期限/4096 总预算/64 步片；完整回复逐字节校验。失败只核对现有长度/32 B 已提交前缀，明确 `full_state_verified=false`。

独立 Python 驱动逐条生成分片/流水线、跨连接、候选失败、上溢/下溢、非法 sequence/operation/reserved 和 8×64 最大请求。每种完整 state 共七种业务；输入十六进制、命令、双流、逐条回复、状态/清理及实际 socket 观测均保存 JSON。显式 `--compare-beam` 使用真实 OTP 29 和发送前有限 gate，缺少 escript 直接失败，不回退模型。BEAM 以完整不可变 binary 保存同业务 state；失败只发提交前缀，健康完整读取并停止 actor/关闭 gate。两端要求零超时才认可这项正确性，不宣称完整超时/取消/过载等价。

Wings 原 socket 设置/每次 I/O 后缓冲上限/MSS 核对保留；BEAM 对 listener 和接受 socket 显式设置 sndbuf/recbuf=16 KiB，macOS 先设 MSS=1024 再设缓冲，每次读写后观察并拒绝超过 64 KiB。用户态逻辑/语言信封/内核分配仍不等价于已完成内存计量。

## 首次失败保留

- 初次新增 Wings 256 B 服务在原五秒握手期限失败，elapsed=5.007s、清理前活跃、exit=-9、双流空。原 `target/tcp-bytes-initial-20261119/wings.json` 与 `/tmp/wings-tcp-bytes-initial-20261119.log` 保留，没有归因 Bytes/清单或宣称旧启动超时已修复。
- 初次 BEAM 接受 socket 没有重新配置继承的缓冲，边界检查拒绝并连接 reset；stderr 的 badmatch false 和双流保存于同目录 `beam.json`，清理时 exit=-9。改为显式接受端设置后首 256 B 四请求通过，不放宽原 64 KiB 上限。
- 首三尺寸矩阵进一步在 BEAM 65536 B 最大连接场景六个连接后再次触发 badmatch false；原报告 `target/tcp-bytes-matrix-initial-20261119/` 保留，已成功的 41 条记录与失败文件不覆盖。随后把接受端设置顺序与 Wings 对齐为先 MSS、后缓冲，并为缓冲错误输出具体 sizes；4096 B 最大 512 请求定向通过，最大观测 32768 B。原诊断只有 badmatch false，不能追溯断言确切哪次检查或缓冲数值超过上限，不能把对齐设置顺序当作已证明的唯一根因。

## 验证状态

六项 Python 模型/报告突变测试通过。首次修正三尺寸显式对照 42 个服务运行通过：Wings/BEAM 各 21 个实例、54 个连接、1581 个已发布请求；每帧/终态符合模型，健康全量 state 和失败旧提交前缀核对、根退账/actor 停止通过，最大 socket 缓冲均为 32768 B。报告 `target/tcp-bytes-matrix-final-debug-20261119/report.json`；此轮在新增 Rust 完整尾部突变单测/helper 重新构建前使用既有新模式 binary，只作为阶段证据，最终完整源码另行验证，不冒充最终回归。

最终源码顺序重建两种构建后，各 107 harness/719 项 workspace 测试通过，比基线增加完整尾部突变/截断两项。最终冻结 Debug/Release 显式 Wings/BEAM/Python 对照各 42 次服务运行通过，合计 84 次；每种构建每端均 21 个服务、54 个连接和 1581 个发布请求，最大实际 socket 缓冲 32768 B，健康状态完整校验/失败旧提交前缀、232 B 根退账/BEAM actor 停止均通过。每构建另六组非法模式在启动前明确拒绝。冻结可执行、独立产物、逐业务 JSON/指纹位于 `target/tcp-bytes-validation-20261119/`，总命令/双流日志指纹见 `compare-reports.json`，逐输入见 `compare-{debug,release}/report.json`，已再次核对每个记录 SHA。

workspace check、all-target clippy `-D warnings`、fmt、MSRV 1.85 all-target、Linux x86-64 all-target 交叉检查、两种构建 doctest、六项新模型、八项 workspace/六项 AOT 运行器及八项原子进展自检通过。命令/退出/日志 SHA 见同目录 `report.json`；238 个文档/Python 的链接/围栏/AST 与依赖/禁 unwind 守卫通过，执行输入 280 文件指纹另存。首批总运行器因试图新建已存在的定向构建日志而在运行 Cargo 前退出，换独立 final 日志前缀后执行上述检查，没有覆盖旧日志。Linux 仍是交叉检查，不是原生验证；本轮没有调用远端 CI。

Debug 完整 AOT 在旧 Int TCP normal 原五秒握手期限再次失败：elapsed=5.008s、清理前活跃、exit=-9、双流前缀空，尚未到新增 Bytes 服务入口。日志 `/tmp/wings-tcp-bytes-full-aot-debug-20261119.log` 完整保留，不用专项/其他构建通过覆盖该失败，也不为本切片重跑至全绿。逐构建结果与日志 SHA 见同目录 `full-aot-reports.json`。

顺序 Release 完整 AOT 通过，覆盖原 Int TCP 九场景、新 Bytes 三尺寸 21 场景以及原语言/根/调度/故障回归；默认该入口未运行 BEAM，真实 OTP 证据仅取上述显式专项。日志 `/tmp/wings-tcp-bytes-full-aot-release-20261119.log`。执行输入在最终两种构建、专项及完整 AOT 后再次按 280 项 SHA 逐一核对，没有变化；没有并行构建/性能长测干扰这些业务验证。

## 复现入口

```sh
cargo build --locked --workspace --bins --examples
target/debug/wings-compile build examples/tcp-bytes-counter.wings -o target/tcp-bytes-new
python3 scripts/verify_tcp_bytes_tests.py
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_tcp_bytes.py --artifact target/tcp-bytes-new --output target/tcp-bytes-results-new --compare-beam
python3 scripts/verify_workspace.py --output target/tcp-bytes-workspace-debug-new
python3 scripts/verify_workspace.py --profile release --output target/tcp-bytes-workspace-release-new
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

完整资源/装载账本、原启动稳定性、公平/过载/服务长测、CPU 硬配额、原生 Linux/最新双平台，以及同机同业务 BEAM 吞吐/整机内存双门禁仍待完成，MVP 未完成。没有推送、发布或读取用户 `docs/proposals/`。
