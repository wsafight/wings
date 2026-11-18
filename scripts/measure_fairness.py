import argparse
from array import array
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
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
    def __init__(self, observation_offset=0):
        self.offset = observation_offset
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
            require(self.yield_requested <= sent <= observed < STAGE_NS and self.yield_end <= observed, "yield clock")
            require(self.yield_count < MAX_YIELDS, "yield history bound")
            require(self.yield_count != 0 or sequence == 1, "first yield sample overwritten")
            self.yield_gaps += sequence - self.yield_sequence - 1
            self.yield_sequence = sequence
            self.yield_requested = sent
            self.yield_end = observed
            self.yield_count += 1
            if sent >= self.offset + WARMUP_NS and sent < self.offset + 90_000_000_000:
                self.yields.append(observed - sent)
                self.yields_by_actor[value - 1].append(observed - sent)
            return
        require(kind in (0, 1, 2, 3, 4, 5, 6), "event kind")
        at = observed if kind in (1, 2, 6) else sent
        require(self.previous_time <= at < STAGE_NS, "event observation order")
        self.previous_time = at
        if kind == 5:
            require(self.hot is None and sequence == self.values[0] + 1 <= MAX_HOT, "one CPU request")
            require((self.next <= COUNT or sent < 90_000_000_000) and observed == value == 0, "CPU admission")
            self.hot = (sequence, sent)
            return
        if kind == 6:
            require(self.hot == (sequence, sent) and value == 1000000 and sent <= observed, "complete CPU result")
            self.hot = None
            self.values[0] += 1
            self.last_hot_end = observed
            self.sample_hot_completions += WARMUP_NS <= observed < 90_000_000_000
            if sent >= WARMUP_NS:
                self.hot_durations.append(observed - sent)
            else:
                self.warmup_hot += 1
            return
        actor = 1 + (sequence - 1) % 100
        expected_at = planned(sequence)
        counts = self.counts if expected_at >= WARMUP_NS else self.warmup
        group = self.by_actor[actor - 1] if expected_at >= WARMUP_NS else None
        if kind in (0, 3, 4):
            require(sequence == self.next <= COUNT, "heartbeat sequence")
            require(self.hot is not None, "CPU load absent during heartbeat arrival")
            require(sent >= expected_at and observed == value == 0, "heartbeat admission clock/value")
            self.next += 1
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
        require(self.next == COUNT + 1 and not self.pending and self.hot is None, "missing heartbeat/CPU result")
        require(self.values[0] > 0 and self.last_hot_end >= self.last_sent, "CPU load ended early")
        require(max(90_000_000_000, self.previous_time) <= integer(result["measurement_elapsed_ns"]) < STAGE_NS,
                "full sample window and drain clock")
        require(type(result["committed"]) is list and all(type(value) is int for value in result["committed"])
                and result["committed"] == self.values, "committed state")
        require(sum(self.counts.values()) == 600000 and sum(self.warmup.values()) == 300000, "phase accounting")
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
            require(sum(group[field] for field in phase()) == 6000, "actor arrivals")
            groups.append({**group, "arrivals": 6000, **quantiles(delays)})
        latency = quantiles(self.latencies)
        tail_candidate = (self.counts["mailbox_full"] == self.counts["pending_full"] == self.counts["timed_out"] == 0
                          and latency["p99_ns"] is not None and latency["p99_ns"] <= 20_000_000
                          and latency["p999_ns"] is not None and latency["p999_ns"] <= 50_000_000)
        return {"arrivals": 600000, **self.counts, **latency, "per_actor": groups,
                "timeout_ratio": self.counts["timed_out"] / 600000,
                "refusal_ratio": (self.counts["mailbox_full"] + self.counts["pending_full"]) / 600000,
                "on_time_replies_per_second": self.counts["reply"] / 60,
                "generator_lateness": quantiles(self.lateness), "warmup": self.warmup,
                "cpu": {"warmup_started_completions": self.warmup_hot, "sample_started_completions": len(self.hot_durations),
                        "completed_in_sample": self.sample_hot_completions,
                        "completions_per_second": self.sample_hot_completions / 60, **quantiles(self.hot_durations)},
                "yield_observation": yield_summary, "tail_candidate_pass": tail_candidate,
                "fairness_gate_proven": False, "drained": True}


def validate_trace(path, result, *, version=8, replay_factory=Replay, max_trace=MAX_TRACE):
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
        require(offset < STAGE_NS, "observation origin offset")
    else:
        require(environment["otp"] == "29" and type(environment["schedulers"]) is int
                and environment["schedulers"] == 1 and result.get("yield_observation") is None, "BEAM environment")
        offset = 0
    require(path.stat().st_size <= max_trace and path.stat().st_size % measure.EVENT.size == 0, "trace bytes")
    replay = replay_factory(offset)
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
    parser = argparse.ArgumentParser(description="Continuous million-add CPU and exact-burst heartbeats, 30s/60s, five pairs")
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--observe", action="store_true", help="enable Wings yield diagnostics; reports instrumentation mode")
    measure_cpu.add_arguments(parser)
    arguments = parser.parse_args()
    cpu_control = measure_cpu.from_arguments(arguments)
    require(aot.PROFILE == "release", "WINGS_PROFILE=release required")
    escript = shutil.which("escript")
    require(escript is not None, "OTP 29 escript required; no model fallback")
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
                   "yield_observation_enabled": arguments.observe, "warmup_seconds": 30, "sample_seconds": 60,
                   "repetitions": 5, "memory_probe_build": memory_build,
                   "cpu_quota_preflight": None if cpu_control is None else cpu_control.initial,
                   "inputs": {str(path.relative_to(frozen)): hashlib.sha256(path.read_bytes()).hexdigest()
                              for path in sorted(frozen.rglob("*")) if path.is_file()},
                   "host_isolation": "unverified; exploratory integration measurement, not acceptance",
                   "limits": {"cpu_seconds": 240, "wall_seconds": 130, "stage_seconds": 120,
                              "file_bytes": measure.MAX_TRACE, "observed_rss_stop_bytes": measure.MAX_RSS},
                   "protocol_max_trace_bytes": MAX_TRACE,
                   "missing_gates": ["OS CPU hard quota", "monitoring overhead acceptance", "fairness overload groups",
                                     "native dual-platform", "same-workload throughput and memory acceptance"]}
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    require(shutil.disk_usage(output).free > MAX_TRACE * 10 + 1024**3, "insufficient disk for all bounded raw traces")
    results = []
    for repetition in range(5):
        for runtime in (("wings", "beam") if repetition % 2 == 0 else ("beam", "wings")):
            name = f"{repetition + 1}-{runtime}"
            trace = output / f"{name}.events.bin"
            command = ([frozen / "beam_fairness", "--trusted", frozen / "artifact", "--measure", trace]
                       if runtime == "wings" else [escript, frozen / "beam_fairness.escript", "--measure", trace])
            if runtime == "wings" and arguments.observe:
                command.append("--observe")
            result = measure.measurement(command, trace, output / name, "fairness", 10000, probe,
                                         runtime=runtime,
                                         cpu_control=cpu_control,
                                         validator=lambda path, _name, _rate, result: validate_trace(path, result))
            results.append({"runtime": runtime, "repetition": repetition + 1, **result["summary"],
                            "platform_memory": result["platform_memory"],
                            "rss_tree_plus_driver_peak_bytes": result["rss_tree_plus_driver_peak_bytes"]})
            require(all(hashlib.sha256((frozen / filename).read_bytes()).hexdigest() == digest
                        for filename, digest in environment["inputs"].items()), "frozen input changed")
            (output / "report.json").write_text(json.dumps({"runs": results, "fairness_gate_proven": False,
                                                            "memory_gate_proven": False}, indent=2) + "\n")
            print(f"{name}: heartbeat tail candidate {result['summary']['tail_candidate_pass']}; no fairness/MVP gate", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Fairness measurement failed: {error}", file=sys.stderr)
        sys.exit(3)
