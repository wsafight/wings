import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import subprocess
import time


ROOT = Path(__file__).resolve().parent.parent
CPU_SECONDS = 20
WALL_SECONDS = 30


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (CPU_SECONDS, CPU_SECONDS))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            checksum.update(block)
    return checksum.hexdigest()


def harnesses(messages):
    executables = set()
    finished = False
    for line in messages.splitlines():
        message = json.loads(line)
        if message.get("reason") == "build-finished":
            if message.get("success") is not True:
                raise ValueError("Cargo build did not succeed")
            finished = True
        if message.get("reason") == "compiler-artifact" and message.get("profile", {}).get("test"):
            executable = message.get("executable")
            if executable is not None:
                path = Path(executable).resolve(strict=True)
                if not path.is_file() or not os.access(path, os.X_OK):
                    raise ValueError(f"invalid test executable: {path}")
                executables.add(path)
    if not finished or not executables:
        raise ValueError("Cargo did not report a complete, nonempty test build")
    return [(path, digest(path)) for path in sorted(executables)]


def test_count(output):
    summaries = re.findall(
        r"^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; "
        r"(\d+) measured; (\d+) filtered out;.*$", output, re.MULTILINE,
    )
    if len(summaries) != 1:
        raise ValueError("expected exactly one successful libtest summary")
    passed, failed, ignored, measured, filtered = map(int, summaries[0])
    if failed or ignored or measured or filtered:
        raise ValueError("test harness failed or skipped tests")
    return passed


def stop_group(child):
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    child.wait()


def execute(path, checksum, output):
    started = time.monotonic()
    record = {"executable": str(path), "sha256": checksum, "status": "failed", "tests": None}
    child = None
    try:
        if digest(path) != checksum:
            raise ValueError("test executable changed after build inventory")
        with output.open("xb") as log:
            child = subprocess.Popen(
                [str(path)], stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                cwd=ROOT, start_new_session=True, preexec_fn=limit_child,
            )
            try:
                child.wait(timeout=WALL_SECONDS)
            except subprocess.TimeoutExpired:
                record["status"] = "timed_out"
                raise ValueError(f"test harness exceeded {WALL_SECONDS}s wall time") from None
            finally:
                stop_group(child)
        record["exit_code"] = child.returncode
        if child.returncode != 0:
            raise ValueError(f"test harness exited with {child.returncode}")
        if digest(path) != checksum:
            raise ValueError("test executable changed during execution")
        record["tests"] = test_count(output.read_text(errors="replace"))
        record["status"] = "passed"
    except (OSError, ValueError) as error:
        record["error"] = str(error)
    finally:
        if child is not None:
            record["exit_code"] = child.returncode
        record["elapsed_seconds"] = time.monotonic() - started
        record["log"] = output.name
    return record


def run(profile, output, packages=(), targets=("--all-targets",)):
    output.mkdir(parents=True, exist_ok=False)
    command = ["cargo", "test", *targets, "--locked", "--no-run", "--message-format=json"]
    if packages:
        for package in sorted(set(packages)):
            command.extend(["-p", package])
    else:
        command.append("--workspace")
    if profile == "release":
        command.append("--release")
    report = {
        "version": 1, "profile": profile, "command": command,
        "limits": {"cpu_seconds": CPU_SECONDS, "wall_seconds": WALL_SECONDS, "core_bytes": 0},
        "status": "failed", "harnesses": [], "tests": 0,
    }
    try:
        with (output / "cargo.jsonl").open("xb") as messages, (output / "cargo.stderr").open("xb") as errors:
            subprocess.run(command, cwd=ROOT, stdout=messages, stderr=errors, check=True)
        inventory = harnesses((output / "cargo.jsonl").read_text())
        (output / "inventory.json").write_text(json.dumps([
            {"executable": str(path), "sha256": checksum} for path, checksum in inventory
        ], indent=2) + "\n")
        for index, (path, checksum) in enumerate(inventory):
            print(f"HARNESS {index + 1}/{len(inventory)} {path}", flush=True)
            record = execute(path, checksum, output / f"harness-{index:03}.log")
            report["harnesses"].append(record)
            if record["status"] != "passed":
                raise ValueError(record.get("error", "test harness failed"))
            report["tests"] += record["tests"]
        report["status"] = "passed"
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        report["error"] = str(error)
    finally:
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"{report['status']}: {len(report['harnesses'])} harnesses, {report['tests']} tests; {output / 'report.json'}", flush=True)
    return report["status"] == "passed"


def main():
    parser = argparse.ArgumentParser(description="Build and execute every workspace libtest harness under fixed OS limits")
    parser.add_argument("--profile", choices=("debug", "release"), default="debug")
    parser.add_argument("-p", "--package", action="append", default=[], help="select a package; repeatable, defaults to the workspace")
    parser.add_argument("--core", action="store_true", help="run library, binary and integration harnesses instead of all targets")
    parser.add_argument("--output", type=Path, required=True, help="new directory for Cargo inventory, harness logs and report")
    arguments = parser.parse_args()
    try:
        targets = ("--lib", "--bins", "--tests") if arguments.core else ("--all-targets",)
        return 0 if run(arguments.profile, arguments.output.resolve(), arguments.package, targets) else 1
    except OSError as error:
        parser.exit(1, f"workspace verification: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
