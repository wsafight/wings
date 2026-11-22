import argparse
from array import array
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys

import measure_memory
import measure_cpu
import measure_open_loop as measure
import verify_scalar_aot as aot
import verify_resource_receipt as receipts
from verify_fairness import quantiles
from verify_open_loop import integer, require


COUNT = 900000
WARMUP_NS = 30_000_000_000
STAGE_NS = 120_000_000_000
MAX_HOT = 1000000
MAX_YIELDS = 1000000
MAX_TRACE = (COUNT * 2 + MAX_HOT * 2 + MAX_YIELDS) * measure.EVENT.size


def planned(sequence):
    return (sequence - 1) // 100 * 10_000_000


def phase():
    return {"reply": 0, "timed_out": 0, "mailbox_full": 0, "pending_full": 0}


class Replay:
    def __init__(self, observation_offset=0, *, warmup_seconds=30, sample_seconds=60):
        self.offset = observation_offset
        self.warmup_seconds = warmup_seconds
        self.sample_seconds = sample_seconds
        self.total_seconds = warmup_seconds + sample_seconds
        self.count_limit = 10_000 * self.total_seconds
        self.warmup_ns = warmup_seconds * 1_000_000_000
        self.total_ns = self.total_seconds * 1_000_000_000
        self.stage_ns = (self.total_seconds + 30) * 1_000_000_000
        self.next = 1
        self.pending = {}
        self.values = [0] * 101
        self.received = [0] * 101
        self.hot = None
        self.previous_time = 0
        self.last_sent = 0
        self.last_hot_end = 0
        self.counts = phase()
        self.warmup = phase()
        self.by_actor = [{"actor": actor, **phase(), "latencies": array("Q")} for actor in range(2, 102)]
        self.latencies = array("Q")
        self.lateness = array("Q")
        self.mailbox_full_attribution = measure.new_mailbox_full_attribution()
        self.hot_durations = array("Q")
        self.warmup_hot = 0
        self.sample_hot_completions = 0
        self.yield_count = 0
        self.yield_sequence = 0
        self.yield_requested = 0
        self.yield_end = 0
        self.yield_gaps = 0
        self.yields = array("Q")
        self.yields_by_actor = [array("Q") for _ in range(101)]

    def consume(self, record):
        sequence, sent, observed, value, kind = record
        require(all(type(field) is int for field in record), "event integers")
        require(1 <= sequence and 0 <= sent and 0 <= observed, "event range")
        if kind == 7:
            require(self.yield_sequence < sequence and 1 <= value <= 101, "yield sequence/actor")
            require(self.yield_requested <= sent <= observed < self.stage_ns and self.yield_end <= observed, "yield clock")
            require(self.yield_count < MAX_YIELDS, "yield history bound")
            require(self.yield_count != 0 or sequence == 1, "first yield sample overwritten")
            self.yield_gaps += sequence - self.yield_sequence - 1
            self.yield_sequence = sequence
            self.yield_requested = sent
            self.yield_end = observed
            self.yield_count += 1
            if sent >= self.offset + self.warmup_ns and sent < self.offset + self.total_ns:
                self.yields.append(observed - sent)
                self.yields_by_actor[value - 1].append(observed - sent)
            return
        require(kind in (0, 1, 2, 3, 4, 5, 6), "event kind")
        at = observed if kind in (1, 2, 6) else sent
        require(self.previous_time <= at < self.stage_ns, "event observation order")
        self.previous_time = at
        if kind == 5:
            require(self.hot is None and sequence == self.values[0] + 1 <= MAX_HOT, "one CPU request")
            require((self.next <= self.count_limit or sent < self.total_ns) and observed == value == 0, "CPU admission")
            self.hot = (sequence, sent)
            return
        if kind == 6:
            require(self.hot == (sequence, sent) and value == 1000000 and sent <= observed, "complete CPU result")
            self.hot = None
            self.values[0] += 1
            self.last_hot_end = observed
            self.sample_hot_completions += self.warmup_ns <= observed < self.total_ns
            if sent >= self.warmup_ns:
                self.hot_durations.append(observed - sent)
            else:
                self.warmup_hot += 1
            return
        actor = 1 + (sequence - 1) % 100
        expected_at = planned(sequence)
        counts = self.counts if expected_at >= self.warmup_ns else self.warmup
        group = self.by_actor[actor - 1] if expected_at >= self.warmup_ns else None
        if kind in (0, 3, 4):
            require(sequence == self.next <= self.count_limit, "heartbeat sequence")
            require(self.hot is not None, "CPU load absent during heartbeat arrival")
            require(sent >= expected_at and observed == value == 0, "heartbeat admission clock/value")
            self.next += 1
            if kind == 3:
                measure.attribute_mailbox_full(self.mailbox_full_attribution, sequence, actor + 1,
                                               "sample" if group is not None else "warmup",
                                               expected_at, sent, self.last_sent)
            self.last_sent = sent
            if group is not None:
                self.lateness.append(sent - expected_at)
            if kind == 0:
                require(len(self.pending) < 255, "heartbeat pending overflow")
                self.values[actor] += 1
                self.pending[sequence] = (sent, actor, self.values[actor])
            else:
                require(kind != 4 or len(self.pending) == 255, "false heartbeat pending-full")
                field = "mailbox_full" if kind == 3 else "pending_full"
                counts[field] += 1
                if group is not None:
                    group[field] += 1
            return
        require(sequence in self.pending, "unknown or repeated heartbeat completion")
        original, actor, expected = self.pending.pop(sequence)
        require(sent == original and sent <= observed and value == expected, "heartbeat result")
        self.received[actor] += 1
        require(value == self.received[actor], "per-target FIFO")
        require(kind == (1 if observed < expected_at + 100_000_000 else 2), "heartbeat deadline")
        field = "reply" if kind == 1 else "timed_out"
        counts[field] += 1
        if group is not None:
            group[field] += 1
            delay = observed - expected_at
            group["latencies"].append(delay)
            self.latencies.append(delay)

    def finish(self, result):
        expected_sample = 10_000 * self.sample_seconds
        expected_warmup = 10_000 * self.warmup_seconds
        expected_per_actor = 100 * self.sample_seconds
        require(self.next == self.count_limit + 1 and not self.pending and self.hot is None, "missing heartbeat/CPU result")
        require(self.values[0] > 0 and self.last_hot_end >= self.last_sent, "CPU load ended early")
        require(max(self.total_ns, self.previous_time) <= integer(result["measurement_elapsed_ns"]) < self.stage_ns,
                "full sample window and drain clock")
        require(type(result["committed"]) is list and all(type(value) is int for value in result["committed"])
                and result["committed"] == self.values, "committed state")
        require(sum(self.counts.values()) == expected_sample and sum(self.warmup.values()) == expected_warmup, "phase accounting")
        observation = result.get("yield_observation")
        yield_summary = None
        if observation is None:
            require(self.yield_count == 0, "unexpected yield events")
        else:
            require(observation["capacity"] == 64 and type(observation["capacity"]) is int
                    and observation["pending"] is False and observation["overflowed"] is False
                    and observation["samples"] == [], "yield control")
            counters = {key: integer(observation[key]) for key in
                        ("requests", "coalesced", "consumed", "cleared", "confirmed", "retained", "taken", "dropped", "published")}
            require(counters["requests"] == counters["coalesced"] + counters["consumed"] + counters["cleared"], "yield requests")
            require(counters["consumed"] == counters["confirmed"] == counters["published"], "yield confirmations")
            require(counters["retained"] == 0 and counters["taken"] == self.yield_count
                    and counters["confirmed"] == self.yield_count + counters["dropped"], "yield queue")
            require(counters["confirmed"] == 0 or self.yield_count > 0, "first accepted yield missing")
            require(self.yield_sequence <= counters["confirmed"]
                    and self.yield_gaps + counters["confirmed"] - self.yield_sequence == counters["dropped"], "yield missing records")
            yield_summary = {**counters, **quantiles(self.yields), "complete": counters["dropped"] == 0,
                             "has_data": bool(self.yields), "per_actor": [
                                 {"actor": index + 1, **quantiles(delays)} for index, delays in enumerate(self.yields_by_actor)]}
        groups = []
        for group in self.by_actor:
            delays = group.pop("latencies")
            require(sum(group[field] for field in phase()) == expected_per_actor, "actor arrivals")
            groups.append({**group, "arrivals": expected_per_actor, **quantiles(delays)})
        latency = quantiles(self.latencies)
        tail_candidate = (self.counts["mailbox_full"] == self.counts["pending_full"] == self.counts["timed_out"] == 0
                          and latency["p99_ns"] is not None and latency["p99_ns"] <= 20_000_000
                          and (len(self.latencies) < 10000 or (latency["p999_ns"] is not None and latency["p999_ns"] <= 50_000_000)))
        return {"arrivals": expected_sample, **self.counts, **latency, "per_actor": groups,
                "timeout_ratio": self.counts["timed_out"] / expected_sample,
                "refusal_ratio": (self.counts["mailbox_full"] + self.counts["pending_full"]) / expected_sample,
                "on_time_replies_per_second": self.counts["reply"] / self.sample_seconds,
                "generator_lateness": quantiles(self.lateness), "warmup": self.warmup,
                "mailbox_full_attribution": self.mailbox_full_attribution,
                "cpu": {"warmup_started_completions": self.warmup_hot, "sample_started_completions": len(self.hot_durations),
                        "completed_in_sample": self.sample_hot_completions,
                        "completions_per_second": self.sample_hot_completions / self.sample_seconds, **quantiles(self.hot_durations)},
                "yield_observation": yield_summary, "tail_candidate_pass": tail_candidate,
                "fairness_gate_proven": tail_candidate, "drained": True}


def validate_trace(path, result, *, version=8, replay_factory=Replay, max_trace=MAX_TRACE, warmup_seconds=30, sample_seconds=60):
    stage_limit_ns = (warmup_seconds + sample_seconds + 30) * 1_000_000_000
    for field, expected in {"version": version, "status": "ok", "mode": "measure", "actors": 101,
                            "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": 255,
                            "deadline_ns": 100_000_000, "message_bytes": 32, "samples": [], "hot": [],
                            "states_verified": True}.items():
        require(type(result.get(field)) is type(expected) and result[field] == expected, field)
    environment = result["environment"]
    require(environment["runtime"] in ("wings", "beam"), "runtime")
    if environment["runtime"] == "wings":
        receipts.validate(result, historical=True)
        require(result.get("resources_released") is True, "Wings cleanup")
        for key, value in (("workers", 1), ("slice_steps", 4096), ("quantum_ns", 1_000_000)):
            require(type(environment[key]) is int and environment[key] == value, key)
        offset = integer(result["observation_offset_ns"])
        require(offset < stage_limit_ns, "observation origin offset")
    else:
        require(environment["otp"] in ("27", "28", "29") and type(environment["schedulers"]) is int
                and environment["schedulers"] == 1 and result.get("yield_observation") is None, "BEAM environment")
        offset = 0
    require(path.stat().st_size <= max_trace and path.stat().st_size % measure.EVENT.size == 0, "trace bytes")
    replay = (replay_factory(offset, warmup_seconds=warmup_seconds, sample_seconds=sample_seconds)
              if replay_factory is Replay else replay_factory(offset))
    digest = hashlib.sha256()
    events = 0
    with path.open("rb") as source:
        while block := source.read(measure.EVENT.size * 16384):
            digest.update(block)
            for record in measure.EVENT.iter_unpack(block):
                replay.consume(record)
                events += 1
    return {**replay.finish(result), "events": events, "trace_bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def main():
    parser = argparse.ArgumentParser(description="Continuous million-add CPU and exact-burst heartbeats paired Wings vs BEAM")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--observe", action="store_true", help="enable Wings yield diagnostics; reports instrumentation mode")
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
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    escript = shutil.which("escript")
    require(escript is not None, "OTP escript required; no model fallback")
    executable = aot.TARGET / "release/examples/beam_fairness"
    artifact = arguments.artifact.resolve()
    require(executable.is_file() and artifact.is_dir(), "build release runner and artifact first")
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "frozen"
    frozen.mkdir()
    for name in ("beam_fairness.escript", "beam_bounded_mailbox.erl", "measure_memory.c"):
        shutil.copy2(aot.ROOT / "scripts" / name, frozen / name)
    shutil.copy2(executable, frozen / "beam_fairness")
    require(sum(path.stat().st_size for path in artifact.rglob("*") if path.is_file()) < 16 << 20, "artifact bytes")
    shutil.copytree(artifact, frozen / "artifact")
    tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=aot.ROOT).decode().split("\0")
    for name in sorted(set(tracked)):
        if name and not name.startswith("docs/proposals/") and (aot.ROOT / name).is_file():
            destination = frozen / "source" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(aot.ROOT / name, destination)
    probe = frozen / "measure_memory"
    memory_setup = {"status": "running"}
    try:
        memory_build = measure_memory.build(frozen / "measure_memory.c", probe)
        # Setup-only preflight, same 30-second tool budget as the probe build;
        # in-measurement snapshots keep the original 2-second deadline.
        memory_preflight = measure_memory.snapshot(os.getpid(), probe, timeout=30)
        memory_setup.update({"status": "ok", "command": memory_build})
    except BaseException as error:
        memory_setup.update({"status": "invalid_experiment", "error": str(error)})
        raise
    finally:
        (output / "memory-setup.json").write_text(json.dumps(memory_setup, indent=2) + "\n")
    (output / "memory-preflight.json").write_text(json.dumps(memory_preflight, indent=2) + "\n")
    environment = {"version": 8, "platform": platform.platform(), "machine": platform.machine(),
                   "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                   "cpu": (subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()
                           if platform.system() == "Darwin" else Path("/proc/cpuinfo").read_text()),
                   "page_bytes": os.sysconf("SC_PAGE_SIZE"),
                   "unrecorded_environment": ["thermal state", "power mode", "QoS", "core assignment"],
                   "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=aot.ROOT, text=True).strip(),
                   "dirty": subprocess.check_output(["git", "status", "--porcelain"], cwd=aot.ROOT, text=True),
                   "yield_observation_enabled": arguments.observe, "warmup_seconds": warmup_s, "sample_seconds": sample_s,
                   "repetitions": reps, "memory_probe_build": memory_build,
                   "cpu_quota_preflight": None if cpu_control is None else cpu_control.initial,
                   "inputs": {str(path.relative_to(frozen)): hashlib.sha256(path.read_bytes()).hexdigest()
                              for path in sorted(frozen.rglob("*")) if path.is_file()},
                   "limits": {"cpu_seconds": 240, "wall_seconds": warmup_s + sample_s + 40, "stage_seconds": warmup_s + sample_s + 30,
                              "file_bytes": measure.MAX_TRACE, "observed_rss_stop_bytes": measure.MAX_RSS},
                   "protocol_max_trace_bytes": MAX_TRACE}
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    scaled_trace_max = ((10_000 * (warmup_s + sample_s)) * 2 + 200_000) * measure.EVENT.size
    require(shutil.disk_usage(output).free > scaled_trace_max * (reps * 2) + (256 << 20), "insufficient disk for raw traces")
    results = []
    for repetition in range(reps):
        for runtime in (("wings", "beam") if repetition % 2 == 0 else ("beam", "wings")):
            name = f"{repetition + 1}-{runtime}"
            trace = output / f"{name}.events.bin"
            command = ([frozen / "beam_fairness", "--trusted", frozen / "artifact", "--measure", trace]
                       if runtime == "wings" else [escript, frozen / "beam_fairness.escript", "--measure", trace])
            if runtime == "wings" and arguments.observe:
                command.append("--observe")
            result = measure.measurement(
                command,
                trace,
                output / name,
                "fairness",
                10000,
                probe,
                runtime=runtime,
                cpu_control=cpu_control,
                warmup_seconds=warmup_s,
                sample_seconds=sample_s,
                validator=lambda path, _name, _rate, res: validate_trace(
                    path, res, warmup_seconds=warmup_s, sample_seconds=sample_s
                ),
            )
            results.append({"runtime": runtime, "repetition": repetition + 1, **result["summary"],
                            "platform_memory": result["platform_memory"],
                            "rss_tree_plus_driver_peak_bytes": result["rss_tree_plus_driver_peak_bytes"],
                            "native_ledger": result["result"].get("native_ledger")})
            require(all(hashlib.sha256((frozen / filename).read_bytes()).hexdigest() == digest
                        for filename, digest in environment["inputs"].items()), "frozen input changed")
            print(f"{name}: tail_pass={result['summary']['tail_candidate_pass']} cpu_rps={result['summary']['cpu']['completions_per_second']:.2f} hb_p99={result['summary']['p99_ns']}ns", flush=True)
    wings_runs = [r for r in results if r["runtime"] == "wings"]
    beam_runs = [r for r in results if r["runtime"] == "beam"]
    wings_tail_ok = all(r["tail_candidate_pass"] for r in wings_runs)
    w_cpu = statistics.median(r["cpu"]["completions_per_second"] for r in wings_runs)
    b_cpu = statistics.median(r["cpu"]["completions_per_second"] for r in beam_runs)
    w_hb_p99 = statistics.median(r["p99_ns"] for r in wings_runs)
    b_hb_p99 = statistics.median(r["p99_ns"] for r in beam_runs)
    w_mem = statistics.median(measure.footprint_metric_bytes(r) for r in wings_runs)
    b_mem = statistics.median(measure.footprint_metric_bytes(r) for r in beam_runs)
    fairness_ok = wings_tail_ok and w_cpu >= b_cpu
    memory_ok = w_mem < b_mem
    comparison = {
        "wings_median_cpu_completions_per_sec": w_cpu,
        "beam_median_cpu_completions_per_sec": b_cpu,
        "wings_median_heartbeat_p99_ns": w_hb_p99,
        "beam_median_heartbeat_p99_ns": b_hb_p99,
        "wings_median_footprint_bytes": w_mem,
        "beam_median_footprint_bytes": b_mem,
        "fairness_throughput_gate_passed": fairness_ok,
        "memory_gate_passed": memory_ok,
    }
    report = {
        "warmup_seconds": warmup_s,
        "sample_seconds": sample_s,
        "repetitions": reps,
        "cpu_quota_enforced": cpu_control is not None,
        "comparison": comparison,
        "runs": results,
        "fairness_gate_proven": fairness_ok,
        "memory_gate_proven": memory_ok,
        "mvp_dual_gate_passed": fairness_ok and memory_ok,
    }
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Fairness measurement failed: {error}", file=sys.stderr)
        sys.exit(3)
