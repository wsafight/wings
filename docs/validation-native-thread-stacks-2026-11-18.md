# 宿主线程栈启动边界验证（2026-11-18）

按 [ADR-0138](adr/0138-native-thread-stack-startup-boundary.md) 推进 MVP 资源账本，基于 `c8d743d`。本机 macOS 26.6.2 AArch64、M3 Pro、36 GiB，Rust 1.99.1、MSRV 1.85；没有原生 Linux。ABI v22、4352 B 执行控制、worker/timer 请求栈和全部既有资源/CPU/墙钟限制不变。

## 缺口与修正

原 Scheduler worker 预留每线程 2 MiB+64 KiB，另计本地活栈队列；timer 预留 256 KiB+两个系统页+4096 B 控制。Builder::stack_size 只是请求，先前未核对平台最终可用范围。新增线程内原生查询及局部地址验证，只允许报告范围落在原已准入栈桶中；超限、零/溢出/不含当前局部、查询错误都在启动成功前拒绝，不调整额度。

worker 使用原启动通道，检查在按需栈池/timer/队列创建前。timer 在原固定控制余量内保存一次性结果，沿既有 mutex/condvar 等待启动；不新增 channel 或线程。拒绝时先 join，再释放原线程/控制票据，不能让父构造成功或执行受管业务。

macOS 使用 pthread_get_stackaddr_np/stacksize_np；Linux 使用 pthread_getattr_np/attr_getstack，成功初始化后无论读取是否成功都销毁 attributes。仅验证 pthread 报告的可用范围；Linux libc 查询内部可能另有暂存，不承诺零分配。这是 OS 创建后的核对，不是所有原生内存的分配前硬限额；guard、TLS、原生控制/缓存、allocator 元数据和完整 mapping/RSS 仍不能据此宣称闭环。

## 首次失败与原生实测

首次实现将上限误设为请求本身，而非已有栈桶，定向测试有两组 exit 101：原生 256 KiB/2 MiB 测试与 Scheduler 注入失败后正常再启动测试；timer 注入拒绝的清理通过。全部双流、结果、原失败 harness 副本保留在 `target/native-stack-focused-debug-initial-20261118/`，SHA 为 `df9b9111eb46fcee608d19c8e21e748ec376bc32a2644c67232a39536a9237cc`。没有将该失败说成现存 VM 提前退账或已复现 OOM。

进一步在相同原限子进程打印原生结果：请求 262144 B，报告 274432 B；请求 2097152 B，报告 2109440 B。两者均多 12288 B，小于原本两页（32768 B）/64 KiB 栈余量。诊断记录 `target/native-stack-size-diagnostic-20261118/{stdout,stderr,both-sizes.stdout,both-sizes.stderr,reported-sizes.stdout,reported-sizes.stderr}`；前两轮只跑到首个断言失败，最后一轮才完整记录两种大小。更正检查边界为已有栈桶，没有增加或转嫁额度，也不猜测多出的 12 KiB 是哪个 guard/TLS/控制结构。

原生源码检索使用本机已安装 Rust 标准库作旁证：平台可按最小栈要求调整显式请求。另一次读取 Apple 上游 pthread 源码的网络请求遭连接重置，未取得内容，不作为证据。没有改签名、扩展属性、系统服务或安全设置。

## 定向与全量状态

修正后 Debug/Release 各五项定向测试通过：三个范围/原生测试、timer 拒绝与一/双 worker 拒绝后恢复。冻结 harness、副本 SHA 和双流保留在 `target/native-stack-focused-{debug,release}-final-20261118/`。其后另补部分 worker 正常、第二 worker 拒绝时的 sibling/timer/pool 清理测试，最终通过情况以完整 workspace 为准。

最终 Debug/Release workspace 各 106 harness/713 项测试通过，报告 `target/native-stack-workspace-{debug,release}-20261118/report.json`。比基线增加六项，包含部分 sibling 拒绝及原 deadline timer 重设/取消/销毁回归。八项原子进展模型、八项 workspace 与六项 AOT 运行器自检通过，日志 `/tmp/wings-native-stack-python-20261118.log`；自测中故意构造的失败报告不是实际 workspace 失败。232 个文档/Python 文件的相对链接、围栏与 AST 检查通过，`target/native-stack-static-20261118/report.json`，未读取用户 `docs/proposals/`。

本轮 Debug 完整 AOT 已通过原子进展和调度诊断，但在后续 TCP normal 场景原 5 秒握手期限失败：elapsed=5.012s、清理前活跃、清理 exit=-9、双流前缀为空。日志 `/tmp/wings-native-stack-full-aot-debug-20261118.log` 保留，未为本切片重跑至全绿。顺序 Release 完整 AOT 通过，日志 `/tmp/wings-native-stack-full-aot-release-20261118.log`；不覆盖 Debug 失败，也不把该栈检查称为原[启动超时](validation-startup-followup-2026-11-18.md)修复。

`cargo check --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、fmt、MSRV 1.85 all-targets、Linux x86-64 all-targets 交叉检查和 Debug/Release doctest 通过，日志 `/tmp/wings-native-stack-{check,clippy,fmt,msrv,linux-cross,doctest-debug,doctest-release}-20261118.log`。工程命令在 Release AOT 结束后才启动，交叉检查不是 Linux 原生执行。依赖/unwind 守卫通过，证据保留在 `target/native-stack-preflight-20261118/`；最终完整指纹清单为该目录的 `report.json`。

复现命令沿用受限运行器：

```sh
cargo build --workspace --bins --examples --locked
cargo build --release --workspace --bins --examples --locked
python3 scripts/verify_workspace.py --output target/native-stack-workspace-debug-new
python3 scripts/verify_workspace.py --profile release --output target/native-stack-workspace-release-new
WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
WINGS_PROFILE=release WINGS_PREBUILT_EXAMPLES=1 python3 scripts/verify_scalar_aot.py
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo +1.85.0 check --workspace --all-targets --locked
cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu --locked
```

## 剩余验收

本切片只补运行宿主线程的一个可用范围闸门，没有完成完整原生账本或测量内存优化。TCP 适配线程、装载器/libc/allocator 内部成本、启动稳定性、服务/公平/过载长测、原生 Linux/最新双平台，以及同机同业务 BEAM 吞吐/整机内存双门禁仍待完成，MVP 未完成。
