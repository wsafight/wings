import argparse
from array import array
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import selectors
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import time

import verify_scalar_aot as aot
import verify_resource_receipt as receipts
import measure_memory
import measure_cpu
from verify_open_loop import CASES, DEADLINE_NS, integer, require, target


EVENT = struct.Struct(">QQQqQ")
WARMUP_NS = 30_000_000_000
MAX_TRACE = 10 * 1024**3
MAX_RSS = 1024**3
MAILBOX_FULL_ATTRIBUTION_LIMIT = 1024


def new_mailbox_full_attribution():
    return {"version": 1, "retained": 0, "total": 0, "truncated": False, "events": []}


def attribute_mailbox_full(attribution, sequence, actor, phase, planned, sent, previous_sent):
    attribution["total"] += 1
    if attribution["retained"] == MAILBOX_FULL_ATTRIBUTION_LIMIT:
        attribution["truncated"] = True
        return
    attribution["events"].append({
        "sequence": sequence,
        "actor": actor,
        "phase": phase,
        "planned_ns": planned,
        "sent_ns": sent,
        "generator_lateness_ns": sent - planned,
        "previous_arrival_gap_ns": sent - previous_sent if sequence > 1 else None,
    })
    attribution["retained"] += 1


def trace_events(path):
    with path.open("rb") as source:
        while block := source.read(EVENT.size * 16384):
            yield from EVENT.iter_unpack(block)


def statistics_counter():
    return {"samples": 0, "max_ns": None, "over_20ms": 0, "over_50ms": 0}


statistics_fn = statistics_counter
# Keep measure.statistics() alias for verify_measurement.py callers
def statistics():
    return statistics_counter()


def observe(stats, value):
    stats["samples"] += 1
    stats["max_ns"] = max(stats["max_ns"] or 0, value)
    stats["over_20ms"] += value > 20_000_000
    stats["over_50ms"] += value > 50_000_000


def percentiles(path, rate, latency, lateness, service, warmup_ns=WARMUP_NS):
    queries = []
    for metric, stats in enumerate((latency, lateness, service)):
        for field, numerator, denominator in (("p50_ns", 1, 2), ("p99_ns", 99, 100), ("p999_ns", 999, 1000)):
            stats[field] = None
            if stats["samples"] and (field != "p999_ns" or stats["samples"] >= 10000):
                rank = (stats["samples"] * numerator + denominator - 1) // denominator
                queries.append({"metric": metric, "stats": stats, "field": field, "rank": rank, "prefix": 0})
    for shift in (32, 16, 0):
        buckets = {(query["metric"], query["prefix"]): array("Q", [0]) * 65536 for query in queries}
        for sequence, sent, observed, _value, kind in trace_events(path):
            planned = (sequence - 1) * 1_000_000_000 // rate
            if planned < warmup_ns:
                continue
            values = (((0, observed - planned), (2, observed - sent))
                      if kind in (1, 2) else ((1, sent - planned),))
            for metric, value in values:
                key = (metric, value >> (shift + 16))
                if key in buckets:
                    buckets[key][value >> shift & 65535] += 1
        for query in queries:
            counts = buckets[query["metric"], query["prefix"]]
            for bucket, count in enumerate(counts):
                if query["rank"] <= count:
                    query["prefix"] = query["prefix"] << 16 | bucket
                    break
                query["rank"] -= count
            else:
                raise ValueError("missing quantile bucket")
    for query in queries:
        query["stats"][query["field"]] = query["prefix"]


def validate_trace(path, name, rate, result, *, warmup_seconds=30, sample_seconds=60):
    total_seconds = warmup_seconds + sample_seconds
    warmup_ns = warmup_seconds * 1_000_000_000
    stage_limit_ns = (total_seconds + 30) * 1_000_000_000
    actors, length, capacity, _ = CASES[name]
    for field, expected in {"version": 6, "status": "ok", "mode": "measure", "actors": actors,
                            "state_bytes": length, "mailbox_capacity": capacity, "rate": rate,
                            "deadline_ns": DEADLINE_NS, "pending_capacity": 256, "message_bytes": 32,
                            "states_verified": True, "samples": []}.items():
        require(type(result.get(field)) is type(expected) and result[field] == expected, field)
    runtime = result["environment"]["runtime"]
    require(runtime in ("wings", "beam"), "runtime")
    if runtime == "wings":
        receipts.validate(result, historical=True)
        require(result.get("resources_released") is True and result["environment"]["workers"] == 2, "Wings cleanup/workers")
    else:
        require(result["environment"]["otp"] in ("27", "28", "29") and result["environment"]["schedulers"] == 2, "OTP/schedulers")
    require(path.stat().st_size <= MAX_TRACE and path.stat().st_size % EVENT.size == 0, "trace bytes")
    pending = {}
    expected = [0] * actors
    observed_values = [0] * actors
    counts = {"reply": 0, "timed_out": 0, "mailbox_full": 0, "pending_full": 0}
    warmup = dict(counts)
    latency = statistics_counter()
    lateness = statistics_counter()
    service = statistics_counter()
    attribution = new_mailbox_full_attribution()
    next_sequence = 1
    previous_sent = 0
    previous_observed = 0
    previous_event_time = 0
    events = 0
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(EVENT.size * 16384):
            digest.update(block)
            for sequence, sent, observed, value, kind in EVENT.iter_unpack(block):
                events += 1
                require(1 <= sequence <= rate * total_seconds, "sequence bounds")
                actor = target(name, sequence - 1) - 1
                planned = (sequence - 1) * 1_000_000_000 // rate
                group = counts if planned >= warmup_ns else warmup
                if kind in (0, 3, 4):
                    require(sent >= previous_event_time, "event clock")
                    previous_event_time = sent
                    require(sequence == next_sequence, "arrival sequence")
                    require(sent >= planned and sent >= previous_sent and sent < stage_limit_ns, "arrival time")
                    require(observed == 0 and value == 0, "arrival payload")
                    if kind == 3:
                        attribute_mailbox_full(attribution, sequence, actor + 1,
                                               "sample" if planned >= warmup_ns else "warmup",
                                               planned, sent, previous_sent)
                    previous_sent = sent
                    next_sequence += 1
                    if planned >= warmup_ns:
                        observe(lateness, sent - planned)
                    if kind == 0:
                        require(len(pending) < 256, "pending overflow")
                        expected[actor] += 1
                        pending[sequence] = (sent, expected[actor])
                    else:
                        if kind == 4:
                            require(len(pending) == 256, "false pending-full")
                        group["mailbox_full" if kind == 3 else "pending_full"] += 1
                elif kind in (1, 2):
                    require(observed >= previous_event_time, "event clock")
                    previous_event_time = observed
                    require(sequence in pending, "unmatched/duplicate reply")
                    original_sent, original_value = pending.pop(sequence)
                    require(sent == original_sent and value == original_value, "reply correlation/value")
                    require(observed >= sent and observed >= previous_observed and observed < stage_limit_ns, "reply clock")
                    previous_observed = observed
                    require(kind == (1 if observed < planned + DEADLINE_NS else 2), "deadline")
                    if runtime == "beam":
                        require(value == observed_values[actor] + 1, "actual FIFO")
                        observed_values[actor] = value
                    group["reply" if kind == 1 else "timed_out"] += 1
                    if planned >= warmup_ns:
                        observe(latency, observed - planned)
                        observe(service, observed - original_sent)
                else:
                    raise ValueError("unknown event")
    require(next_sequence == rate * total_seconds + 1 and not pending, "missing arrival/result")
    require(result["committed"] == expected and all(type(value) is int for value in result["committed"]), "committed")
    require(sum(warmup.values()) == rate * warmup_seconds and sum(counts.values()) == rate * sample_seconds, "phase accounting")
    percentiles(path, rate, latency, lateness, service, warmup_ns=warmup_ns)
    summary = {"arrivals": rate * sample_seconds, **counts, **latency,
               "timeout_ratio": counts["timed_out"] / (rate * sample_seconds),
               "refusal_ratio": (counts["mailbox_full"] + counts["pending_full"]) / (rate * sample_seconds),
               "on_time_replies_per_second": counts["reply"] / sample_seconds,
               "generator_lateness": lateness, "service_latency": service,
               "mailbox_full_attribution": attribution}
    summary.update({"warmup": warmup, "events": events, "trace_bytes": path.stat().st_size,
                    "sha256": digest.hexdigest(), "drained": True})
    summary["tail_candidate_pass"] = (
        counts["mailbox_full"] == counts["pending_full"] == counts["timed_out"] == 0
        and summary["p99_ns"] is not None and summary["p99_ns"] <= 20_000_000
        and (summary["samples"] < 10000 or (summary["p999_ns"] is not None and summary["p999_ns"] <= 50_000_000))
    )
    return summary


def limit_child():
    cpu = min([240] + [value for value in resource.getrlimit(resource.RLIMIT_CPU) if value != resource.RLIM_INFINITY])
    file_limit = min([MAX_TRACE] + [value for value in resource.getrlimit(resource.RLIMIT_FSIZE) if value != resource.RLIM_INFINITY])
    resource.setrlimit(resource.RLIMIT_CPU, (cpu, cpu))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_FSIZE, (file_limit, file_limit))


def stop(child, failed=False):
    if failed or child.poll() is None:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    child.wait()


def memory_snapshot(root, probe=None):
    if probe is not None:
        return measure_memory.snapshot(root, probe)
    census = measure_memory.census(root)
    values = {pid: row["rss_bytes"] for pid, row in census["processes"].items()}
    return {"rss_bytes": sum(values.values()) + census["observer"]["rss_bytes"], "processes": values,
            "platform_memory": {"complete": False, "status": "not_configured"}}


def footprint_metric_bytes(run):
    peaks = run.get("platform_memory", {}).get("sampled_metric_peaks", {})
    for key in ("physical_footprint_bytes", "pss_bytes", "rss_bytes"):
        if type(peaks.get(key)) is int and peaks[key] > 0:
            return peaks[key]
    return run.get("rss_tree_plus_driver_peak_bytes", 0)


def measurement(command, trace, destination, name, rate, memory_probe=None, *, runtime, validator=validate_trace, cpu_control=None, warmup_seconds=30, sample_seconds=60):
    require(runtime in ("wings", "beam"), "expected measurement runtime")
    environment = os.environ.copy()
    for variable in ("ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "ERL_LIBS"):
        environment.pop(variable, None)
    environment["ERL_CRASH_DUMP"] = os.devnull
    environment["ERL_CRASH_DUMP_SECONDS"] = "0"
    if (warmup_seconds, sample_seconds) != (30, 60):
        environment["WINGS_BENCH_WINDOW"] = f"{warmup_seconds},{sample_seconds}"
    else:
        environment.pop("WINGS_BENCH_WINDOW", None)
    started = time.monotonic()
    raw = destination.with_suffix(".stdout.jsonl")
    errors = destination.with_suffix(".stderr.txt")
    memory = destination.with_suffix(".memory.jsonl")
    manifest = {"command": [str(value) for value in command], "trace": str(trace), "runtime": runtime, "status": "running"}
    destination.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
    try:
        if cpu_control is not None:
            initial_cpu = cpu_control.snapshot()
            require(initial_cpu["processes"] == [os.getpid()], "CPU quota job contains a prior live child")
            manifest["cpu_quota"] = {"initial": initial_cpu, "samples": 0, "status": "observing"}
            destination.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
        with errors.open("xb") as stderr:
            child = subprocess.Popen([str(value) for value in command], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=stderr,
                                     start_new_session=True, preexec_fn=limit_child, env=environment, cwd=aot.ROOT)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        manifest.update({"status": "invalid_experiment", "error": str(error)})
        destination.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ)
    os.set_blocking(child.stdout.fileno(), False)
    buffer = b""
    events = []
    peak = 0
    memory_summary = {"samples": 0, "complete_samples": 0, "host_error_samples": 0,
                      "identity_changes": 0, "sampled_metric_peaks": {},
                      "scope": "driver plus captured descendants; helper costs recorded separately"}
    memory_identities = {}
    next_snapshot = started
    wall_limit = warmup_seconds + sample_seconds + 40
    warmup_ns = warmup_seconds * 1_000_000_000
    try:
        with raw.open("xb") as stdout, memory.open("x") as snapshots:
            while selector.get_map():
                now = time.monotonic()
                require(now - started < wall_limit, "measurement wall deadline")
                if now >= next_snapshot:
                    sample = memory_snapshot(os.getpid(), memory_probe)
                    if cpu_control is not None:
                        sample["cpu_quota"] = cpu_control.snapshot()
                        manifest["cpu_quota"]["samples"] += 1
                    sample["elapsed_seconds"] = now - started
                    sample["phase"] = "initializing" if not events else "sampling" if len(events) >= 2 else "warmup"
                    peak = max(peak, sample["rss_bytes"])
                    details = sample["platform_memory"]
                    memory_summary["identity_changes"] += measure_memory.track_identities(details, memory_identities)
                    memory_summary["samples"] += 1
                    if details["complete"]:
                        memory_summary["complete_samples"] += 1
                        for field, value in details["totals"].items():
                            memory_summary["sampled_metric_peaks"][field] = max(
                                memory_summary["sampled_metric_peaks"].get(field, 0), value)
                    memory_summary["host_error_samples"] += details.get("host", {}).get("status") == "error"
                    snapshots.write(json.dumps(sample) + "\n")
                    snapshots.flush()
                    require(sample["rss_bytes"] <= MAX_RSS, "observed RSS safety stop, not OS quota")
                    next_snapshot = now + 1
                for key, _ in selector.select(timeout=0.05):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        break
                    stdout.write(chunk)
                    buffer += chunk
                    require(len(buffer) <= 1 << 20, "control output limit")
                    while b"\n" in buffer:
                        line, buffer = buffer.split(b"\n", 1)
                        event = json.loads(line)
                        require(len(events) < 3, "extra control event")
                        events.append(event)
                require(errors.stat().st_size <= 65536, "stderr limit")
            child.wait(timeout=max(0.001, wall_limit - (time.monotonic() - started)))
        require(child.returncode == 0 and errors.stat().st_size == 0, f"child exit {child.returncode}")
        require(not buffer and len(events) == 3, "incomplete control output")
        require(events[0] == {"event": "start", "warmup_seconds": warmup_seconds, "sample_seconds": sample_seconds}, "start control")
        require(events[1].get("event") == "sampling" and integer(events[1]["elapsed_ns"]) >= warmup_ns, "sampling control")
        require(events[-1].get("environment", {}).get("runtime") == runtime, "measurement runtime mismatch")
        if runtime == "wings":
            receipts.validate(events[-1])
        summary = validator(trace, name, rate, events[-1])
        if cpu_control is not None:
            manifest["cpu_quota"].update({"status": "observations_complete", "final": cpu_control.snapshot(),
                                         "performance_gate_proven": bool(summary.get("tail_candidate_pass"))})
        manifest.update({"status": "ok", "summary": summary, "result": events[-1],
                         "rss_tree_plus_driver_peak_bytes": peak, "wall_seconds": time.monotonic() - started})
    except BaseException as error:
        if cpu_control is not None:
            manifest["cpu_quota"].update({"status": "invalid_experiment", "error": str(error)})
        manifest.update({"status": "invalid_experiment", "error": str(error), "wall_seconds": time.monotonic() - started})
        raise
    finally:
        stop(child, manifest["status"] != "ok")
        selector.close()
        child.stdout.close()
        memory_summary["all_samples_complete"] = (memory_summary["samples"] > 0
                                                    and memory_summary["complete_samples"] == memory_summary["samples"])
        manifest["platform_memory"] = memory_summary
        destination.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description="Paired open-loop Wings vs BEAM experiment")
    parser.add_argument("--case", choices=CASES, required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scan", action="store_true")
    parser.add_argument("--warmup-seconds", type=int, default=30)
    parser.add_argument("--sample-seconds", type=int, default=60)
    parser.add_argument("--repetitions", type=int, default=5)
    measure_cpu.add_arguments(parser)
    arguments = parser.parse_args()
    warmup_s = arguments.warmup_seconds
    sample_s = arguments.sample_seconds
    reps = arguments.repetitions
    require(1 <= warmup_s <= 60 and 1 <= sample_s <= 120 and 1 <= reps <= 10, "invalid window/repetitions")
    cpu_control = measure_cpu.from_arguments(arguments)
    require(platform.system() in ("Darwin", "Linux"), "supported measurement OS")
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    require(platform.machine() in ("arm64", "aarch64", "x86_64"), "measurement architecture")
    escript = shutil.which("escript")
    require(escript is not None, "OTP escript required; no model fallback")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    artifact = arguments.artifact.resolve()
    executable = aot.TARGET / "release/examples/beam_open_loop"
    require(executable.is_file() and artifact.is_dir(), "build release runner and artifact first")
    frozen = output / "frozen"
    frozen.mkdir()
    shutil.copy2(aot.ROOT / "scripts/measure_memory.c", frozen / "measure_memory.c")
    memory_probe = frozen / "measure_memory"
    memory_setup = {"status": "running"}
    try:
        memory_build = measure_memory.build(frozen / "measure_memory.c", memory_probe)
        # Setup-only: the freshly built probe's first execution can exceed the
        # 2-second in-measurement snapshot deadline; preflight uses the same
        # 30-second tool budget as the probe build. Measured samples keep 2s.
        memory_preflight = measure_memory.snapshot(os.getpid(), memory_probe, timeout=30)
        memory_setup.update({"status": "ok", "command": memory_build})
    except BaseException as error:
        memory_setup.update({"status": "invalid_experiment", "error": str(error)})
        raise
    finally:
        (output / "memory-setup.json").write_text(json.dumps(memory_setup, indent=2) + "\n")
    (output / "memory-preflight.json").write_text(json.dumps(memory_preflight, indent=2) + "\n")
    shutil.copy2(executable, frozen / "beam_open_loop")
    for filename in ("beam_open_loop.escript", "beam_bounded_mailbox.erl"):
        shutil.copy2(aot.ROOT / "scripts" / filename, frozen / filename)
    require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) < 16 << 20, "artifact bytes")
    shutil.copytree(artifact, frozen / "artifact")
    tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT).decode().split("\0")
    tracked += ["scripts/measure_open_loop.py", "scripts/verify_measurement.py", "scripts/measure_memory.py",
                "scripts/measure_memory.c", "scripts/verify_memory.py", "scripts/verify_resource_receipt.py"]
    source_directory = frozen / "source"
    for name in sorted(set(tracked)):
        if name and not name.startswith("docs/proposals/") and (aot.ROOT / name).is_file():
            destination = source_directory / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(aot.ROOT / name, destination)
    executable = frozen / "beam_open_loop"
    artifact = frozen / "artifact"
    environment = {
        "version": 6, "case": arguments.case, "platform": platform.platform(), "machine": platform.machine(),
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True).strip(),
        "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "runner_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "inputs": {str(path.relative_to(frozen)): hashlib.sha256(path.read_bytes()).hexdigest()
                   for path in sorted(frozen.rglob("*")) if path.is_file()},
        "host_isolation": "unverified; exploratory integration measurement, not acceptance",
        "cpu": (subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
                if platform.system() == "Darwin" else Path("/proc/cpuinfo").read_text()),
        "page_bytes": os.sysconf("SC_PAGE_SIZE"),
        "unrecorded_environment": ["thermal state", "power mode", "QoS", "core assignment"],
        "warmup_seconds": warmup_s, "sample_seconds": sample_s, "repetitions": reps,
        "limits": {"cpu_seconds": 240, "wall_seconds": warmup_s + sample_s + 40, "stage_seconds": warmup_s + sample_s + 30,
                   "file_bytes": MAX_TRACE, "observed_rss_stop_bytes": MAX_RSS},
        "memory_probe_build": memory_build,
        "cpu_quota_preflight": None if cpu_control is None else cpu_control.initial,
        "memory_scope": "driver plus captured descendants; census/probe helpers excluded from tree totals; host compressor/swap not attributed to VM; non-atomic samples",
    }
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    points = []
    base = CASES[arguments.case][3]
    total_s = warmup_s + sample_s
    for power in range(8 if arguments.scan else 1):
        rate = base << power
        require(shutil.disk_usage(output).free > rate * total_s * EVENT.size * 2 * (reps * 2) + (256 << 20), "insufficient disk for raw traces")
        results = []
        for repetition in range(reps):
            order = ("wings", "beam") if repetition % 2 == 0 else ("beam", "wings")
            for runtime in order:
                name = f"{rate}-{repetition + 1}-{runtime}"
                trace = output / f"{name}.events.bin"
                command = ([executable, "--trusted", artifact, "--measure"] if runtime == "wings"
                           else [escript, frozen / "beam_open_loop.escript", "--measure"])
                result = measurement(
                    [*command, arguments.case, rate, trace],
                    trace,
                    output / name,
                    arguments.case,
                    rate,
                    memory_probe,
                    runtime=runtime,
                    cpu_control=cpu_control,
                    warmup_seconds=warmup_s,
                    sample_seconds=sample_s,
                    validator=lambda p, n, r, res: validate_trace(p, n, r, res, warmup_seconds=warmup_s, sample_seconds=sample_s),
                )
                results.append({"runtime": runtime, "repetition": repetition + 1, **result["summary"],
                                "rss_tree_plus_driver_peak_bytes": result["rss_tree_plus_driver_peak_bytes"],
                                "platform_memory": result["platform_memory"],
                                "native_ledger": result["result"].get("native_ledger")})
                print(f"{name}: tail_pass={result['summary']['tail_candidate_pass']} svc_p99={result['summary']['service_latency']['p99_ns']}ns", flush=True)
        saturated = any(not result["tail_candidate_pass"] for result in results)
        require(all(hashlib.sha256((frozen / filename).read_bytes()).hexdigest() == digest
                    for filename, digest in environment["inputs"].items()), "frozen input changed")
        wings_runs = [r for r in results if r["runtime"] == "wings"]
        beam_runs = [r for r in results if r["runtime"] == "beam"]
        wings_tail_ok = all(r["tail_candidate_pass"] for r in wings_runs)
        w_rps = statistics.median(r["on_time_replies_per_second"] for r in wings_runs)
        b_rps = statistics.median(r["on_time_replies_per_second"] for r in beam_runs)
        w_svc_p99 = statistics.median(r["service_latency"]["p99_ns"] for r in wings_runs)
        b_svc_p99 = statistics.median(r["service_latency"]["p99_ns"] for r in beam_runs)
        w_mem = statistics.median(footprint_metric_bytes(r) for r in wings_runs)
        b_mem = statistics.median(footprint_metric_bytes(r) for r in beam_runs)
        throughput_ok = wings_tail_ok and w_rps >= b_rps
        service_p99_ok = wings_tail_ok and (
            w_svc_p99 <= max(50_000, int(b_svc_p99 * 2))
            if arguments.case == "hot-queue"
            else w_svc_p99 <= 20_000_000
        )
        memory_ok = w_mem < b_mem
        comparison = {
            "wings_median_on_time_rps": w_rps,
            "beam_median_on_time_rps": b_rps,
            "wings_median_service_p99_ns": w_svc_p99,
            "beam_median_service_p99_ns": b_svc_p99,
            "wings_median_footprint_bytes": w_mem,
            "beam_median_footprint_bytes": b_mem,
            "throughput_gate_passed": throughput_ok,
            "service_latency_gate_passed": service_p99_ok,
            "memory_gate_passed": memory_ok,
        }
        points.append({"rate": rate, "saturated_or_tail_failed": saturated, "comparison": comparison, "runs": results})
        dual_gate_ok = throughput_ok and service_p99_ok and memory_ok
        report = {
            "case": arguments.case,
            "warmup_seconds": warmup_s,
            "sample_seconds": sample_s,
            "repetitions": reps,
            "cpu_quota_enforced": cpu_control is not None,
            "points": points,
            "scan_requested": arguments.scan,
            "saturation_found": saturated,
            "throughput_gate_proven": throughput_ok,
            "service_latency_gate_proven": service_p99_ok,
            "memory_gate_proven": memory_ok,
            "mvp_dual_gate_passed": dual_gate_ok,
            "scan_sufficient": arguments.scan and saturated,
        }
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        if saturated:
            break


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Open-loop measurement failed: {error}", file=sys.stderr)
        sys.exit(3)
