import argparse
import copy
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import verify_scalar_aot as aot
import verify_resource_receipt as receipts
from verify_multi_counter import invoke


CASES = {
    "sparse-256": (10000, 256, 1, 1000),
    "mixed-4k": (1000, 4096, 1, 1000),
    "dense-64k": (100, 65536, 1, 1000),
    "hot-queue": (100, 4096, 100, 10000),
}
DEADLINE_NS = 100_000_000


def target(name, index):
    if name != "hot-queue":
        return index % 100 + 1
    return 1 if index % 100 < 90 else 2 + (index // 100 * 10 + index % 100 - 90) % 99


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, minimum=0):
    require(type(value) is int and value >= minimum, "integer")
    return value


def validate(result, name, duration, rate, runtime):
    actors, length, mailbox, _ = CASES[name]
    for field, expected in {
        "version": 5, "status": "ok", "mode": "smoke", "actors": actors,
        "state_bytes": length, "mailbox_capacity": mailbox, "rate": rate,
        "deadline_ns": DEADLINE_NS, "pending_capacity": 256, "message_bytes": 32,
        "states_verified": True,
    }.items():
        require(type(result.get(field)) is type(expected) and result[field] == expected, field)
    require(result["environment"]["runtime"] == runtime, "runtime")
    if runtime == "wings":
        receipts.validate(result)
        require(result.get("resources_released") is True, "resources")
        require(integer(result["environment"]["workers"]) == 2, "workers")
    else:
        require(result["environment"]["otp"] == "29", "OTP")
        require(integer(result["environment"]["schedulers"]) == 2, "schedulers")
    samples = result["samples"]
    require(type(samples) is list and len(samples) == (duration * rate + 999) // 1000, "sample count")
    values = [0] * actors
    counts = {"reply": 0, "timed_out": 0, "mailbox_full": 0, "pending_full": 0}
    previous_sent = 0
    outstanding = []
    for index, sample in enumerate(samples):
        require(type(sample) is list and len(sample) == 7, "sample shape")
        sequence, actor, planned, sent, status, observed, value = sample
        require(integer(sequence, 1) == index + 1, "sequence")
        require(integer(actor, 1) == target(name, index), "route")
        require(integer(planned) == index * 1_000_000_000 // rate, "plan")
        integer(sent)
        require(sent >= planned and sent >= previous_sent, "send clock")
        previous_sent = sent
        require(type(status) is str and status in counts, "outcome")
        counts[status] += 1
        outstanding = [completion for completion in outstanding if completion > sent]
        if status in ("reply", "timed_out"):
            require(len(outstanding) < 256, "pending overflow")
            integer(observed)
            require(observed >= sent, "observation clock")
            require(status == ("reply" if observed < planned + DEADLINE_NS else "timed_out"), "deadline classification")
            values[actor - 1] += 1
            require(integer(value, 1) == values[actor - 1], "per-target FIFO/value")
            outstanding.append(observed)
        else:
            require(observed is None and value is None, "refused request executed")
            if status == "pending_full":
                require(len(outstanding) == 256, "false pending-full")
    require(type(result["committed"]) is list and len(result["committed"]) == actors, "state count")
    require(all(type(value) is int for value in result["committed"]), "state integers")
    require(result["committed"] == values, "committed state")
    require(sum(counts.values()) == len(samples), "accounting")
    return counts


def model_tests():
    name, duration, rate = "hot-queue", 1, 10000
    samples = [[index + 1, 1, index * 100000, index * 100000, "reply", index * 100000 + 1, index + 1]
               for index in range(10)]
    result = {"version": 5, "status": "ok", "mode": "smoke", "actors": 100,
              "state_bytes": 4096, "mailbox_capacity": 100, "rate": rate, "deadline_ns": DEADLINE_NS,
              "pending_capacity": 256, "message_bytes": 32, "states_verified": True,
              "resources_released": True, "bootstrap_bytes": 168, "remaining_bytes": 168,
              "environment": {"runtime": "wings", "workers": 2},
              "samples": samples, "committed": [10] + [0] * 99}
    validate(result, name, duration, rate, "wings")
    late = copy.deepcopy(result)
    late["samples"][-1][4:6] = ["timed_out", late["samples"][-1][2] + DEADLINE_NS]
    require(validate(late, name, duration, rate, "wings")["timed_out"] == 1, "deadline edge")
    late["samples"][-1][4:6] = ["reply", late["samples"][-1][2] + DEADLINE_NS - 1]
    validate(late, name, duration, rate, "wings")
    bounded = copy.deepcopy(result)
    bounded["rate"] = 257000
    bounded["samples"] = []
    totals = [0] * 100
    for index in range(257):
        actor = target(name, index)
        planned = index * 1_000_000_000 // 257000
        if index < 256:
            totals[actor - 1] += 1
            sample = [index + 1, actor, planned, planned, "timed_out", 200_000_000, totals[actor - 1]]
        else:
            sample = [index + 1, actor, planned, planned, "pending_full", None, None]
        bounded["samples"].append(sample)
    bounded["committed"] = totals
    require(validate(bounded, name, 1, 257000, "wings")["pending_full"] == 1, "bounded pending")
    refused = copy.deepcopy(result)
    refused["samples"][0][4:] = ["mailbox_full", None, None]
    for sample in refused["samples"][1:]:
        sample[6] -= 1
    refused["committed"][0] -= 1
    validate(refused, name, duration, rate, "wings")
    changed = []
    for column, value in [(0, 2), (0, True), (1, 2), (2, 1), (3, -1), (3, 0.0),
                          (4, "pending"), (4, "timed_out"), (4, "pending_full"),
                          (4, "mailbox_full"), (5, DEADLINE_NS), (5, None), (6, 2), (6, True)]:
        forged = copy.deepcopy(result)
        forged["samples"][0][column] = value
        changed.append(forged)
    forged = copy.deepcopy(result)
    forged["samples"].pop()
    changed.append(forged)
    forged = copy.deepcopy(result)
    forged["committed"][0] = 9
    changed.append(forged)
    forged = copy.deepcopy(result)
    forged["samples"][1][3] = 0
    changed.append(forged)
    forged = copy.deepcopy(result)
    forged["states_verified"] = 1
    changed.append(forged)
    forged = copy.deepcopy(bounded)
    actor = target(name, 256)
    forged["samples"][-1][4:] = ["timed_out", 200_000_000, totals[actor - 1] + 1]
    forged["committed"][actor - 1] += 1
    try:
        validate(forged, name, 1, 257000, "wings")
    except ValueError:
        pass
    else:
        raise AssertionError("accepted pending overflow after business deadline")
    for forged in changed:
        try:
            validate(forged, name, duration, rate, "wings")
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError("accepted forged open-loop trace")
    require([target(name, index) for index in range(90)] == [1] * 90, "hot prefix")
    require([target(name, index) for index in range(90, 100)] == list(range(2, 12)), "hot tail")
    require(target(name, 1090) == 3, "hot continuous round robin")
    print(f"Open-loop model: deadline/late reply, retained pending/refusal, routing and {len(changed) + 1} forged histories checked")


def verify(artifact, compare_beam, output):
    model_tests()
    wings = [aot.TARGET / aot.PROFILE / "examples/beam_open_loop", "--trusted", artifact]
    commands = [("wings", wings)]
    if compare_beam:
        escript = shutil.which("escript")
        if escript is None:
            raise RuntimeError("Erlang/OTP 29 escript is required")
        commands.append(("beam", [escript, aot.ROOT / "scripts/beam_open_loop.escript"]))
    workloads = [(name, 100, configuration[3]) for name, configuration in CASES.items()]
    workloads += [("hot-queue", 1, 1280000), ("dense-64k", 10, 1000000)]
    if output:
        output.mkdir(parents=True, exist_ok=True)
    for name, duration, rate in workloads:
        for runtime, command in commands:
            try:
                code, result = invoke([*command, name, duration, rate], b"")
            except (RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
                raise RuntimeError(f"{runtime} {name} {duration}ms {rate}/s: {error}") from error
            if output:
                (output / f"{runtime}-{name}-{duration}-{rate}.json").write_text(json.dumps(result) + "\n")
            require(code == 0, f"{runtime} exit {code}: {result}")
            counts = validate(result, name, duration, rate, runtime)
            print(f"Open-loop smoke {runtime} {name} {duration}ms {rate}/s: {counts}; no performance claim")
    rejected = [("unknown", 1, 1), ("hot-queue", 0, 1), ("hot-queue", 1001, 1),
                ("hot-queue", 1, 0), ("hot-queue", 1, 1280001), ("hot-queue", 1000, 10001),
                ("hot-queue", 1, "false"), ("hot-queue", 1, "1.0"), ("hot-queue", 1, -1),
                ("hot-queue", 1, 18446744073709551616)]
    for runtime, command in commands:
        for arguments in rejected:
            invalid_command = command if runtime == "beam" else [*wings[:2], artifact / "missing"]
            code, result = invoke([*invalid_command, *arguments], b"")
            require(code == 2 and result == {"version": 5, "status": "invalid"}, "invalid config accepted")
    print(f"Open-loop v5 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: "
          f"{len(workloads)} schedules and {len(rejected)} configuration rejections; smoke only")


def main():
    parser = argparse.ArgumentParser(description="Bounded planned-arrival Counter smoke, not performance acceptance")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam, arguments.output)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-open-loop-") as directory:
            artifact = Path(directory) / "counter"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-multi-counter.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam, arguments.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Open-loop verification failed: {error}", file=sys.stderr)
        sys.exit(3)
