import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys

import measure_failure_soak as driver
import verify_resource_receipt as receipts
import verify_scalar_aot as aot
from verify_open_loop import require


def exact(value, expected, label):
    require(type(value) is type(expected), label)
    if type(expected) is dict:
        require(set(value) == set(expected), label)
        for key, item in expected.items():
            exact(value[key], item, f"{label} {key}")
    else:
        require(value == expected, label)


def resources(value):
    require(type(value) is dict and set(value) == {"bytes", "processes", "messages", "requests"}, "recovery resource fields")
    for key, maximum in {"bytes": 128 << 20, "processes": 8, "messages": 64, "requests": 64}.items():
        driver.number(value[key], 0, maximum)


def result(value, runtime="wings"):
    require(runtime in ("wings", "beam"), "recovery result runtime")
    keys = {"state_bytes", "queued", "retained_request_slots", "mailbox_full", "before_rejected",
            "after_rejected", "healthy_committed", "stop_restarted", "outcomes", "failures"}
    require(type(value) is dict and set(value) == keys, "recovery result fields")
    for key, expected in {"state_bytes": 4096, "queued": 16, "retained_request_slots": 16,
                          "mailbox_full": True, "healthy_committed": 7, "stop_restarted": False}.items():
        exact(value[key], expected, f"recovery {key}")
    if runtime == "wings":
        resources(value["before_rejected"])
        resources(value["after_rejected"])
        driver.number(value["before_rejected"]["processes"], 2, 2)
        driver.number(value["before_rejected"]["messages"], 17, 17)
    else:
        for field in ("before_rejected", "after_rejected"):
            exact(value[field], {"queued": 16, "requests": 16}, "BEAM protocol counters")
    require(value["before_rejected"] == value["after_rejected"], "rejected request changed resources")
    driver.number(value["before_rejected"]["requests"], 16, 16)
    require(type(value["outcomes"]) is list and len(value["outcomes"]) == 16, "recovery outcome count")
    seen = set()
    counts = {"timed_out": 0, "cancelled": 0, "failed": 0}
    for outcome in value["outcomes"]:
        require(type(outcome) is dict and set(outcome) == {"sender", "seq", "status", "consumed_once"}, "recovery outcome fields")
        sender = driver.number(outcome["sender"], 1, 2)
        sequence = driver.number(outcome["seq"], 1, 8)
        require((sender, sequence) not in seen, "duplicate recovery outcome")
        seen.add((sender, sequence))
        require(outcome["consumed_once"] is True, "repeated recovery consumption")
        expected = {"timed_out"} if sequence <= 2 else {"cancelled"} if sequence <= 4 else {"cancelled", "failed"}
        require(outcome["status"] in expected, "recovery terminal outcome")
        counts[outcome["status"]] += 1
    require(type(value["failures"]) is list and len(value["failures"]) == 2, "recovery failure count")
    for attempt, failure in enumerate(value["failures"], 1):
        expected = {"attempt": attempt, "handler": "fail", "reason": "division_by_zero", "state_bytes": 4096,
                    "committed": 0, "restart": "restarted" if attempt == 1 else "intensity_exceeded",
                    "next_initial": 0 if attempt == 1 else None}
        require(type(failure) is dict and set(failure) == {*expected, "status"}, "recovery failure fields")
        for key, item in expected.items():
            exact(failure[key], item, f"recovery failure {key}")
        require(failure["status"] in ("cancelled", "failed"), "cancelled failure was retracted")
    return counts


def replay(path, config, events, runtime, *, soak):
    require(runtime in ("wings", "beam") and type(soak) is bool, "recovery runtime/mode")
    workers = driver.number(config["workers"], 1, 2)
    count = 900 if soak else 3
    mode = "soak" if soak else "verify"
    require(type(events) is list and len(events) == (3 if soak else 2), "recovery control count")
    start, final = events[0], events[-1]
    for key, expected in {"version": 11, "event": "start", "mode": mode, "epochs": count,
                          "warmup_seconds": 30 if soak else 0, "sample_seconds": 60 if soak else 0,
                          "period_ns": driver.PERIOD_NS if soak else 0, "round_deadline_ns": 3_000_000_000,
                          "trace_limit_bytes": driver.TRACE_BYTES,
                          "max_cached_stacks": workers * 2 if runtime == "wings" else None}.items():
        exact(start.get(key), expected, f"recovery start {key}")
    environment = {"runtime": runtime, "senders": 2, "mailbox": 16, "result_slots": 17, "state_bytes": 4096}
    if runtime == "wings":
        environment["workers"] = workers
        cold = driver.number(start.get("cold_bytes"), 1, 128 << 20)
        slot = driver.number(start.get("stack_slot_bytes"), 4096)
    else:
        environment.update({"schedulers": workers, "otp": "29"})
        exact(start.get("cold_bytes"), None, "BEAM cold accounting")
        exact(start.get("stack_slot_bytes"), None, "BEAM stack accounting")
    exact(start.get("environment"), environment, "recovery environment")
    for key, expected in {"version": 11, "event": "complete", "status": "ok", "mode": mode, "epochs": count}.items():
        exact(final.get(key), expected, f"recovery final {key}")
    duration = driver.number(final.get("measurement_elapsed_ns"), 90_000_000_000 if soak else 0, 119_999_999_999)
    if soak:
        exact(events[1].get("version"), 11, "recovery sampling version")
        exact(events[1].get("event"), "sampling", "recovery sampling event")
        driver.number(events[1].get("elapsed_ns"), 30_000_000_000, duration)
    if runtime == "wings":
        receipts.validate(final)
    else:
        exact(final.get("actors_and_senders_terminated"), True, "BEAM process cleanup")
        exact(final.get("protocol_slots_released"), True, "BEAM protocol cleanup")
    size = path.stat().st_size
    require(size <= driver.TRACE_BYTES, "recovery trace size")
    driver.number(final.get("trace_bytes"), size, size)
    phases = {phase: {"epochs": 0, "timed_out": 0, "cancelled": 0, "failed": 0,
                      "automatic_restarts": 0, "intensity_exceeded": 0} for phase in ("warmup", "sample")}
    digest = hashlib.sha256()
    previous = 0
    checks = 0
    epochs = 0
    maximum = 0
    keys = {"version", "epoch", "planned_ns", "started_ns", "completed_ns", "stale_identities_checked",
            "retained", "cached_stacks", "result"}
    if runtime == "beam":
        keys |= {"live_protocol_slots", "beam_process_count", "beam_port_count"}
    with path.open("rb") as source:
        while line := source.readline((1 << 20) + 1):
            require(len(line) <= 1 << 20 and line.endswith(b"\n"), "recovery bounded complete line")
            digest.update(line)
            row = driver.decode_json(line)
            epochs += 1
            require(epochs <= count and type(row) is dict and set(row) == keys, "recovery epoch fields")
            driver.number(row["version"], 11, 11)
            driver.number(row["epoch"], epochs, epochs)
            planned = (epochs - 1) * driver.PERIOD_NS if soak else 0
            driver.number(row["planned_ns"], planned, planned)
            started = driver.number(row["started_ns"], max(planned, previous), duration)
            ended = driver.number(row["completed_ns"], started, min(duration, started + 3_000_000_000))
            maximum = max(maximum, ended - started)
            previous = ended
            stale = 4 if epochs == 1 else 8
            checks += driver.number(row["stale_identities_checked"], stale, stale)
            if runtime == "wings":
                resources(row["retained"])
                cached = driver.number(row["cached_stacks"], 0, workers * 2)
                driver.number(row["retained"]["bytes"], cold + cached * slot, cold + cached * slot)
                for key in ("processes", "messages", "requests"):
                    driver.number(row["retained"][key], 0, 0)
            else:
                exact(row["retained"], None, "BEAM retained accounting")
                exact(row["cached_stacks"], None, "BEAM cache accounting")
                driver.number(row["live_protocol_slots"], 0, 0)
                driver.number(row["beam_process_count"], 1, 1024)
                driver.number(row["beam_port_count"], 0, 1024)
            totals = result(row["result"], runtime)
            group = phases["warmup" if soak and planned < 30_000_000_000 else "sample"]
            group["epochs"] += 1
            group["automatic_restarts"] += 1
            group["intensity_exceeded"] += 1
            for key, amount in totals.items():
                group[key] += amount
    require(epochs == count, "recovery missing epoch")
    driver.number(final.get("stale_identities_checked"), checks, checks)
    require(phases["warmup"]["epochs"] == (300 if soak else 0) and phases["sample"]["epochs"] == (600 if soak else 3), "recovery phase accounting")
    return {"epochs": epochs, "phases": phases, "stale_identities_checked": checks,
            "trace_bytes": size, "sha256": digest.hexdigest(), "max_epoch_ns": maximum,
            "persistent_recovery_lifecycle_pass": soak, "complete_concurrent_fault_gate_proven": False,
            "performance_or_memory_gate_proven": False}


def beam_command(workers, script, mode, trace):
    driver.number(workers, 1, 2)
    require(mode in ("--verify-reuse", "--soak"), "BEAM recovery mode")
    escript, env = shutil.which("escript"), shutil.which("env")
    require(escript is not None and env is not None, "OTP 29 and env required; no fallback")
    return [env, f"ERL_FLAGS=+S {workers}:{workers}", escript, script, mode, workers, trace]


def main():
    parser = argparse.ArgumentParser(description="Persistent cancellation and automatic recovery: fixed 30s/60s, 900 epochs per worker count")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    if arguments.compare_beam:
        beam_command(1, "", "--soak", "")
    runner = aot.TARGET / "release/examples/concurrent_recovery"
    artifact = arguments.artifact.resolve()
    require(runner.is_file() and artifact.is_dir(), "build recovery runner and artifact first")
    require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) <= 16 << 20, "recovery artifact size")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "frozen"
    frozen.mkdir()
    shutil.copy2(runner, frozen / "concurrent_recovery")
    shutil.copytree(artifact, frozen / "artifact")
    files = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT).decode().split("\0")
    for name in sorted(set(files)):
        if name and not name.startswith("docs/proposals/") and (aot.ROOT / name).is_file():
            target = frozen / "source" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(aot.ROOT / name, target)
    inputs = {str(path.relative_to(frozen)): driver.checksum(path) for path in sorted(frozen.rglob("*")) if path.is_file()}
    metadata = {"commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True).strip(),
                "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True),
                "platform": platform.platform(), "machine": platform.machine(), "inputs": inputs,
                "rustc": subprocess.check_output(["rustc", "-vV"], text=True).strip(),
                "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120, "round_seconds": 3,
                           "trace_bytes": driver.TRACE_BYTES, "observed_rss_safety_bytes": 1 << 30},
                "warmup_seconds": 30, "sample_seconds": 60, "epochs": 900,
                "compare_beam": arguments.compare_beam,
                "missing_gates": ["long overload and service I/O races", "CPU hard quota",
                                  "complete memory and monitoring overhead", "native dual-platform", "BEAM throughput and memory acceptance"]}
    if not arguments.compare_beam:
        metadata["missing_gates"].append("BEAM recovery comparison not requested")
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    reports = []
    for workers in (1, 2):
        for runtime in (("wings", "beam") if arguments.compare_beam else ("wings",)):
            require(all(driver.checksum(frozen / name) == digest for name, digest in inputs.items()), "frozen recovery input changed")
            name = f"{runtime}-workers-{workers}" if arguments.compare_beam else f"workers-{workers}"
            trace = output / f"{name}.epochs.jsonl"
            command = ([frozen / "concurrent_recovery", "--trusted", frozen / "artifact", "--soak", workers, trace]
                       if runtime == "wings" else beam_command(workers, frozen / "source/scripts/beam_recovery_soak.escript", "--soak", trace))
            trial = driver.trial(command, b"", trace, output / name, {"workers": workers}, runtime, soak=True, replayer=replay)
            require(all(driver.checksum(frozen / name) == digest for name, digest in inputs.items()), "frozen recovery input changed")
            reports.append({"runtime": runtime, "workers": workers, **trial["summary"]})
            (output / "report.json").write_text(json.dumps(reports, indent=2) + "\n")
            print(f"{runtime} workers={workers}: 900 persistent cancellation/recovery epochs replayed; no complete fault/performance/memory gate", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Recovery soak experiment failed: {error}", file=sys.stderr)
        sys.exit(3)
