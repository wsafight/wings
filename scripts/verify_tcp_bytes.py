import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import time

import verify_scalar_aot as aot


def frame(sequence, delta, operation=1, reserved=0):
    return struct.pack(">QQqQ", sequence, operation, delta, reserved)


def workloads():
    return [
        ("fragmented", [[frame(1, 3), frame(2, 1), frame(3, 5), frame(4, -1)]], True),
        ("connections", [[frame(1, -5), frame(2, 8)], [frame(1, 10), frame(2, -7)]], False),
        ("candidate-failure", [[frame(1, 7), frame(2, 1, 2), frame(3, 99)]], False),
        ("overflow", [[frame(1, 2**63 - 1), frame(2, 1), frame(3, 99)]], False),
        ("underflow", [[frame(1, -(2**63)), frame(2, -1), frame(3, 99)]], False),
        ("invalid", [[frame(0, 1)], [frame(1, 1, 9)], [frame(1, 1, reserved=1)],
                     [frame(1, 4)]], False),
        ("maximum", [[frame(index + 1, 1) for index in range(64)] for _ in range(8)], False),
    ]


def model(groups):
    state, dead = 0, False
    counts = {key: 0 for key in ("connections", "published", "replies", "rejected", "timed_out",
                                "failed", "invalid", "io_errors")}
    replies = []
    for group in groups:
        counts["connections"] += 1
        current = []
        for payload in group:
            sequence, operation, delta, reserved = struct.unpack(">QQqQ", payload)
            if not sequence or operation not in (1, 2) or reserved:
                counts["invalid"] += 1
                break
            if dead:
                counts["rejected"] += 1
                current.append((sequence, 4, 4, 0))
            else:
                counts["published"] += 1
                candidate = state + delta
                if operation == 2 or not -(2**63) <= candidate < 2**63:
                    dead = True
                    counts["failed"] += 1
                    current.append((sequence, 4, 3, 0))
                else:
                    state = candidate
                    counts["replies"] += 1
                    current.append((sequence, 3, state, 0))
        replies.append(current)
    return replies, counts | {"committed": state, "full_state_verified": not dead}


def receive(connection):
    payload = bytearray()
    deadline = time.monotonic() + 2
    while len(payload) < 32:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("reply frame deadline")
        connection.settimeout(remaining)
        block = connection.recv(32 - len(payload))
        if not block:
            raise AssertionError("service closed before completing a reply")
        payload.extend(block)
    return struct.unpack(">QQqQ", payload)


def validate_report(report, groups):
    counts = model(groups)[1]
    assert isinstance(report, dict), report
    for key, value in counts.items():
        assert type(report[key]) is type(value) and report[key] == value, (key, report, value)
    assert report["resources_released"] is True, report
    assert type(report["max_socket_buffer"]) is int and 0 < report["max_socket_buffer"] <= 65536, report
    if "bootstrap_bytes" in report:
        assert type(report["bootstrap_bytes"]) is int and type(report["remaining_bytes"]) is int
        assert report["bootstrap_bytes"] == report["remaining_bytes"] > 0, report
        for key in ("wake_errors", "socket_setup_errors", "socket_buffer_rejections"):
            assert type(report[key]) is int and report[key] == 0, report
        if os.uname().sysname == "Darwin":
            assert type(report["max_tcp_segment"]) is int and 0 < report["max_tcp_segment"] <= 1024, report
        else:
            assert report["max_tcp_segment"] is None, report
    else:
        assert report["gate_closed"] is True and report["actor_stopped"] is True and report["otp_release"] == "29", report


def run(command, groups, fragmented, output):
    expected, _ = model(groups)
    started = time.monotonic()
    record = {"command": list(map(str, command)), "status": "failed",
              "input_frames": [[payload.hex() for payload in group] for group in groups],
              "replies": [], "cpu_seconds": 20, "handshake_seconds": 5}
    child = subprocess.Popen(list(map(str, command)), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             bufsize=0, cwd=aot.ROOT, start_new_session=True, preexec_fn=aot.limit_child)
    try:
        ready = aot.tcp_ready(child)
        record["readiness"] = ready
        host, port = ready["listening"].split(":")
        assert host == "127.0.0.1", ready
        for index, group in enumerate(groups):
            with socket.create_connection((host, int(port)), timeout=2) as connection:
                connection.settimeout(2)
                payload = b"".join(group)
                if fragmented and index == 0:
                    connection.sendall(payload[:3])
                    connection.sendall(payload[3:])
                else:
                    connection.sendall(payload)
                actual = [receive(connection) for _ in expected[index]]
                record["replies"].append(actual)
                assert actual == expected[index], (actual, expected[index])
                connection.settimeout(2)
                if len(expected[index]) != len(group) or len(group) == max(map(len, groups)):
                    try:
                        assert connection.recv(1) == b""
                    except ConnectionResetError:
                        pass
        stdout, stderr = child.communicate(timeout=5)
        record["exit"] = child.returncode
        record["stdout"] = stdout.decode(errors="replace")
        record["stderr"] = stderr.decode(errors="replace")
        assert child.returncode == 0 and not stderr, record
        report = json.loads(stdout)
        validate_report(report, groups)
        record["report"] = report
        record["status"] = "ok"
    except BaseException as error:
        record["error"] = repr(error)
        raise
    finally:
        record["exit_before_cleanup"] = child.poll()
        if child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        stdout, stderr = child.communicate(timeout=5)
        record.setdefault("stdout", (stdout or b"").decode(errors="replace"))
        record.setdefault("stderr", (stderr or b"").decode(errors="replace"))
        record["exit"] = child.returncode
        record["elapsed_seconds"] = time.monotonic() - started
        output.write_text(json.dumps(record, indent=2) + "\n")
    return record


def verify_invalid_modes(artifact, output):
    rows = []
    executable = aot.TARGET / aot.PROFILE / "examples/tcp_counter"
    for index, options in enumerate((["--state-bytes", "0"], ["--state-bytes", "257"],
                                      ["--state-bytes", "-1"], ["--unknown", "256"],
                                      ["--state-bytes"], ["--state-bytes", "256", "extra"])):
        command = [executable, "--trusted", artifact, 1, 1, *options]
        result = aot.command(command, expected=1)
        assert not result.stdout and result.stderr.startswith("tcp_counter: "), result
        rows.append({"command": list(map(str, command)), "exit": result.returncode,
                     "stdout": result.stdout, "stderr": result.stderr})
    (output / "invalid-modes.json").write_text(json.dumps(rows, indent=2) + "\n")


def verify(artifact, output, compare_beam=False):
    output.mkdir(parents=True, exist_ok=False)
    executable = aot.TARGET / aot.PROFILE / "examples/tcp_counter"
    records = []
    summary = {"status": "failed", "compare_beam": compare_beam, "cases": records,
               "performance_gate_proven": False,
               "wings_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
               "artifact_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                   for path in artifact.iterdir() if path.is_file()},
               "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
               "beam_sha256": hashlib.sha256((aot.ROOT / "scripts/beam_tcp_counter.escript").read_bytes()).hexdigest()}
    try:
        beam = shutil.which("escript") if compare_beam else None
        if compare_beam and beam is None:
            raise RuntimeError("Erlang/OTP 29 escript is required")
        verify_invalid_modes(artifact, output)
        for length in (256, 4096, 65536):
            for name, groups, fragmented in workloads():
                rows = []
                command = [executable, "--trusted", artifact, len(groups), max(map(len, groups)),
                           "--state-bytes", length]
                commands = [("wings", command)]
                if compare_beam:
                    commands.append(("beam", [beam, aot.ROOT / "scripts/beam_tcp_counter.escript",
                                              length, len(groups), max(map(len, groups))]))
                for engine, command in commands:
                    destination = output / f"{length}-{name}-{engine}.json"
                    entry = {"state_bytes": length, "workload": name, "engine": engine,
                             "record": destination.name, "status": "failed"}
                    records.append(entry)
                    try:
                        record = run(command, groups, fragmented, destination)
                    finally:
                        if destination.exists():
                            entry["sha256"] = hashlib.sha256(destination.read_bytes()).hexdigest()
                    assert record["report"]["state_bytes"] == length, record
                    rows.append(record)
                    entry["status"] = "ok"
                if compare_beam:
                    assert rows[0]["replies"] == rows[1]["replies"]
                    expected = model(groups)[1]
                    assert all(rows[0]["report"][key] == rows[1]["report"][key] for key in expected)
        summary["status"] = "ok"
    except BaseException as error:
        summary["error"] = repr(error)
        raise
    finally:
        (output / "report.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"TCP Bytes Counter {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: "
          "three complete state sizes, fragmented/pipelined connections, checked failures and cleanup; not performance acceptance")


def main():
    parser = argparse.ArgumentParser(description="Bounded real TCP Bytes Counter equivalence")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    verify(arguments.artifact.resolve(), arguments.output.resolve(), arguments.compare_beam)


if __name__ == "__main__":
    main()
