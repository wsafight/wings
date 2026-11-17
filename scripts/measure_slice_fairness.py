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
from verify_open_loop import require
from verify_slice_fairness import MAX_TRACE


def replay_frozen(frozen, trace, destination, result):
    payload = destination.with_suffix(".result.json")
    with payload.open("x") as output:
        json.dump(result, output)
        output.write("\n")
    report = destination.with_suffix(".replay.json")
    errors = destination.with_suffix(".replay.stderr.txt")
    child = None
    try:
        with report.open("xb") as stdout, errors.open("xb") as stderr:
            child = subprocess.Popen(
                [sys.executable, str(frozen / "source/scripts/verify_slice_fairness.py"),
                 "--trace", str(trace), "--result", str(payload)],
                stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                start_new_session=True, preexec_fn=measure.limit_child,
            )
            child.wait(timeout=130)
            require(child.returncode == 0, f"frozen resume replay exit {child.returncode}; see {errors}")
    finally:
        if child is not None:
            measure.stop(child, child.poll() is None)
    require(report.stat().st_size <= 1024**2 and errors.stat().st_size == 0, "replay output limit")
    return json.loads(report.read_text())


def main():
    parser = argparse.ArgumentParser(description="Five independent Wings resume diagnostics, original 30s/60s business; not BEAM acceptance")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--observe", action="store_true", help="also enable the independent atomic-request observer")
    arguments = parser.parse_args()
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    executable = aot.TARGET / "release/examples/beam_fairness"
    artifact = arguments.artifact.resolve()
    require(executable.is_file() and artifact.is_dir(), "build release runner and artifact first")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    require(shutil.disk_usage(output).free > MAX_TRACE * 5 + 1024**3, "insufficient disk for bounded raw traces")
    frozen = output / "frozen"
    frozen.mkdir()
    shutil.copy2(executable, frozen / "beam_fairness")
    require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) < 16 << 20, "artifact bytes")
    shutil.copytree(artifact, frozen / "artifact")
    tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT).decode().split("\0")
    for name in sorted(set(tracked)):
        if name and not name.startswith("docs/proposals/") and (aot.ROOT / name).is_file():
            destination = frozen / "source" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(aot.ROOT / name, destination)
    shutil.copy2(aot.ROOT / "scripts/measure_memory.c", frozen / "measure_memory.c")
    probe = frozen / "measure_memory"
    memory_build = measure_memory.build(frozen / "measure_memory.c", probe)
    preflight = measure_memory.snapshot(os.getpid(), probe)
    (output / "memory-preflight.json").write_text(json.dumps(preflight, indent=2) + "\n")
    environment = {
        "version": 13, "platform": platform.platform(), "machine": platform.machine(),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True).strip(),
        "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "cpu": (subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
                if platform.system() == "Darwin" else Path("/proc/cpuinfo").read_text()),
        "page_bytes": os.sysconf("SC_PAGE_SIZE"), "warmup_seconds": 30, "sample_seconds": 60,
        "repetitions": 5, "runtimes": ["wings"], "slice_fifo": 1024,
        "atomic_observation_enabled": arguments.observe, "memory_probe_build": memory_build,
        "inputs": {str(path.relative_to(frozen)): hashlib.sha256(path.read_bytes()).hexdigest()
                   for path in sorted(frozen.rglob("*")) if path.is_file()},
        "host_isolation": "unverified; exploratory instrumented diagnostics, not acceptance",
        "unrecorded_environment": ["thermal state", "power mode", "QoS", "core assignment"],
        "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120,
                   "file_bytes": measure.MAX_TRACE, "protocol_trace_bytes": MAX_TRACE,
                   "observed_rss_stop_bytes": measure.MAX_RSS,
                   "offline_replay_cpu_seconds": 240, "offline_replay_wall_seconds": 130},
        "missing_gates": ["OS CPU hard quota", "monitoring overhead acceptance", "fairness overload groups",
                          "native dual-platform", "same-workload BEAM throughput and memory acceptance"],
    }
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    results = []
    for repetition in range(1, 6):
        name = f"{repetition}-wings"
        trace = output / f"{name}.events.bin"
        destination = output / name
        command = [frozen / "beam_fairness", "--trusted", frozen / "artifact", "--measure-slices-bounded", trace]
        if arguments.observe:
            command.append("--observe")
        result = measure.measurement(
            command, trace, destination, "resume-fairness", 10000, probe, runtime="wings",
            validator=lambda path, _name, _rate, final: replay_frozen(frozen, path, destination, final),
        )
        results.append({"repetition": repetition, **result["summary"],
                        "platform_memory": result["platform_memory"],
                        "rss_tree_plus_driver_peak_bytes": result["rss_tree_plus_driver_peak_bytes"]})
        require(all(hashlib.sha256((frozen / filename).read_bytes()).hexdigest() == digest
                    for filename, digest in environment["inputs"].items()), "frozen input changed")
        (output / "report.json").write_text(json.dumps({"runs": results,
            "fairness_gate_proven": False, "memory_gate_proven": False, "BEAM_comparison": False}, indent=2) + "\n")
        print(f"{name}: complete resume trace {result['summary']['slice_observation']['complete']}; no fairness/MVP gate", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Resume fairness measurement failed: {error}", file=sys.stderr)
        sys.exit(3)
