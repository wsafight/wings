import argparse
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import subprocess
import sys


SCRIPT = Path(__file__).with_name("beam_workload.escript")
MAX_INPUT = 65536
MIN_I64 = -(2**63)
MAX_I64 = 2**63 - 1


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def invoke(payload):
    escript = shutil.which("escript")
    if escript is None:
        raise RuntimeError("Erlang/OTP 29 escript is required")
    environment = os.environ.copy()
    for name in ("ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "ERL_LIBS"):
        environment.pop(name, None)
    environment["ERL_CRASH_DUMP"] = os.devnull
    environment["ERL_CRASH_DUMP_SECONDS"] = "0"
    child = subprocess.Popen(
        [escript, str(SCRIPT)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        start_new_session=True,
        preexec_fn=limit_child,
    )
    try:
        stdout, stderr = child.communicate(payload, timeout=10)
    except BaseException:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.communicate()
        raise
    if len(stdout) > 65536 or len(stderr) > 65536:
        raise RuntimeError("Oracle output exceeds 64 KiB")
    if stderr:
        raise RuntimeError(f"Oracle stderr: {stderr.decode(errors='replace')}")
    try:
        result = json.loads(stdout)
    except (ValueError, UnicodeError) as error:
        raise RuntimeError(f"Invalid oracle output (exit {child.returncode}): {stdout!r}") from error
    return child.returncode, result


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def config(initial=0, requests=(), state_bytes=256):
    return {
        "version": 1,
        "state_bytes": state_bytes,
        "initial": initial,
        "requests": [{"delta": delta, "fail": fail} for delta, fail in requests],
    }


def reference(workload):
    committed = workload["initial"]
    replies = []
    for sequence, request in enumerate(workload["requests"], 1):
        candidate = committed + request["delta"]
        reason = None
        if not MIN_I64 <= candidate <= MAX_I64:
            reason = "overflow"
        elif request["fail"]:
            reason = "injected_failure"
        if reason:
            return 1, {
                "status": "failure", "reason": reason, "failed_seq": sequence,
                "committed": committed, "replies": replies,
            }
        committed = candidate
        replies.append({"seq": sequence, "value": committed})
    return 0, {"status": "ok", "committed": committed, "replies": replies}


def verify():
    valid = [config(state_bytes=size, requests=[(delta, False) for delta in [5, -2, 7, -9, 0] * 20])
             for size in (256, 4096, 65536)]
    valid.extend([
        config(),
        config(requests=[(1, False)] * 256),
        config(requests=[(MIN_I64, False), (MAX_I64, False), (1, False)]),
        config(MAX_I64 - 1, [(1, False), (1, False), (-1, False)]),
        config(MIN_I64 + 1, [(-1, False), (-1, False), (1, False)]),
        config(17, [(3, False), (4, True), (100, False)]),
        config(requests=[(1, False)] * 64 + [(2, True), (100, False)]),
        config(MAX_I64, [(1, True)]),
    ])
    for workload in valid:
        code, result = invoke(json.dumps(workload).encode())
        expected_code, expected = reference(workload)
        require(code == expected_code, f"exit {code}: {result}")
        for key, value in expected.items():
            require(result.get(key) == value, f"{key}: {result} != {expected}")
        require(result.get("version") == 1, result)
        require(result.get("state_bytes") == workload["state_bytes"], result)
        require(result.get("message_bytes") == 32, result)
        require(result.get("environment", {}).get("otp") == "29", result)
        require(result["environment"].get("schedulers") == 1, result)

    invalid = [
        {},
        {**config(), "version": 2},
        {**config(), "version": True},
        {**config(), "initial": MAX_I64 + 1},
        {**config(), "initial": MIN_I64 - 1},
        {**config(), "initial": True},
        {**config(), "initial": 0.5},
        {**config(), "state_bytes": 0},
        {**config(), "state_bytes": 256.0},
        {**config(), "extra": 1},
        config(requests=[(1, False)] * 257),
        config(requests=[(MAX_I64 + 1, False)]),
        config(requests=[(True, False)]),
        config(requests=[(1, 1)]),
        {**config(), "requests": {}},
        {**config(), "requests": [{"delta": 1}]},
    ]
    payloads = [json.dumps(value).encode() for value in invalid]
    payloads.extend([
        b"", b"{", b"[]", b"\xff", b"{} {}",
        json.dumps(config()).replace('"version": 1', '"version": 1, "version": 1').encode(),
        b" " * (MAX_INPUT + 1),
    ])
    for payload in payloads:
        code, result = invoke(payload)
        require(code == 2 and result.get("status") == "invalid", (code, result))
    print(f"BEAM oracle v1: {len(valid)} checked/FIFO/exit cases, {len(payloads)} rejected inputs; no Wings or performance claim")


def main():
    parser = argparse.ArgumentParser(description="Bounded OTP 29 counter correctness oracle")
    parser.add_argument("--input", type=Path, help="Run one JSON workload instead of the regression suite")
    arguments = parser.parse_args()
    if arguments.input is None:
        verify()
        return 0
    with arguments.input.open("rb") as source:
        payload = source.read(MAX_INPUT + 1)
    if len(payload) > MAX_INPUT:
        print("Input exceeds 64 KiB", file=sys.stderr)
        return 2
    code, result = invoke(payload)
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"BEAM oracle verification failed: {error}", file=sys.stderr)
        sys.exit(3)
