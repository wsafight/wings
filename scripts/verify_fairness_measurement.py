import argparse
import copy
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import measure_fairness as fairness
import measure_open_loop as measure
import verify_scalar_aot as aot
from verify_multi_counter import invoke
from verify_open_loop import require


def rejected(action):
    try:
        action()
    except (ValueError, KeyError):
        return
    raise AssertionError("accepted forged fairness history")


def replay(records):
    model = fairness.Replay()
    for record in records:
        model.consume(record)
    return model


def verify(compare_beam):
    records = [(1, 0, 0, 0, 5)]
    values = [0] * 100
    for sequence in range(1, 301):
        sent = fairness.planned(sequence) + (sequence - 1) % 100 * 1000
        actor = (sequence - 1) % 100
        values[actor] += 1
        records.extend(((sequence, sent, 0, 0, 0), (sequence, sent, sent + 1, values[actor], 1)))
    records.append((1, 0, records[-1][2] + 1, 1000000, 6))
    model = replay(records)
    require(model.values == [1] + [3] * 100 and not model.pending and model.hot is None, "burst FIFO/results")
    rejected(lambda: model.finish({"committed": model.values}))
    for boundary, expected in ((1, 0), (100, 0), (101, 10_000_000), (300000, 29_990_000_000),
                               (300001, 30_000_000_000), (900000, 89_990_000_000)):
        require(fairness.planned(boundary) == expected, "exact phase/burst boundary")
    changed = []
    for row, column, value in ((0, 0, 2), (0, 1, -1), (0, 2, 1), (1, 0, 2), (1, 1, -1),
                               (1, 2, 1), (1, 4, 4), (2, 0, 2), (2, 1, 1), (2, 2, -1),
                               (2, 3, 2), (2, 4, 2), (201, 1, 0), (202, 3, 1),
                               (-1, 2, fairness.STAGE_NS), (-1, 3, 999999), (-1, 4, 5), (1, 0, True)):
        forged = [list(record) for record in records]
        forged[row][column] = value
        changed.append(forged)
    changed.extend((records[1:], records[:2] + records[1:]))
    for forged in changed:
        rejected(lambda: replay(forged))
    loaded = replay([(1, 0, 0, 0, 5)])
    for sequence in range(1, 256):
        loaded.consume((sequence, fairness.planned(sequence), 0, 0, 0))
    rejected(lambda: copy.deepcopy(loaded).consume((256, fairness.planned(256), 0, 0, 0)))
    loaded.consume((256, fairness.planned(256), 0, 0, 4))
    for sequence in range(1, 256):
        sent, _actor, value = loaded.pending[sequence]
        loaded.consume((sequence, sent, 500_000_000 + sequence, value, 2))
    require(not loaded.pending and loaded.warmup["timed_out"] == 255
            and loaded.warmup["pending_full"] == 1, "deadline does not remove actual pending")
    rejected(lambda: loaded.consume((255, fairness.planned(255), 500_000_300, 3, 2)))
    absent = replay([(1, 0, 0, 0, 5), (1, 0, 1, 1000000, 6)])
    rejected(lambda: absent.consume((1, 1, 0, 0, 0)))
    observed = replay([(1, 10, 20, 1, 7), (3, 30, 40, 101, 7)])
    require(observed.yield_count == 2 and observed.yield_gaps == 1, "yield loss accounting")
    for record in ((1, 0, 1, 0, 7), (2, 0, 1, 1, 7), (1, 2, 1, 1, 7), (1, 0, fairness.STAGE_NS, 1, 7)):
        rejected(lambda: replay([record]))
    rejected(lambda: observed.consume((4, 29, 41, 1, 7)))
    rejected(lambda: observed.consume((3, 40, 41, 1, 7)))
    overlap = replay([(1, 10, 20, 1, 7), (2, 15, 30, 1, 7)])
    require(overlap.yield_count == 2 and overlap.yield_gaps == 0, "later request survives earlier confirmation")
    with tempfile.TemporaryDirectory(prefix="wings-fairness-measure-model-") as temporary:
        parent = Path(temporary)
        broken = parent / "partial.bin"
        broken.write_bytes(b"short")
        invalid = {"version": 8, "status": "ok", "mode": "measure", "actors": 101, "state_bytes": 256,
                   "mailbox_capacity": 1, "pending_capacity": 255, "deadline_ns": 100_000_000,
                   "message_bytes": 32, "samples": [], "hot": [], "states_verified": True,
                   "resources_released": True, "observation_offset_ns": 0,
                   "environment": {"runtime": "wings", "workers": 1, "slice_steps": 4096, "quantum_ns": 1_000_000}}
        rejected(lambda: fairness.validate_trace(broken, invalid))
        broken.write_bytes(b"")
        rejected(lambda: fairness.validate_trace(broken, invalid))
        sentinel = parent / "existing.bin"
        sentinel.write_bytes(b"retain")
        result = aot.command([aot.TARGET / aot.PROFILE / "examples/beam_fairness", "--trusted", parent / "missing", "--measure", sentinel], expected=3)
        require("File exists" in result.stderr and sentinel.read_bytes() == b"retain", "exclusive Wings trace creation")
        commands = [("wings", [aot.TARGET / aot.PROFILE / "examples/beam_fairness", "--trusted", parent / "missing"])]
        if compare_beam:
            escript = shutil.which("escript")
            require(escript is not None, "OTP 29 escript required")
            commands.append(("beam", [escript, aot.ROOT / "scripts/beam_fairness.escript"]))
            code, result = invoke([*commands[-1][1], "--measure", sentinel], b"")
            require(code == 3 and result == {"version": 8, "status": "oracle_error"}
                    and sentinel.read_bytes() == b"retain", "exclusive BEAM trace creation")
        for runtime, command in commands:
            for suffix in (("--measure",), ("--measure", "trace", "extra"), ("--measure", "trace", "--observe", "extra")):
                code, result = invoke([*command, *suffix], b"")
                require(code == 2 and result["status"] == "invalid" and result["version"] == (8 if runtime == "beam" else 7), "invalid long config")
        controls = [{"event": "start", "warmup_seconds": 30, "sample_seconds": 60},
                    {"event": "sampling", "elapsed_ns": 30_000_000_000},
                    {"version": 8, "status": "synthetic", "environment": {"runtime": "wings"},
                     "resources_released": True, "bootstrap_bytes": 168, "remaining_bytes": 168}]
        program = "import json; [print(json.dumps(item), flush=True) for item in " + repr(controls) + "]"
        calls = []
        def validator(path, name, rate, result):
            calls.append((path, name, rate, result))
            return {"synthetic_control_validation": True}
        collected = measure.measurement([sys.executable, "-c", program], parent / "synthetic.bin", parent / "collector", "fairness", 10000,
                                        runtime="wings", validator=validator)
        require(collected["status"] == "ok" and calls == [(parent / "synthetic.bin", "fairness", 10000, controls[-1])], "independent collector validator")
        for index, change in enumerate([
            {"bootstrap_bytes": None}, {"remaining_bytes": 0}, {"bootstrap_bytes": True},
            {"remaining_bytes": 169}, {"environment": {"runtime": "beam"}},
        ]):
            forged = copy.deepcopy(controls)
            forged[-1].update(change)
            program = "import json; [print(json.dumps(item), flush=True) for item in " + repr(forged) + "]"
            destination = parent / f"forged-receipt-{index}"
            rejected(lambda: measure.measurement([sys.executable, "-c", program], parent / "synthetic.bin", destination,
                                                 "fairness", 10000, runtime="wings", validator=validator))
            require(json.loads(destination.with_suffix(".json").read_text())["status"] == "invalid_experiment",
                    "forged collection receipt preserved as invalid")
        require(len(calls) == 1, "forged collection reached the trace validator")
    print(f"Fairness v8 bounded model: exact phases/bursts, retained pending, CPU/FIFO, yield loss, {len(changed)} forged histories, strict CLI and collector checked; no long experiment")


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description="Bounded streamed fairness model; no long experiment")
        parser.add_argument("--compare-beam", action="store_true")
        verify(parser.parse_args().compare_beam)
    except (OSError, ValueError, RuntimeError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Fairness measurement verification failed: {error}", file=sys.stderr)
        sys.exit(3)
