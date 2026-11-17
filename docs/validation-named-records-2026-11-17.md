# 2026-11-17 具名记录验证

## 范围

基于 `fbd4419`、分支 `feat/mvp`，按 [ADR-0108](adr/0108-bounded-named-record-values.md) 接入本地/state/fn 具名不可变记录。源码 ABI v20，旧产物重编译；context 88 B、EffectRequest 80 B、4352 B 执行控制预留和原资源上限不变。未修改或暂存待审语法提案。

本机 macOS 26.6.2/Darwin 25.6.0、AArch64 Apple M3 Pro、36 GiB、Rust/Cargo 1.99.1，MSRV 1.85.0。独立测试/AOT 子进程 CPU 20s、墙钟 30s、core 0，业务阶段 3s、TCP 启动等待 5s。没有最新原生 Linux x86-64 结果或 CPU 硬配额，worker 数不当作配额。

## 实际验证

- 解析/语义/lowering 接受空记录、8 字段、32 schema、嵌套业务值和函数/分支/循环；拒绝同形错名义类型、错误参数、重复字段、递归/前向/能力字段、字段赋值、记录比较及具名记录消息/回复。`record` 不成为保留字，旧局部/状态/参数/字段名仍可使用。
- ABI、IR 与清单独立拒绝伪造类型索引、构造类型/数量、字段索引/结果和外部签名；LSP 保留记录类型悬停及未知字段诊断，未加入记录类型/字段的完整跳转或补全。
- runtime 同形布局保持独立，拒绝错布局/错标签/陈旧代次及非规范 Bool，失败不写输出、不改变已提交根；嵌套引用通过 GC 保活。布局表容量先计费，创建不足和无效 schema 回到原账目；原 Rust 堆 API 的异堆身份检查继续保留。受信任 ABI 局部编码不是可传输的跨堆身份。
- 新真实 AOT `named_records` 覆盖全字段类型、i64 两端、Unicode、跨帧/分支/循环、逐步恢复/重复 GC、初始化/执行堆不足、预算/取消清理、单/双 worker、隔离失败和一次全新监督重启及强度上限；CLI 检查标量及嵌套 String 字段回复。每阶段三秒，未放大槽数上限或控制预留。
- 新可信 Rust ABI 回归在错名义候选、受管失败、非法 Bool 回复后三种路径直接核对旧 state/root，GC 后只剩原记录且内容仍为 7；真实 AOT 另覆盖源码候选的失败与退出。

## 命令与结果

```sh
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.85.0 check --workspace --all-targets --locked
cargo build --locked -p wings-compile -p wings-vm --bins --examples
cargo build --release --workspace --locked --bins --examples
cargo test --workspace --all-targets --locked --no-run --message-format=json
cargo test --release --workspace --all-targets --locked --no-run --message-format=json
cargo test --workspace --doc --locked
cargo test --release --workspace --doc --locked
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
```

`--no-run` 仅生成清单；随后实际逐个运行各 harness，独立子进程保留上述限制。debug/release 各 93 harness、642 项通过、失败 0，VM 各 302 项；doc-test 实际各 0 项。check/clippy/fmt/MSRV 和两种构建通过，新的具名记录 AOT 两种模式各连续五轮通过。依赖隔离/协程禁 unwind、Python AST、相对链接/围栏和 `git diff --check` 通过。

日志：`/tmp/wings-named-{check,clippy,fmt,msrv}-final.log`、`/tmp/wings-named-tests-{debug,release}.log`、`/tmp/wings-named-harness-{debug,release}.jsonl`、`/tmp/wings-named-aot-focused-repeat.log`、`/tmp/wings-named-doctests-{debug,release}.log`。最终完整 AOT debug 见 `/tmp/wings-named-aot-debug-repeat.log`，release 见 `/tmp/wings-named-aot-release.log`，两者通过。

## 失败证据

首轮前端测试因新增保留字 `record` 破坏旧局部名，25 项通过、1 项失败；改为仅声明位置识别后前端回归通过，未改旧示例来掩盖不兼容。新增 fixture 编译时出现借用期间移动 process，以及测试调用私有 collect；分别按先释放 execution 和经原 heap callback 收集修正，不放宽运行契约。

debug 首次完整 AOT 在既有 TCP 场景五秒启动等待失败，保留 `/tmp/wings-named-aot-debug.log`。首次专项诊断误用普通 Counter，返回 `unknown handler Apply`，见 `/tmp/wings-named-tcp-{debug-focused,startup-debug}.log`，这是诊断输入错误，不是 TCP 根因证据。改用原 `tcp-counter.wings` 后完整 TCP 专项原参数通过，见 `/tmp/wings-named-tcp-debug-corrected.log`；随后完整 debug 原限制复跑通过，release 完整 AOT 首轮通过。没有捕获首次启动失败的子进程位置，不以复跑通过或同时存在其他测试推定根因已解决，也没有增加超时。

## 剩余门禁

本轮补的是最小记录业务值，不是完整 MVP。具名记录跨进程传输、全部增长/启动/装载资源账本、长时公平/过载/并发故障恢复、最新原生双平台以及同机同负载 BEAM 吞吐/整机内存双门禁仍未完成；不将逻辑预留或局部回归视作 RSS/性能结果。不推送、发布或绕过 main 保护。
