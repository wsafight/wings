import argparse
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import struct
import subprocess
import sys
import tempfile

import verify_scalar_aot as aot
import verify_resource_receipt as receipts
from verify_counter_equivalence import strict_object

MIN_I64 = -(2**63)
MAX_I64 = 2**63 - 1


def decode(payload):
    if len(payload) > 65536:
        raise ValueError("input_bytes")
    workload = json.loads(payload.decode("utf-8"), object_pairs_hook=strict_object)
    if type(workload) is not dict or set(workload) != {"version", "actors", "state_bytes", "initial", "requests"}:
        raise ValueError("fields")
    if type(workload["version"]) is not int or workload["version"] != 2:
        raise ValueError("version")
    actors, length = workload["actors"], workload["state_bytes"]
    if type(actors) is not int or not 1 <= actors <= 10000 or type(length) is not int or length not in (256, 4096, 65536) or actors * length > 16 << 20:
        raise ValueError("limits")
    if type(workload["initial"]) is not int or not MIN_I64 <= workload["initial"] <= MAX_I64:
        raise ValueError("initial")
    requests = workload["requests"]
    if type(requests) is not list or len(requests) > 256:
        raise ValueError("requests")
    for request in requests:
        if type(request) is not dict or set(request) != {"actor", "delta", "fail"}:
            raise ValueError("request fields")
        if type(request["actor"]) is not int or not 1 <= request["actor"] <= actors:
            raise ValueError("actor")
        if type(request["delta"]) is not int or not MIN_I64 <= request["delta"] <= MAX_I64 or type(request["fail"]) is not bool:
            raise ValueError("request")
    return workload


def bridge(workload):
    payload = struct.pack(">QQqQQ", 2, workload["state_bytes"], workload["initial"], workload["actors"], len(workload["requests"]))
    for sequence, request in enumerate(workload["requests"], 1):
        payload += struct.pack(">QQQqQ", request["actor"], sequence, 2 if request["fail"] else 1, request["delta"], 0)
    return payload


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def invoke(command, payload):
    environment = os.environ.copy()
    for name in ("ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "ERL_LIBS"):
        environment.pop(name, None)
    environment["ERL_CRASH_DUMP"] = os.devnull
    environment["ERL_CRASH_DUMP_SECONDS"] = "0"
    child = subprocess.Popen([str(item) for item in command], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             env=environment, cwd=aot.ROOT, start_new_session=True, preexec_fn=limit_child)
    try:
        stdout, stderr = child.communicate(payload, timeout=10)
    except BaseException:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.communicate()
        raise
    if len(stdout) > 1 << 20 or len(stderr) > 65536 or stderr:
        raise RuntimeError(f"multi Counter stderr/output limit: {stderr[:4096]!r}")
    return child.returncode, json.loads(stdout)


def reference(workload):
    committed = [workload["initial"]] * workload["actors"]
    replies, failures = [], {}
    for sequence, request in enumerate(workload["requests"], 1):
        actor = request["actor"]
        if actor in failures:
            continue
        candidate = committed[actor - 1] + request["delta"]
        reason = "overflow" if not MIN_I64 <= candidate <= MAX_I64 else "injected_failure" if request["fail"] else None
        if reason:
            failures[actor] = {"actor": actor, "reason": reason, "failed_seq": sequence}
        else:
            committed[actor - 1] = candidate
            replies.append({"actor": actor, "seq": sequence, "value": candidate})
    return int(bool(failures)), {"version": 2, "status": "failure" if failures else "ok", "actors": workload["actors"],
                                "state_bytes": workload["state_bytes"], "message_bytes": 32, "committed": committed,
                                "replies": replies, "failures": [failures[actor] for actor in sorted(failures)], "states_verified": True}


def config(actors=2, length=256, initial=0, requests=()):
    return {"version": 2, "actors": actors, "state_bytes": length, "initial": initial,
            "requests": [{"actor": actor, "delta": delta, "fail": fail} for actor, delta, fail in requests]}


def cases():
    result = []
    for actors, length in ((10000, 256), (1000, 4096), (100, 65536)):
        result.append(config(actors, length, -7, [(1 + (index % 100) * (actors // 100), index % 7 - 3, False) for index in range(256)]))
    for length in (256, 4096, 65536):
        result.extend([
            config(1, length, -7, [(1, 7, False)]),
            config(2, length),
            config(2, length, 0, [(1 + index % 2, 1, index in (4, 9)) for index in range(80)]),
            config(2, length, MAX_I64, [(1, 1, True), (2, -1, False), (1, -3, False), (2, 1, False)]),
            config(2, length, MIN_I64, [(1, -1, False), (2, 1, False), (2, -1, True)]),
        ])
    return result


def invalid_json():
    sample = config(requests=[(1, -7, False)])
    payload = json.dumps(sample).encode()
    rejected = [b"", b"[", b"null", b"[]", b"true", b"\xff", payload + b"{}", b" " * 65537,
                b"[" * 1500 + b"]" * 1500,
                payload.replace(b'"version": 2', b'"version": 2, "version": 2'),
                payload.replace(b'"actor": 1', b'"actor": 1, "actor": 1')]
    for key, value in [("version", 1), ("version", True), ("version", 2.0), ("actors", 0), ("actors", 10001),
                       ("actors", True), ("actors", 2.0), ("state_bytes", 257), ("state_bytes", 256.0),
                       ("initial", MIN_I64 - 1), ("initial", MAX_I64 + 1), ("initial", False),
                       ("initial", 0.0), ("requests", None), ("requests", {}),
                       ("requests", [sample["requests"][0]] * 257)]:
        rejected.append(json.dumps({**sample, key: value}).encode())
    rejected.append(json.dumps({**sample, "actors": 10000, "state_bytes": 65536}).encode())
    rejected.append(json.dumps({**sample, "extra": 0}).encode())
    rejected.extend(json.dumps({key: value for key, value in sample.items() if key != missing}).encode() for missing in sample)
    for key, value in [("actor", 0), ("actor", 3), ("actor", True), ("actor", 1.0),
                       ("delta", MIN_I64 - 1), ("delta", MAX_I64 + 1), ("delta", False),
                       ("delta", 0.0), ("fail", 0), ("fail", "false"), ("extra", 0)]:
        rejected.append(json.dumps({**sample, "requests": [{**sample["requests"][0], key: value}]}).encode())
    for missing in sample["requests"][0]:
        request = {key: value for key, value in sample["requests"][0].items() if key != missing}
        rejected.append(json.dumps({**sample, "requests": [request]}).encode())
    rejected.append(json.dumps({**sample, "requests": [None]}).encode())
    return rejected


def verify(artifact, compare_beam):
    wings = [aot.TARGET / aot.PROFILE / "examples/beam_multi_counter", "--trusted", artifact]
    escript = shutil.which("escript") if compare_beam else None
    if compare_beam and escript is None:
        raise RuntimeError("Erlang/OTP 29 escript is required")
    beam = [escript, aot.ROOT / "scripts/beam_multi_workload.escript"]
    valid = cases()
    for workload in valid:
        payload = json.dumps(workload, separators=(",", ":")).encode()
        decode(payload)
        expected_code, expected = reference(workload)
        for command, data, runtime in [(wings, bridge(workload), "wings")] + ([(beam, payload, "beam")] if compare_beam else []):
            code, result = invoke(command, data)
            assert code == expected_code, (code, result, workload)
            for key, value in expected.items():
                assert result.get(key) == value, (runtime, key, result, expected)
            assert result["environment"]["window"] == 32, result
            if runtime == "wings":
                receipts.validate(result)
                assert result["resources_released"] is True and result["environment"]["workers"] == 2, result
            else:
                assert result["environment"]["otp"] == "29" and result["environment"]["schedulers"] == 2, result
    sample = bridge(config(requests=[(1, -7, False)]))
    rejected = [sample[:length] for length in (0, 1, 39, 40, 79)] + [sample + b"\0", b"\0" * 10281]
    for index, value in [(0, 1), (1, 257), (3, 0), (3, 10001), (4, 257), (5, 0), (5, 3), (6, 0), (7, 3), (9, 1)]:
        changed = bytearray(sample)
        struct.pack_into(">Q", changed, index * 8, value)
        rejected.append(bytes(changed))
    for payload in rejected:
        code, result = invoke([*wings[:2], artifact / "missing"], payload)
        assert code == 2 and result.get("status") == "invalid", (code, result)
    invalid = invalid_json()
    for payload in invalid:
        try:
            decode(payload)
        except (ValueError, RecursionError):
            pass
        else:
            raise AssertionError(f"accepted invalid JSON: {payload[:200]!r}")
        if compare_beam:
            code, result = invoke(beam, payload)
            assert code == 2 and result.get("status") == "invalid", (code, result, payload[:200])
    print(f"Multi Counter v2 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: {len(valid)} byte-state/FIFO/exit cases, {len(rejected)} bridge and {len(invalid)} JSON rejections; no performance claim")


def main():
    parser = argparse.ArgumentParser(description="Bounded multi-actor byte Counter correctness")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-multi-counter-") as directory:
            artifact = Path(directory) / "counter"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-multi-counter.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Multi Counter verification failed: {error}", file=sys.stderr)
        sys.exit(3)
