import argparse
import json
import os
from pathlib import Path
import resource
import signal
import struct
import subprocess
import sys
import tempfile

import verify_beam_workload as beam
import verify_scalar_aot as aot
import verify_resource_receipt as receipts


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate field")
        result[key] = value
    return result


def decode(payload):
    if len(payload) > beam.MAX_INPUT:
        raise ValueError("input_bytes")
    value = json.loads(payload.decode("utf-8"), object_pairs_hook=strict_object)
    if type(value) is not dict or set(value) != {"version", "state_bytes", "initial", "requests"}:
        raise ValueError("fields")
    if type(value["version"]) is not int or value["version"] != 1:
        raise ValueError("version")
    if type(value["state_bytes"]) is not int or value["state_bytes"] not in (256, 4096, 65536):
        raise ValueError("state_bytes")

    def integer(number):
        if type(number) is not int or not beam.MIN_I64 <= number <= beam.MAX_I64:
            raise ValueError("i64")

    integer(value["initial"])
    requests = value["requests"]
    if type(requests) is not list or len(requests) > 256:
        raise ValueError("requests")
    for request in requests:
        if type(request) is not dict or set(request) != {"delta", "fail"}:
            raise ValueError("request fields")
        integer(request["delta"])
        if type(request["fail"]) is not bool:
            raise ValueError("fail")
    return value


def bridge(workload):
    result = struct.pack(">QQqQ", 1, workload["state_bytes"], workload["initial"], len(workload["requests"]))
    for sequence, request in enumerate(workload["requests"], 1):
        result += struct.pack(">QQqQ", sequence, 2 if request["fail"] else 1, request["delta"], 0)
    return result


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def invoke_bridge(payload, artifact):
    executable = aot.TARGET / aot.PROFILE / "examples/beam_counter"
    child = subprocess.Popen(
        [str(executable), "--trusted", str(artifact)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        start_new_session=True, preexec_fn=limit_child, cwd=aot.ROOT,
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
    if len(stdout) > 65536 or len(stderr) > 65536 or stderr:
        raise RuntimeError(f"Invalid Wings output: {stderr.decode(errors='replace')}")
    try:
        result = json.loads(stdout)
    except (ValueError, UnicodeError) as error:
        raise RuntimeError(f"Invalid Wings JSON (exit {child.returncode}): {stdout!r}") from error
    return child.returncode, result


def invoke(payload, artifact):
    try:
        workload = decode(payload)
    except (ValueError, UnicodeError, RecursionError):
        return 2, {"version": 1, "status": "invalid", "reason": "schema"}
    return invoke_bridge(bridge(workload), artifact)


def cases():
    valid = []
    seen = set()
    for workload in beam.valid_cases():
        key = (workload["initial"], tuple((request["delta"], request["fail"]) for request in workload["requests"]))
        if key in seen:
            continue
        seen.add(key)
        for size in (256, 4096, 65536):
            valid.append({**workload, "state_bytes": size})
    for size in (256, 4096, 65536):
        valid.extend([
            beam.config(beam.MIN_I64, [(-1, True), (1, False)], size),
            beam.config(beam.MAX_I64, [(0, True), (-1, False)], size),
        ])
        for failed in (1, 32, 33, 256):
            requests = [(1, False)] * (failed - 1) + [(7, True)] + [(100, False)] * (256 - failed)
            valid.append(beam.config(-19, requests, size))
    return valid


def verify(artifact, compare_beam):
    valid = cases()
    for workload in valid:
        payload = json.dumps(workload).encode()
        code, result = invoke(payload, artifact)
        expected_code, expected = beam.reference(workload)
        beam.require(code == expected_code, (code, result, workload))
        for key, value in expected.items():
            beam.require(result.get(key) == value, (key, result, expected))
        beam.require(result.get("version") == 1, result)
        beam.require(result.get("state_bytes") == workload["state_bytes"], result)
        beam.require(result.get("message_bytes") == 32 and result.get("resources_released") is True, result)
        receipts.validate(result)
        beam.require(result.get("environment", {}).get("runtime") == "wings", result)
        beam.require(result["environment"].get("workers") == 1 and result["environment"].get("window") == 32, result)
        if compare_beam:
            beam_code, beam_result = beam.invoke(payload)
            beam.require(beam_code == code, (beam_code, code))
            for key, value in expected.items():
                beam.require(beam_result.get(key) == value, (key, beam_result, result))
            beam.require(beam_result.get("state_bytes") == workload["state_bytes"], beam_result)
            beam.require(beam_result.get("message_bytes") == 32, beam_result)
            beam.require(beam_result.get("environment", {}).get("otp") == "29", beam_result)
            beam.require(beam_result["environment"].get("schedulers") == 1, beam_result)

    invalid = beam.invalid_payloads() + [
        b'{"version":1,"state_bytes":256,"initial":0,"requests":[{"delta":1,"delta":2,"fail":false}]}',
        b'{"version":1,"state_bytes":256,"initial":0,"requests":[{"delta":1,"fail":false,"extra":1}]}',
        b"[" * 200 + b"]" * 200,
        b'{"version":1,"state_bytes":256,"initial":NaN,"requests":[]}',
        b'{"version":1,"state_bytes":256,"initial":0,"requests":[{"delta":1.0,"fail":false}]}',
    ]
    for payload in invalid:
        code, result = invoke(payload, artifact)
        beam.require(code == 2 and result.get("status") == "invalid", (code, result))
        if compare_beam:
            beam_code, beam_result = beam.invoke(payload)
            beam.require(beam_code == 2 and beam_result.get("status") == "invalid", (beam_code, beam_result))

    sample = bridge(beam.config(requests=[(-1, False)]))
    invalid_bridge = [sample[:length] for length in (0, 1, 31, 32, 63)] + [sample + b"\0", b"\0" * 8225]
    for index, value in [(0, 2), (1, 257), (3, 257), (3, 2**64 - 1), (4, 0), (5, 3), (7, 1)]:
        changed = bytearray(sample)
        struct.pack_into(">Q", changed, index * 8, value)
        invalid_bridge.append(bytes(changed))
    for payload in invalid_bridge:
        code, result = invoke_bridge(payload, artifact / "missing")
        beam.require(code == 2 and result.get("status") == "invalid", (code, result))
    label = "Wings/BEAM/Python" if compare_beam else "Wings/Python (BEAM not executed)"
    print(f"Counter v1 {label}: {len(valid)} checked/FIFO/exit cases, {len(invalid)} JSON and {len(invalid_bridge)} bridge rejections; no performance claim")


def run(arguments, artifact):
    if arguments.input is None:
        verify(artifact, arguments.compare_beam)
        return 0
    with arguments.input.open("rb") as source:
        payload = source.read(beam.MAX_INPUT + 1)
    code, result = invoke(payload, artifact)
    if arguments.compare_beam:
        beam_code, beam_result = beam.invoke(payload)
        beam.require(beam_code == code, (beam_code, code))
        if code in (0, 1):
            _, expected = beam.reference(decode(payload))
            for key, value in expected.items():
                beam.require(result.get(key) == beam_result.get(key) == value, (key, result, beam_result))
        else:
            beam.require(beam_result.get("status") == result.get("status"), (result, beam_result))
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    return code


def main():
    parser = argparse.ArgumentParser(description="Bounded fixed-byte Wings/BEAM Counter correctness comparison")
    parser.add_argument("--artifact", type=Path, help="Use an existing trusted Counter artifact")
    parser.add_argument("--compare-beam", action="store_true", help="Also execute the OTP 29 oracle; required for a BEAM comparison claim")
    parser.add_argument("--input", type=Path, help="Run one JSON workload instead of the regression suite")
    arguments = parser.parse_args()
    if arguments.artifact is not None:
        return run(arguments, arguments.artifact.resolve())
    with tempfile.TemporaryDirectory(prefix="wings-counter-v1-") as directory:
        artifact = Path(directory) / "counter"
        aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-counter.wings", "-o", artifact])
        return run(arguments, artifact)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Counter equivalence verification failed: {error}", file=sys.stderr)
        sys.exit(3)
