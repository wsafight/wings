import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import selectors
import shutil
import signal
import subprocess
import sys
import time

import measure_open_loop as measure
import verify_concurrent_failure as failure
import verify_resource_receipt as receipts
import verify_scalar_aot as aot
from verify_counter_equivalence import strict_object
from verify_open_loop import require


TRACE_BYTES = 64 << 20
PERIOD_NS = 100_000_000


def number(value, low=0, high=2**64 - 1):
    require(type(value) is int and low <= value <= high, "failure soak integer")
    return value


def decode_json(payload):
    def invalid_constant(value):
        raise ValueError(f"non-JSON constant: {value}")
    return json.loads(payload, object_pairs_hook=strict_object, parse_constant=invalid_constant)


def validate_workload(workload):
    failure.decode(json.dumps(workload).encode())
    require(all(workload["streams"]), "two nonempty failure soak streams")
    failed = {item["actor"] for stream in workload["streams"] for item in stream if item["fail"]}
    require(bool(failed), "injected failure required")
    require(any(item["actor"] not in failed for stream in workload["streams"] for item in stream), "healthy traffic required")


def workload(length=4096):
    return failure.config(length, 3, -7, [[(1 + index % 3, sender + 1, index == 18)
                                         for index in range(128)] for sender in range(2)])


def environment(value, runtime):
    require(type(value) is dict and value.get("runtime") == runtime, "failure soak runtime")
    number(value.get("senders"), 2, 2)
    number(value.get("window_per_sender"), 16, 16)
    if runtime == "wings":
        number(value.get("workers"), 2, 2)
    else:
        require(value.get("otp") == "29", "failure soak OTP")
        number(value.get("schedulers"), 2, 2)


def replay(path, workload, events, runtime, *, soak):
    validate_workload(workload)
    require(runtime in ("wings", "beam") and type(soak) is bool, "failure soak mode")
    require(type(events) is list and len(events) == (3 if soak else 2), "failure soak control count")
    start, final = events[0], events[-1]
    count = 900 if soak else 3
    mode = "soak" if soak else "verify"
    for key, expected in {"version": 10, "event": "start", "mode": mode, "epochs": count,
                          "warmup_seconds": 30 if soak else 0, "sample_seconds": 60 if soak else 0,
                          "period_ns": PERIOD_NS if soak else 0, "round_deadline_ns": 3_000_000_000,
                          "trace_limit_bytes": TRACE_BYTES}.items():
        require(type(start.get(key)) is type(expected) and start[key] == expected, f"start {key}")
    environment(start.get("environment"), runtime)
    if runtime == "wings":
        cold = number(start.get("cold_bytes"), 1)
        slot = number(start.get("stack_slot_bytes"), 4096)
        number(start.get("max_cached_stacks"), 4, 4)
    else:
        require(all(start.get(key) is None for key in ("cold_bytes", "stack_slot_bytes", "max_cached_stacks")), "BEAM logical accounting must remain null")
    for key, expected in {"version": 10, "event": "complete", "status": "ok", "mode": mode, "epochs": count}.items():
        require(type(final.get(key)) is type(expected) and final[key] == expected, f"final {key}")
    duration = number(final.get("measurement_elapsed_ns"), 90_000_000_000 if soak else 0, 119_999_999_999)
    if soak:
        sample = events[1]
        require(sample.get("version") == 10 and type(sample["version"]) is int and sample.get("event") == "sampling", "sampling event")
        number(sample.get("elapsed_ns"), 30_000_000_000, duration)
    if runtime == "wings":
        receipts.validate(final)
    else:
        require(final.get("actors_and_senders_terminated") is True, "BEAM cleanup")
    size = path.stat().st_size
    require(size <= TRACE_BYTES and number(final.get("trace_bytes")) == size, "failure soak trace size")
    previous_end = 0
    stale_total = 0
    digest = hashlib.sha256()
    totals = {phase: {"epochs": 0, "reply": 0, "failed": 0, "closed": 0, "injected_failures": 0}
              for phase in ("warmup", "sample")}
    keys = {"version", "epoch", "planned_ns", "started_ns", "completed_ns", "stale_identities_checked",
            "retained_bytes", "retained_processes", "retained_messages", "retained_requests", "cached_stacks", "result"}
    if runtime == "beam":
        keys |= {"beam_process_count", "beam_port_count"}
    epochs = 0
    with path.open("rb") as source:
        while line := source.readline((1 << 20) + 1):
            require(len(line) <= 1 << 20 and line.endswith(b"\n"), "failure soak line bound/completeness")
            digest.update(line)
            record = decode_json(line)
            epochs += 1
            require(epochs <= count and type(record) is dict and set(record) == keys, "failure soak epoch fields")
            number(record["version"], 10, 10)
            number(record["epoch"], epochs, epochs)
            planned = (epochs - 1) * PERIOD_NS if soak else 0
            number(record["planned_ns"], planned, planned)
            started = number(record["started_ns"], max(planned, previous_end), duration)
            ended = number(record["completed_ns"], started, min(duration, started + 3_000_000_000))
            previous_end = ended
            checked = workload["actors"] * (1 if epochs == 1 else 2)
            stale_total += number(record["stale_identities_checked"], checked, checked)
            if runtime == "wings":
                cached = number(record["cached_stacks"], 0, 4)
                number(record["retained_bytes"], cold + cached * slot, cold + cached * slot)
                for field in ("retained_processes", "retained_messages", "retained_requests"):
                    number(record[field], 0, 0)
            else:
                require(all(record[key] is None for key in ("retained_bytes", "retained_processes", "retained_messages", "retained_requests", "cached_stacks")), "BEAM accounting attribution")
                number(record["beam_process_count"], 1)
                number(record["beam_port_count"])
            failure.check(workload, record["result"])
            environment(record["result"].get("environment"), runtime)
            phase = "warmup" if soak and planned < 30_000_000_000 else "sample"
            group = totals[phase]
            group["epochs"] += 1
            group["injected_failures"] += len(record["result"]["failures"])
            for outcome in record["result"]["outcomes"]:
                group[outcome["status"]] += 1
    require(epochs == count, "missing failure soak epochs")
    number(final.get("stale_identities_checked"), stale_total, stale_total)
    require(totals["warmup"]["epochs"] == (300 if soak else 0) and totals["sample"]["epochs"] == (600 if soak else 3), "failure soak phase accounting")
    return {"epochs": epochs, "phases": totals, "stale_identities_checked": stale_total,
            "trace_bytes": size, "sha256": digest.hexdigest(),
            "persistent_failure_lifecycle_pass": soak,
            "complete_concurrent_fault_gate_proven": False, "performance_or_memory_gate_proven": False}


def limits(soak):
    def apply():
        for field, maximum in ((resource.RLIMIT_CPU, 240 if soak else 5), (resource.RLIMIT_FSIZE, TRACE_BYTES)):
            current = [value for value in resource.getrlimit(field) if value != resource.RLIM_INFINITY]
            bound = min([maximum, *current])
            resource.setrlimit(field, (bound, bound))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    return apply


def trial(command, payload, trace, directory, workload, runtime, *, soak):
    directory.mkdir(exist_ok=False)
    manifest = {"status": "running", "command": [str(item) for item in command], "runtime": runtime, "soak": soak}
    report = directory / "run.json"
    report.write_text(json.dumps(manifest, indent=2) + "\n")
    env = os.environ.copy()
    for name in ("ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "ERL_LIBS"):
        env.pop(name, None)
    env["ERL_CRASH_DUMP"] = os.devnull
    env["ERL_CRASH_DUMP_SECONDS"] = "0"
    child = None
    selector = selectors.DefaultSelector()
    started = time.monotonic()
    wall = 130 if soak else 10
    events = []
    buffer = b""
    output_bytes = 0
    next_sample = started
    try:
        with (directory / "stderr.txt").open("xb") as errors, (directory / "stdout.jsonl").open("xb") as output, (directory / "rss.jsonl").open("x") as observations:
            child = subprocess.Popen([str(item) for item in command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=errors, env=env, cwd=aot.ROOT, start_new_session=True, preexec_fn=limits(soak))
            child.stdin.write(payload)
            child.stdin.close()
            selector.register(child.stdout, selectors.EVENT_READ)
            os.set_blocking(child.stdout.fileno(), False)
            while selector.get_map():
                now = time.monotonic()
                require(now - started < wall, "failure soak wall deadline")
                if now >= next_sample:
                    sample = measure.memory_snapshot(os.getpid())
                    sample["elapsed_seconds"] = now - started
                    observations.write(json.dumps(sample) + "\n")
                    observations.flush()
                    require(sample["rss_bytes"] <= 1 << 30, "failure soak RSS safety stop; not memory acceptance")
                    next_sample = now + 1
                for key, _ in selector.select(timeout=0.05):
                    block = os.read(key.fileobj.fileno(), 65536)
                    if not block:
                        selector.unregister(key.fileobj)
                        continue
                    output_bytes += len(block)
                    require(output_bytes <= 65536, "failure soak control output bound")
                    output.write(block)
                    output.flush()
                    buffer += block
                    while b"\n" in buffer:
                        line, buffer = buffer.split(b"\n", 1)
                        require(len(events) < 3, "failure soak extra control event")
                        events.append(decode_json(line))
                require((directory / "stderr.txt").stat().st_size <= 65536, "failure soak stderr bound")
            child.wait(timeout=max(0.001, wall - (time.monotonic() - started)))
        require(child.returncode == 0 and (directory / "stderr.txt").stat().st_size == 0, f"failure soak child exit {child.returncode}")
        require(not buffer, "failure soak partial control line")
        manifest.update({"status": "ok", "summary": replay(trace, workload, events, runtime, soak=soak), "events": events})
    except BaseException as error:
        manifest.update({"status": "invalid_experiment", "error": str(error), "events": events})
        raise
    finally:
        if child is not None:
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            child.wait()
            child.stdout.close()
            if not child.stdin.closed:
                try:
                    child.stdin.close()
                except BrokenPipeError:
                    pass
        selector.close()
        manifest["wall_seconds"] = time.monotonic() - started
        report.write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1 << 20):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description="Persistent two-sender failure lifecycle: fixed 30s warmup/60s sample, no complete fault/performance acceptance")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--state-bytes", type=int, choices=(256, 4096, 65536), default=4096)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    escript = shutil.which("escript") if arguments.compare_beam else None
    require(not arguments.compare_beam or escript is not None, "OTP 29 required; no fallback")
    runner = aot.TARGET / "release/examples/beam_failure_soak"
    artifact = arguments.artifact.resolve()
    require(runner.is_file() and artifact.is_dir(), "build runner and artifact first")
    require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) <= 16 << 20, "failure soak artifact size")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "frozen"
    frozen.mkdir()
    shutil.copy2(runner, frozen / "beam_failure_soak")
    shutil.copytree(artifact, frozen / "artifact")
    files = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT).decode().split("\0")
    for name in sorted(set(files)):
        if name and not name.startswith("docs/proposals/") and (aot.ROOT / name).is_file():
            target = frozen / "source" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(aot.ROOT / name, target)
    config = workload(arguments.state_bytes)
    validate_workload(config)
    (output / "workload.json").write_text(json.dumps(config, indent=2) + "\n")
    (frozen / "input.wings.bin").write_bytes(failure.bridge(config))
    (frozen / "input.beam.json").write_text(json.dumps(config) + "\n")
    inputs = {str(path.relative_to(frozen)): checksum(path) for path in sorted(frozen.rglob("*")) if path.is_file()}
    metadata = {"commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True).strip(),
                "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True),
                "platform": platform.platform(), "machine": platform.machine(),
                "rustc": subprocess.check_output(["rustc", "-vV"], text=True).strip(), "inputs": inputs,
                "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120,
                           "round_seconds": 3, "trace_bytes": TRACE_BYTES, "observed_rss_safety_bytes": 1 << 30},
                "warmup_seconds": 30, "sample_seconds": 60, "epochs": 900,
                "missing_gates": ["CPU hard quota", "host isolation and power/thermal state", "complete platform memory and monitoring overhead",
                                  "long timeout/cancel/automatic restart/overload races", "native dual-platform", "BEAM throughput and memory acceptance"]}
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    commands = [("wings", [frozen / "beam_failure_soak", "--trusted", frozen / "artifact"], (frozen / "input.wings.bin").read_bytes())]
    if arguments.compare_beam:
        commands.append(("beam", [escript, frozen / "source/scripts/beam_failure_soak.escript"], (frozen / "input.beam.json").read_bytes()))
    results = []
    for runtime, command, payload in commands:
        require(all(checksum(frozen / name) == digest for name, digest in inputs.items()), "frozen failure soak input changed")
        trace = output / f"{runtime}.epochs.jsonl"
        result = trial([*command, "--soak", trace], payload, trace, output / runtime, config, runtime, soak=True)
        require(all(checksum(frozen / name) == digest for name, digest in inputs.items()), "frozen failure soak input changed")
        results.append({"runtime": runtime, **result["summary"]})
        (output / "report.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{runtime}: {result['summary']['epochs']} persistent failure epochs replayed; no complete fault/performance/memory gate", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Failure soak experiment failed: {error}", file=sys.stderr)
        sys.exit(3)
