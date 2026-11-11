import argparse
import json
import os
from pathlib import Path
import platform
import re
import resource
import select
import statistics
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
PROBE = ROOT / "target/release/examples/stack_probe"
COMPILER = ROOT / "target/release/wings-compile"


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (30, 30))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def run(arguments, timeout=20):
    return subprocess.check_output(arguments, text=True, timeout=timeout, cwd=ROOT)


def snapshot(pid, destination):
    rss_kib, virtual_kib = map(int, run(["ps", "-o", "rss=,vsz=", "-p", str(pid)]).split())
    summary = run(["vmmap", "-summary", str(pid)])
    physical = run(["footprint", "-f", "bytes", "--swapped", "--noCategories", str(pid)])
    destination.with_suffix(".vmmap.txt").write_text(summary)
    destination.with_suffix(".footprint.txt").write_text(physical)
    match = re.search(r"^\s+phys_footprint:\s+(\d+) B", physical, re.M)
    total = re.search(r"^TOTAL\s+(.+)$", summary, re.M).group(1).split()
    return {
        "rss_bytes": rss_kib * 1024,
        "virtual_bytes": virtual_kib * 1024,
        "physical_bytes": int(match.group(1)) if match else None,
        "regions": int(total[-1]),
    }


def measure(case, repetition, artifact, output):
    mode, count, stack_kib, frames = case
    name = f"{mode}-{count}-{stack_kib}-{frames}-{repetition}"
    child = subprocess.Popen(
        [str(PROBE), mode, str(count), str(stack_kib), str(frames), str(artifact)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=ROOT,
        preexec_fn=limit_child,
    )
    result = {"mode": mode, "count": count, "stack_kib": stack_kib, "frames": frames, "repetition": repetition}
    deadline = time.monotonic() + 60
    try:
        for phase in ["baseline", "allocated", "released"]:
            while True:
                timeout = max(0, deadline - time.monotonic())
                if not select.select([child.stdout], [], [], timeout)[0]:
                    raise TimeoutError(name)
                line = child.stdout.readline()
                if not line:
                    raise RuntimeError(f"{name}: {child.stderr.read()}")
                event = json.loads(line)
                assert event["phase"] == phase, event
                if event["switch_roundtrip_ns"]:
                    result["switch_roundtrip_ns"] = event["switch_roundtrip_ns"]
                break
            memory = snapshot(child.pid, output / f"{name}-{phase}")
            assert memory["rss_bytes"] < 384 * 1024**2, memory
            memory["elapsed_ns"] = event["elapsed_ns"]
            result[phase] = memory
            child.stdin.write("next\n")
            child.stdin.flush()
        child.wait(timeout=10)
        assert child.returncode == 0, child.stderr.read()
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "target/stack-measurements")
    parser.add_argument("--repetitions", type=int, default=3, choices=range(1, 6))
    arguments = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise SystemExit("This measurement driver currently requires macOS AArch64")
    if not PROBE.is_file() or not COMPILER.is_file():
        raise SystemExit("Build release wings-compile and wings-vm --bins --examples first")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    environment = {
        "date": run(["date", "+%Y-%m-%dT%H:%M:%S%z"]).strip(),
        "platform": platform.platform(),
        "cpu": run(["sysctl", "-n", "machdep.cpu.brand_string"]).strip(),
        "page_bytes": os.sysconf("SC_PAGE_SIZE"),
        "rustc": run(["rustc", "--version"]).strip(),
        "commit": run(["git", "rev-parse", "HEAD"]).strip(),
        "dirty": bool(run(["git", "status", "--porcelain"])),
        "limits": {"cpu_seconds": 30, "max_count": 2048, "max_stack_mapping_bytes": 2048 * (256 * 1024 + os.sysconf("SC_PAGE_SIZE")), "rss_phase_check_bytes": 384 * 1024**2, "wall_seconds": 60, "note": "macOS rejects RLIMIT_AS/DATA/RSS here; bounded allocation, not an OS memory quota or OOM test"},
    }
    results = []
    with tempfile.TemporaryDirectory(prefix="wings-stack-measure-") as temporary:
        parent = Path(temporary)
        source = parent / "live.wings"
        bindings = "\n".join(f"let value_{index} = seed + {index}" for index in range(48))
        total = " + ".join(f"value_{index}" for index in range(48))
        source.write_text(f"process Live {{ state count: Int = 0; on Work(seed: Int) -> Int {{ {bindings}\n let total = {total}; count = count + total; total }} on Get -> Int {{ count }} }}")
        artifact = parent / "artifact"
        subprocess.run([str(COMPILER), "build", str(source), "-o", str(artifact)], check=True, timeout=30, preexec_fn=limit_child)
        cases = []
        for count in [512, 2048]:
            for stack_kib in [32, 64, 256]:
                for mode in ["unstarted", "suspended"]:
                    cases.append((mode, count, stack_kib, 0))
            for stack_kib, frames in [(32, 2), (64, 8), (256, 16)]:
                cases.append(("suspended", count, stack_kib, frames))
            cases.extend([(mode, count, 256, 0) for mode in ["vm-idle", "vm-suspended"]])
        cases.extend(("switch", 1, stack_kib, 0) for stack_kib in [32, 64, 256])
        for repetition in range(arguments.repetitions):
            for case in cases:
                result = measure(case, repetition, artifact, output)
                results.append(result)
                (output / "results.json").write_text(json.dumps({"environment": environment, "results": results}, indent=2))
                physical = result["allocated"]["physical_bytes"] - result["baseline"]["physical_bytes"]
                print(f"{case} repeat={repetition}: delta_physical={physical} bytes", flush=True)
    groups = {}
    for result in results:
        key = (result["mode"], result["count"], result["stack_kib"], result["frames"])
        groups.setdefault(key, []).append(result)
    summary = []
    for key, values in groups.items():
        entry = dict(zip(["mode", "count", "stack_kib", "frames"], key))
        for metric in ["rss_bytes", "virtual_bytes", "physical_bytes", "regions"]:
            for phase in ["allocated", "released"]:
                deltas = [value[phase][metric] - value["baseline"][metric] for value in values]
                entry[f"{phase}_{metric}"] = {"median": statistics.median(deltas), "min": min(deltas), "max": max(deltas)}
        for phase in ["allocated", "released"]:
            timings = [value[phase]["elapsed_ns"] / key[1] for value in values]
            entry[f"{phase}_ns_per_item"] = statistics.median(timings)
        if "switch_roundtrip_ns" in values[0]:
            entry["switch_roundtrip_ns"] = statistics.median(value["switch_roundtrip_ns"] for value in values)
        summary.append(entry)
    (output / "summary.json").write_text(json.dumps({"environment": environment, "summary": summary}, indent=2))
    print(f"Saved raw snapshots, results and summary to {output}")


if __name__ == "__main__":
    main()
