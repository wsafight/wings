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


RATE = 10000
LABELS = ("baseline", "candidate")


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1 << 20):
            digest.update(block)
    return digest.hexdigest()


def verify_inputs(frozen, inputs):
    for name, expected in inputs.items():
        measure.require(checksum(frozen / name) == expected, f"frozen candidate input changed: {name}")


def prepare(baseline, candidate, artifact, output):
    measure.require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    measure.require(platform.system() in ("Darwin", "Linux"), "native macOS/Linux required")
    baseline = baseline.resolve()
    candidate = candidate.resolve()
    artifact = artifact.resolve()
    output = output.resolve()
    measure.require(baseline.is_file() and candidate.is_file() and artifact.is_dir(), "two runners and artifact required")
    measure.require(checksum(baseline) != checksum(candidate), "baseline and candidate runners are identical")
    measure.require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) <= 16 << 20,
                    "candidate artifact size")
    required = RATE * 90 * measure.EVENT.size * 2 * 10 + (1 << 30)
    measure.require(shutil.disk_usage(aot.ROOT).free > required, "insufficient space for ten traces")
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "frozen"
    frozen.mkdir()
    runners = {}
    for label, source in zip(LABELS, (baseline, candidate)):
        destination = frozen / f"beam_open_loop-{label}"
        shutil.copy2(source, destination)
        runners[label] = checksum(destination)
    shutil.copytree(artifact, frozen / "artifact")
    shutil.copy2(aot.ROOT / "scripts/measure_memory.c", frozen / "measure_memory.c")
    setup = {"status": "running"}
    try:
        setup["command"] = measure_memory.build(frozen / "measure_memory.c", frozen / "measure_memory")
        setup["preflight"] = measure_memory.snapshot(os.getpid(), frozen / "measure_memory", timeout=30)
        setup["status"] = "ok"
    except BaseException as error:
        setup.update({"status": "invalid_experiment", "error": str(error)})
        raise
    finally:
        (output / "memory-setup.json").write_text(json.dumps(setup, indent=2) + "\n")
    names = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
                                    cwd=aot.ROOT, timeout=5).decode().split("\0")
    for name in sorted(set(names)):
        source = aot.ROOT / name
        if name and not name.startswith("docs/proposals/") and source.is_file():
            destination = frozen / "source" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
    inputs = {str(path.relative_to(frozen)): checksum(path) for path in sorted(frozen.rglob("*")) if path.is_file()}
    environment = {
        "status": "paired_candidate_experiment_not_acceptance", "case": "hot-queue", "rate": RATE,
        "warmup_seconds": 30, "sample_seconds": 60, "paired_repetitions": 5,
        "platform": platform.platform(), "machine": platform.machine(),
        "source_base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT,
                                                       text=True, timeout=5).strip(),
        "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True, timeout=5),
        "rustc": aot.command(["rustc", "-vV"]).stdout.strip(), "runner_sha256": runners, "inputs": inputs,
        "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120,
                   "trace_bytes": measure.MAX_TRACE, "observed_rss_safety_bytes": 1 << 30},
        "host_isolation": "unverified; exploratory integration measurement, not acceptance",
        "missing_gates": ["CPU hard quota", "host isolation and monitoring bias", "complete memory",
                          "native dual-platform", "BEAM throughput and memory acceptance"],
    }
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    verify_inputs(frozen, inputs)
    os.execv(sys.executable, [sys.executable, str(frozen / "source/scripts/measure_reply_candidate.py"),
                              "--frozen-output", str(output)])


def run(output):
    frozen = output / "frozen"
    measure.require(Path(__file__).resolve() == (frozen / "source/scripts/measure_reply_candidate.py").resolve(),
                    "execute the frozen candidate driver")
    environment = json.loads((output / "environment.json").read_text())
    inputs = environment["inputs"]
    reports = []
    try:
        for repetition in range(1, 6):
            order = LABELS if repetition % 2 else tuple(reversed(LABELS))
            for label in order:
                verify_inputs(frozen, inputs)
                name = f"{repetition}-{label}"
                trace = output / f"{name}.events.bin"
                command = [frozen / f"beam_open_loop-{label}", "--trusted", frozen / "artifact",
                           "--measure", "hot-queue", RATE, trace]
                result = measure.measurement(command, trace, output / name, "hot-queue", RATE,
                                             frozen / "measure_memory", runtime="wings")
                verify_inputs(frozen, inputs)
                reports.append({"repetition": repetition, "label": label, **result})
                status = "complete" if len(reports) == 10 else "running"
                (output / "report.json").write_text(json.dumps({
                    "status": status, "runs": reports, "stable_improvement_proven": False,
                    "performance_or_memory_gate_proven": False}, indent=2) + "\n")
                print(f"{name}: tail candidate {result['summary']['tail_candidate_pass']}; not acceptance", flush=True)
    except BaseException as error:
        (output / "report.json").write_text(json.dumps({
            "status": "invalid_experiment", "error": str(error), "runs": reports,
            "stable_improvement_proven": False, "performance_or_memory_gate_proven": False}, indent=2) + "\n")
        raise


def main():
    parser = argparse.ArgumentParser(description="Frozen hot-queue baseline/candidate five-pair experiment")
    parser.add_argument("--baseline-runner", type=Path)
    parser.add_argument("--candidate-runner", type=Path)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--frozen-output", type=Path, help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if arguments.frozen_output is not None:
        measure.require(arguments.baseline_runner is None and arguments.candidate_runner is None
                        and arguments.artifact is None and arguments.output is None, "frozen candidate arguments")
        run(arguments.frozen_output.resolve())
    else:
        measure.require(arguments.baseline_runner is not None and arguments.candidate_runner is not None
                        and arguments.artifact is not None and arguments.output is not None,
                        "two runners, artifact and output required")
        prepare(arguments.baseline_runner, arguments.candidate_runner, arguments.artifact, arguments.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Reply candidate measurement failed: {error}", file=sys.stderr)
        sys.exit(3)
