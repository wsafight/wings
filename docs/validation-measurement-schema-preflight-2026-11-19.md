# 长测产物 schema 前置核对验证（2026-11-19）

按 [ADR-0146](adr/0146-measurement-artifact-schema-preflight.md) 推进第 5/7 项实验输入有效性，基于 `8089bae`。本机 macOS 26.6.2 AArch64；只修改 Wings 的 open-loop/fairness 运行端前置检查和两个有界验证 harness，不执行 90 秒长测。

## 开发失败与修正

- 首次定向 `cargo check` 误用私有的 `wings_vm::ValueType`，两个 example 编译失败；改用既有 Counter 同款 `wings_abi::ValueType` 后通过。这是接入错误，不是 VM 缺陷。
- fairness 首轮错误产物回归在 Configure 核对前查找 Cpu，实际 stderr 为 `unknown handler Cpu`，未达到固定 schema 诊断。将 state/Configure 核对置于 Cpu/Pulse 查找前后，真实 `beam-counter.wings` 以 `fairness artifact schema mismatch` 拒绝；随后完整 fairness harness 通过。
- Debug 与 Release 的 `verify_measurement.py` 首次执行都在进入新增用例前遇到新编译原生内存探针超过原 2 秒期限，stdout 只到 Memory model，exit 3。未修改期限或把该次记为通过；后续执行完成全部 harness，Release 第二次为顺序执行。它与既有探针首执行不稳定性同类，但不据此断言系统根因。

## 最终定向结果

两种构建的 `verify_measurement.py`、`verify_fairness_measurement.py` 均通过。新增真实错误产物用例均得到 exit 3、`status=oracle_error`（version 分别为 6/8）和固定 schema stderr，trace 存在且为 0 B，没有 `start`/`sampling` 或 90 秒业务。

两种构建的原 `verify_open_loop.py` 与 `verify_fairness.py` 正确产物短回归也通过：open-loop 六组调度及配置拒绝、fairness 10ms/30ms 百万 checked 加法与同刻心跳、原状态/清理核对保持。压力短测的拒绝数量按实际结果保留，不作性能或长测结论。BEAM 未在这些默认入口执行。

`cargo check -p wings-vm --example beam_open_loop --example beam_fairness`、两种构建的相应 example 构建、Python 语法与 `cargo fmt --all -- --check` 通过。`cargo check --workspace --all-targets --locked`、clippy `-D warnings`、MSRV 1.85 all-target check、`git diff --check` 与 Release 完整 `verify_scalar_aot.py` 通过；双平台 CI 结果另补。

## 范围

本切片只让错误实验输入在 actor 创建和业务计时前明确失败，不重跑或改写既有长测，不改变 mailbox_full 判定。完整公平/过载/服务故障、CPU 硬配额、完整资源账本、原生双平台和 BEAM 吞吐/整机内存双门禁仍未完成。
