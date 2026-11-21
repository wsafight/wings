# 失败长测异常来源诊断验证（2026-11-19）

按 [ADR-0145](adr/0145-bounded-failure-soak-tracebacks.md) 补充第 6 项失败取证，基于 `f25e7ff`。本切片只修改 `verify_failure_soak.py` 的顶层失败报告，不修改 VM、业务协议、资源限制或期限。

## 触发证据

旧 push 流水线 [`44477376615`](https://github.com/wsafight/wings/actions/runs/44477376615) 的 macOS `Verify prebuilt AOT test entry point` 在 2026-11-19T07:13:22Z 失败：`verify_failure_soak.py` exit 3，stderr 只有 `Failure soak verification failed: [Errno 1] Operation not permitted`。同提交 PR 流水线 `44477382518` 通过；两种结果都保留。旧日志没有异常类型以外的调用位置，不能断定 EPERM 来自 `setrlimit`、`killpg`、`ps` 或其他路径。

## 自测

`verify_failure_soak_tests.py` 三项覆盖：权限异常保留类型、消息和实际 raise 位置；异常因果链保留；多字节 UTF-8 的任意长消息按 8192 B 截断并保留末尾标记。首轮一项断言错误地要求调用方测试名出现在异常实际 raise 点之外，改为直接在被测方法中 raise 后，又将遗留的源码文本断言改为实际的 `raise PermissionError`；最终三项通过。这些测试编写失败不是生产或验证目标失败。

`python3 -m py_compile scripts/verify_failure_soak.py scripts/verify_failure_soak_tests.py` 通过。原 `python3 scripts/verify_failure_soak.py` 真实 Wings/Python 短回归通过：三种 Bytes state 各三轮、34 组伪造历史、四种非法负载、trace 独占及失败驱动证据均保持，BEAM 未执行。提交 `8089bae` 的 push 流水线 [`44540323324`](https://github.com/wsafight/wings/actions/runs/44540323324) 在 macOS、Linux、MSRV 全绿；PR 流水线 [`44540333787`](https://github.com/wsafight/wings/actions/runs/44540333787) 的 Linux/MSRV 成功，但 macOS 未执行任何步骤即被取消，整 run 为 failure，未重跑或省略该差异。本机没有复现上述 EPERM，因此没有生产修复或系统设置调整。

## 范围

新增 traceback 只改善下一次失败的定位证据，不关闭完整并发故障、长时过载/真实服务 I/O、公平性、完整账本、CPU 硬配额、原生双平台或 BEAM/MVP 门禁。
