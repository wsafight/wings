import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

import measure_memory
import measure_open_loop as measure
import verify_scalar_aot as aot


RATE = 80000
PHASES = ("claim", "start", "snapshot", "finish")
NUMBERS = ("acquisitions", "wait_ns", "held_ns", "max_wait_ns", "max_held_ns")


def number(value):
    measure.require(type(value) is int and 0 <= value <= (1 << 64) - 1, "lock statistic u64")
    return value


def validate_statistics(result, enabled):
    measure.require(type(enabled) is bool, "lock observation switch")
    if not enabled:
        measure.require("worker_lock_statistics" not in result, "disabled lock statistics emitted")
        return None
    value = result.get("worker_lock_statistics")
    measure.require(type(value) is dict and set(value) == {"version", "scope", "metric", "after_worker_join", "workers"}, "lock statistics fields")
    for key, expected in {"version": 1, "scope": "whole_scheduler_lifetime", "metric": "wall_time_not_cpu", "after_worker_join": True}.items():
        measure.require(type(value[key]) is type(expected) and value[key] == expected, f"lock statistics {key}")
    rows = value["workers"]
    measure.require(type(rows) is list and len(rows) == 2, "lock worker count")
    committed = result["committed"]
    measure.require(type(committed) is list and len(committed) == number(result["actors"]), "lock committed count")
    expected = number(len(committed) + sum(number(item) for item in committed))
    seen = set()
    starts = finishes = 0
    for row in rows:
        measure.require(type(row) is dict and set(row) == {"worker", *PHASES}, "lock worker fields")
        worker = number(row["worker"])
        measure.require(worker < 2 and worker not in seen, "lock worker identity")
        seen.add(worker)
        for phase in PHASES:
            stats = row[phase]
            measure.require(type(stats) is dict and set(stats) == {*NUMBERS, "saturated"}, "lock phase fields")
            measure.require(stats["saturated"] is False, "lock saturated statistics")
            for key in NUMBERS:
                number(stats[key])
            measure.require(stats["max_wait_ns"] <= stats["wait_ns"] and stats["max_held_ns"] <= stats["held_ns"], "lock maxima exceed totals")
            measure.require(stats["wait_ns"] <= stats["acquisitions"] * stats["max_wait_ns"]
                            and stats["held_ns"] <= stats["acquisitions"] * stats["max_held_ns"], "lock totals exceed counted maxima")
            if stats["acquisitions"] == 0:
                measure.require(all(stats[key] == 0 for key in NUMBERS), "unacquired lock has timing")
        measure.require(row["claim"]["acquisitions"] >= row["start"]["acquisitions"], "lock claims missing")
        starts += row["start"]["acquisitions"]
        finishes += row["finish"]["acquisitions"]
    measure.require(starts == expected and finishes == expected, "lock handler count mismatch")
    return value


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1 << 20):
            digest.update(block)
    return digest.hexdigest()


def validate_cpu(result, enabled):
    measure.require(type(enabled) is bool, "worker CPU observation switch")
    if not enabled:
        measure.require("worker_cpu_statistics" not in result, "disabled worker CPU statistics emitted")
        return None
    value = result.get("worker_cpu_statistics")
    measure.require(type(value) is dict and set(value) == {"version", "scope", "metric", "after_worker_join", "workers"}, "worker CPU fields")
    for key, expected in {"version": 1, "scope": "worker_function_including_setup_cleanup",
                          "metric": "thread_cpu_time_not_wall", "after_worker_join": True}.items():
        measure.require(type(value[key]) is type(expected) and value[key] == expected, f"worker CPU {key}")
    rows = value["workers"]
    measure.require(type(rows) is list and len(rows) == 2, "CPU worker count")
    seen = set()
    for row in rows:
        measure.require(type(row) is dict and set(row) == {"worker", "status", "nanoseconds"}, "worker CPU record")
        worker = number(row["worker"])
        measure.require(worker < 2 and worker not in seen, "CPU worker identity")
        seen.add(worker)
        measure.require(row["status"] == "ok", "worker CPU unavailable or incomplete")
        measure.require(number(row["nanoseconds"]) <= (1 << 64) - 3, "worker CPU encoding overflow")
    return value


def verify_inputs(frozen, inputs):
    for name, expected in inputs.items():
        measure.require(checksum(frozen / name) == expected, f"frozen lock diagnostic input changed: {name}")


def prepare(artifact, output):
    measure.require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    measure.require(platform.system() in ("Darwin", "Linux"), "native macOS/Linux required")
    runner = aot.TARGET / "release/examples/beam_open_loop"
    artifact = artifact.resolve()
    output = output.resolve()
    measure.require(runner.is_file() and artifact.is_dir(), "build release runner and artifact first")
    measure.require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) <= 16 << 20, "lock diagnostic artifact size")
    required = RATE * 90 * measure.EVENT.size * 3 * 4 + (1 << 30)
    measure.require(shutil.disk_usage(aot.ROOT).free > required, "insufficient space for four worst-case traces and safety margin")
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "frozen"
    frozen.mkdir()
    shutil.copy2(runner, frozen / "beam_open_loop")
    shutil.copytree(artifact, frozen / "artifact")
    shutil.copy2(aot.ROOT / "scripts/measure_memory.c", frozen / "measure_memory.c")
    setup = {"status": "running"}
    try:
        setup["command"] = measure_memory.build(frozen / "measure_memory.c", frozen / "measure_memory")
        setup["preflight"] = measure_memory.snapshot(os.getpid(), frozen / "measure_memory")
        setup["status"] = "ok"
    except BaseException as error:
        setup.update({"status": "invalid_experiment", "error": str(error)})
        raise
    finally:
        (output / "memory-setup.json").write_text(json.dumps(setup, indent=2) + "\n")
    names = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT, timeout=5).decode().split("\0")
    for name in sorted(set(names)):
        source = aot.ROOT / name
        if name and not name.startswith("docs/proposals/") and source.is_file():
            destination = frozen / "source" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
    inputs = {str(path.relative_to(frozen)): checksum(path) for path in sorted(frozen.rglob("*")) if path.is_file()}
    environment = {
        "status": "diagnostic_observer_overhead_not_performance_acceptance", "case": "hot-queue", "rate": RATE,
        "warmup_seconds": 30, "sample_seconds": 60, "paired_repetitions": 2,
        "order": [[False, True], [True, False]], "platform": platform.platform(), "machine": platform.machine(),
        "source_base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True, timeout=5).strip(),
        "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True, timeout=5),
        "rustc": aot.command(["rustc", "-vV"]).stdout.strip(), "inputs": inputs,
        "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120,
                   "trace_bytes": measure.MAX_TRACE, "observed_rss_safety_bytes": 1 << 30},
        "scope": "whole scheduler lifetime including initialization and cleanup; joined workers; wall time, not CPU",
        "worker_cpu_scope": "only worker function including its setup and cleanup; excludes timer/generator and OS thread startup/teardown",
        "missing_gates": ["CPU hard quota and attribution", "host isolation and monitoring bias", "overload/service I/O races",
                          "complete memory", "native dual-platform", "BEAM throughput and memory acceptance"],
    }
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    verify_inputs(frozen, inputs)
    os.execv(sys.executable, [sys.executable, str(frozen / "source/scripts/measure_lock_statistics.py"), "--frozen-output", str(output)])


def run(output):
    frozen = output / "frozen"
    measure.require(Path(__file__).resolve() == (frozen / "source/scripts/measure_lock_statistics.py").resolve(), "execute the frozen lock driver")
    environment = json.loads((output / "environment.json").read_text())
    inputs = environment["inputs"]
    reports = []
    try:
        for repetition, order in enumerate(((False, True), (True, False)), 1):
            for enabled in order:
                verify_inputs(frozen, inputs)
                name = f"{repetition}-{'observed' if enabled else 'disabled'}"
                trace = output / f"{name}.events.bin"
                command = [frozen / "beam_open_loop", "--trusted", frozen / "artifact", "--measure", "hot-queue", RATE, trace]
                if enabled:
                    command.append("--observe-locks")

                def validate(path, case, rate, result):
                    summary = measure.validate_trace(path, case, rate, result)
                    summary["worker_lock_statistics"] = validate_statistics(result, enabled)
                    summary["worker_cpu_statistics"] = validate_cpu(result, enabled)
                    summary["performance_or_memory_gate_proven"] = False
                    return summary

                result = measure.measurement(command, trace, output / name, "hot-queue", RATE,
                                             frozen / "measure_memory", runtime="wings", validator=validate)
                verify_inputs(frozen, inputs)
                reports.append({"repetition": repetition, "enabled": enabled, **result})
                (output / "report.json").write_text(json.dumps({"status": "complete" if len(reports) == 4 else "running",
                    "runs": reports, "performance_or_memory_gate_proven": False, "full_cpu_attribution_proven": False}, indent=2) + "\n")
                print(f"{name}: replayed 30s/60s, lock statistics validated; not CPU or performance acceptance", flush=True)
    except BaseException as error:
        (output / "report.json").write_text(json.dumps({"status": "invalid_experiment", "error": str(error), "runs": reports,
            "performance_or_memory_gate_proven": False, "full_cpu_attribution_proven": False}, indent=2) + "\n")
        raise


def main():
    parser = argparse.ArgumentParser(description="Frozen hot-queue 80000/s worker lock diagnostics: two off/on pairs, not performance acceptance")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--frozen-output", type=Path, help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if arguments.frozen_output is not None:
        measure.require(arguments.artifact is None and arguments.output is None, "frozen diagnostic arguments")
        run(arguments.frozen_output.resolve())
    else:
        measure.require(arguments.artifact is not None and arguments.output is not None, "--artifact and --output required")
        prepare(arguments.artifact, arguments.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Worker lock diagnostic failed: {error}", file=sys.stderr)
        sys.exit(3)
