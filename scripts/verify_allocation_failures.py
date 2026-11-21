import argparse
import hashlib
import json
from pathlib import Path
import subprocess

import verify_scalar_aot as aot


def validate(report):
    expected = {
        "status": "ok", "fixed_heap_failures": 4, "root_failures": 1,
        "module_path_failures": 1,
        "aot_heap_failures": 7, "invocation_failures": 1, "managed_recovered_failures": 1,
        "managed_terminal_failures": 2, "host_payload_failures": 1, "reply_payload_failures": 1,
        "total_injected_failures": 19, "healthy_replies": 2, "invocation_retry_value": 8,
        "resources_released": True, "metric": "rust_requested_storage_not_rss_or_complete_resource_acceptance",
    }
    if type(report) is not dict or set(report) != set(expected) | {"bootstrap_bytes", "remaining_bytes"}:
        raise ValueError("allocation failure report fields")
    for key, value in expected.items():
        if type(report[key]) is not type(value) or report[key] != value:
            raise ValueError(f"allocation failure report {key}")
    if (type(report["bootstrap_bytes"]) is not int or type(report["remaining_bytes"]) is not int
            or report["bootstrap_bytes"] <= 0 or report["remaining_bytes"] != report["bootstrap_bytes"]):
        raise ValueError("allocation failure report resource receipt")


def verify(artifact, output):
    output.mkdir(parents=True, exist_ok=False)
    executable = aot.TARGET / aot.PROFILE / "examples/allocation_failures"
    record = {"status": "failed", "cpu_seconds": 20, "wall_seconds": 30, "core_bytes": 0,
              "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
              "artifact_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                  for path in artifact.iterdir() if path.is_file()},
              "command": list(map(str, [executable, "--trusted", artifact])),
              "performance_gate_proven": False, "complete_resource_gate_proven": False}
    stdout, stderr = "", ""
    try:
        result = subprocess.run(record["command"], input="", text=True, capture_output=True,
                                timeout=30, cwd=aot.ROOT, preexec_fn=aot.limit_child)
        stdout, stderr = result.stdout, result.stderr
        record["exit"] = result.returncode
        if result.returncode != 0 or stderr:
            raise ValueError("allocation failure fixture failed or stderr is nonempty")
        report = json.loads(stdout)
        validate(report)
        record["report"] = report
        record["status"] = "ok"
    except BaseException as error:
        record["error"] = str(error)
        if isinstance(error, subprocess.TimeoutExpired):
            stdout, stderr = error.stdout or b"", error.stderr or b""
        raise
    finally:
        for name, value in (("stdout", stdout), ("stderr", stderr)):
            path = output / name
            path.write_bytes(value if isinstance(value, bytes) else value.encode())
            record[name + "_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        (output / "report.json").write_text(json.dumps(record, indent=2) + "\n")
    print("Allocator failures verified: 19 actual null results, fixed/AOT heaps, roots, module path storage, "
          "argument retry, managed GC retry/failure, message/reply copies, old state and healthy progress; "
          "not complete resource acceptance")


def main():
    parser = argparse.ArgumentParser(description="Run bounded allocator failure injection in a real AOT child")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    verify(arguments.artifact.resolve(), arguments.output.resolve())


if __name__ == "__main__":
    main()
