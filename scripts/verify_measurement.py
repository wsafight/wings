import argparse
import copy
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

import measure_open_loop as measure
import verify_memory
import verify_scalar_aot as aot
from verify_multi_counter import invoke
from verify_open_loop import require, target


def rejected(function):
    try:
        function()
    except (ValueError, KeyError):
        return
    raise AssertionError("accepted invalid measurement trace")


def models(parent):
    trace = parent / "synthetic.bin"
    values = [0] * 10000
    latencies = []
    with trace.open("wb") as output:
        for sequence in range(1, 90001):
            planned = (sequence - 1) * 1_000_000
            actor = target("sparse-256", sequence - 1) - 1
            if sequence in (1, 40000):
                output.write(measure.EVENT.pack(sequence, planned, 0, 0, 3))
                continue
            values[actor] += 1
            delay = sequence % 1000 * 1000
            if sequence == 90000:
                delay = measure.DEADLINE_NS
            output.write(measure.EVENT.pack(sequence, planned, 0, 0, 0))
            output.write(measure.EVENT.pack(sequence, planned, planned + delay, values[actor], 2 if sequence == 90000 else 1))
            if planned >= measure.WARMUP_NS:
                latencies.append(delay)
    result = {"version": 6, "status": "ok", "mode": "measure", "actors": 10000,
              "state_bytes": 256, "mailbox_capacity": 1, "rate": 1000,
              "deadline_ns": measure.DEADLINE_NS, "pending_capacity": 256, "message_bytes": 32,
              "states_verified": True, "samples": [], "committed": values, "resources_released": True,
              "environment": {"runtime": "wings", "workers": 2}}
    summary = measure.validate_trace(trace, "sparse-256", 1000, result)
    ordered = sorted(latencies)
    require(summary["arrivals"] == 60000 and summary["timed_out"] == 1 and summary["mailbox_full"] == 1, "phase totals")
    for field, numerator, denominator in (("p50_ns", 1, 2), ("p99_ns", 99, 100), ("p999_ns", 999, 1000)):
        rank = (len(ordered) * numerator + denominator - 1) // denominator
        require(summary[field] == ordered[rank - 1], "exact radix nearest-rank")
    require(summary["max_ns"] == measure.DEADLINE_NS and not summary["tail_candidate_pass"], "late result retained")
    require(summary["generator_lateness"]["p999_ns"] == 0, "lateness includes refusals")
    beam = copy.deepcopy(result)
    beam["environment"] = {"runtime": "beam", "otp": "29", "schedulers": 2}
    measure.validate_trace(trace, "sparse-256", 1000, beam)
    changed = parent / "changed.bin"
    with trace.open("rb") as source:
        prefix = source.read(120)
    for offset, value in ((0, 2), (8, 120000000000), (16, 1), (24, 1), (32, 4), (32, 5),
                          (40, 1), (48, 0), (72, 1), (80, 1), (96, 100000000), (104, 2), (112, 2)):
        shutil.copyfile(trace, changed)
        with changed.open("r+b") as output:
            output.seek(offset)
            output.write(struct.pack(">Q", value))
        rejected(lambda: measure.validate_trace(changed, "sparse-256", 1000, result))
    changed.write_bytes(prefix[:-1])
    rejected(lambda: measure.validate_trace(changed, "sparse-256", 1000, result))
    changed.write_bytes(prefix)
    rejected(lambda: measure.validate_trace(changed, "sparse-256", 1000, result))
    invalid = copy.deepcopy(result)
    invalid["committed"][0] += 1
    rejected(lambda: measure.validate_trace(trace, "sparse-256", 1000, invalid))
    short = parent / "quantiles.bin"
    short.write_bytes(b"".join(measure.EVENT.pack(30001 + index, 30_000_000_000 + index * 1_000_000,
                                              30_000_000_000 + index * 1_000_000 + delay, 1, 1)
                               for index, delay in enumerate((0, 2**16 - 1, 2**16, 2**32, 99_000_000_000))))
    latency = measure.statistics()
    lateness = measure.statistics()
    for delay in (0, 2**16 - 1, 2**16, 2**32, 99_000_000_000):
        measure.observe(latency, delay)
    measure.percentiles(short, 1000, latency, lateness)
    require(latency["p50_ns"] == 2**16 and latency["p99_ns"] == 99_000_000_000 and latency["p999_ns"] is None, "multiword/small-sample quantile")
    bounded = parent / "pending.bin"
    bounded_values = [0] * 10000
    with bounded.open("wb") as output:
        for sequence in range(1, 258):
            planned = (sequence - 1) * 1_000_000
            output.write(measure.EVENT.pack(sequence, planned, 0, 0, 0 if sequence <= 256 else 4))
            if sequence <= 256:
                bounded_values[target("sparse-256", sequence - 1) - 1] += 1
        completed = [0] * 10000
        for sequence in range(1, 257):
            actor = target("sparse-256", sequence - 1) - 1
            completed[actor] += 1
            output.write(measure.EVENT.pack(sequence, (sequence - 1) * 1_000_000, 500_000_000, completed[actor], 2))
        for sequence in range(258, 90001):
            planned = (sequence - 1) * 1_000_000
            output.write(measure.EVENT.pack(sequence, max(planned, 500_000_000), 0, 0, 3))
    bounded_result = copy.deepcopy(result)
    bounded_result["committed"] = bounded_values
    bounded_summary = measure.validate_trace(bounded, "sparse-256", 1000, bounded_result)
    require(bounded_summary["warmup"]["timed_out"] == 256 and bounded_summary["warmup"]["pending_full"] == 1, "timeout pending retained")
    shutil.copyfile(bounded, changed)
    with changed.open("r+b") as output:
        output.seek(256 * 40 + 32)
        output.write(struct.pack(">Q", 0))
    rejected(lambda: measure.validate_trace(changed, "sparse-256", 1000, bounded_result))
    print("Measurement model: 90000 arrivals, phase/deadline/refusal accounting, exact bounded-memory quantiles and malformed/truncated traces")


def verify(compare_beam=False):
    verify_memory.verify()
    with tempfile.TemporaryDirectory(prefix="wings-measure-model-") as temporary:
        parent = Path(temporary)
        models(parent)
        failure = parent / "failure"
        failed = False
        try:
            measure.measurement([sys.executable, "-c", "raise SystemExit(7)"], parent / "absent.bin", failure, "sparse-256", 1000)
        except ValueError:
            failed = True
        require(failed and json.loads(failure.with_suffix(".json").read_text())["status"] == "invalid_experiment", "child failure preserved")
        failure = parent / "incomplete"
        failed = False
        try:
            measure.measurement([sys.executable, "-c", "print('{}')"], parent / "absent.bin", failure, "sparse-256", 1000)
        except ValueError:
            failed = True
        require(failed and json.loads(failure.with_suffix(".json").read_text())["status"] == "invalid_experiment", "truncated control preserved")
        failure = parent / "not-started"
        failed = False
        try:
            measure.measurement([parent / "missing-executable"], parent / "absent.bin", failure, "sparse-256", 1000)
        except OSError:
            failed = True
        require(failed and json.loads(failure.with_suffix(".json").read_text())["status"] == "invalid_experiment", "startup failure preserved")
        limited = subprocess.run(
            [sys.executable, "-c", 'import resource; resource.setrlimit(resource.RLIMIT_CPU, (5, 5)); resource.setrlimit(resource.RLIMIT_FSIZE, (4096, 8192)); import measure_open_loop; measure_open_loop.limit_child(); assert resource.getrlimit(resource.RLIMIT_CPU) == (5, 5); assert resource.getrlimit(resource.RLIMIT_FSIZE) == (4096, 4096); assert resource.getrlimit(resource.RLIMIT_CORE) == (0, 0)'],
            cwd=aot.ROOT / "scripts", capture_output=True, text=True, timeout=3,
        )
        require(limited.returncode == 0 and not limited.stdout and not limited.stderr, "inherited harder limits preserved")
        commands = [[aot.TARGET / aot.PROFILE / "examples/beam_open_loop", "--trusted", parent / "missing", "--measure"]]
        if compare_beam:
            escript = shutil.which("escript")
            require(escript is not None, "OTP29 required")
            commands.append([escript, aot.ROOT / "scripts/beam_open_loop.escript", "--measure"])
        for command in commands:
            for name, rate in (("unknown", 1000), ("sparse-256", 0), ("hot-queue", 1000),
                               ("mixed-4k", 1001), ("dense-64k", 256000), ("hot-queue", 2560000),
                               ("sparse-256", "false"), ("sparse-256", "1.0"), ("sparse-256", -1)):
                trace = parent / "not-created.bin"
                code, result = invoke([*command, name, rate, trace], b"")
                require(code == 2 and result == {"version": 6, "status": "invalid"} and not trace.exists(), "invalid measure config created file or ran")
            trace = parent / "existing.bin"
            trace.write_bytes(b"preserve existing trace")
            try:
                code, result = invoke([*command, "hot-queue", 10000, trace], b"")
            except RuntimeError:
                require(command[0] != shutil.which("escript"), "unexpected BEAM stderr")
            else:
                require(code == 3 and result == {"version": 6, "status": "oracle_error"}, "existing trace accepted")
            require(trace.read_bytes() == b"preserve existing trace", "existing trace overwritten")
        print(f"Measurement config: {'Wings/BEAM' if compare_beam else 'Wings (BEAM not executed)'} pre-load rejections; no long experiment executed")


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description="Bounded long-run protocol/collector validation; no long experiment")
        parser.add_argument("--compare-beam", action="store_true")
        verify(parser.parse_args().compare_beam)
    except (OSError, ValueError, RuntimeError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Measurement verification failed: {error}", file=sys.stderr)
        sys.exit(3)
