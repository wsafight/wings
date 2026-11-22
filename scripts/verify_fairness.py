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
from verify_open_loop import integer, require


def quantiles(values):
    ordered = sorted(values)
    count = len(ordered)
    return {"samples": count,
            "p50_ns": ordered[(count + 1) // 2 - 1] if count else None,
            "p99_ns": ordered[(count * 99 + 99) // 100 - 1] if count else None,
            "p999_ns": ordered[(count * 999 + 999) // 1000 - 1] if count >= 10000 else None,
            "max_ns": ordered[-1] if count else None}


def validate_yields(result):
    require(integer(result["capacity"], 1) == 64, "yield capacity")
    require(result["pending"] is False and result["overflowed"] is False, "yield incomplete counters")
    counters = {key: integer(result[key]) for key in
                ("requests", "coalesced", "consumed", "cleared", "confirmed", "retained", "taken", "dropped", "published")}
    require(counters["requests"] == counters["coalesced"] + counters["consumed"] + counters["cleared"], "yield request accounting")
    require(counters["consumed"] == counters["confirmed"] == counters["published"], "yield confirmation accounting")
    require(counters["retained"] == 0 and counters["confirmed"] == counters["taken"] + counters["dropped"], "yield queue accounting")
    samples = result["samples"]
    require(type(samples) is list and len(samples) == counters["taken"] <= 4096, "yield history size")
    require(counters["confirmed"] == 0 or bool(samples), "yield first accepted sample missing")
    previous_sequence = 0
    previous_request = 0
    previous_yield = 0
    delays = []
    by_actor = [[] for _ in range(101)]
    for sample in samples:
        require(type(sample) is list and len(sample) == 4, "yield sample shape")
        sequence, actor, requested, yielded = sample
        require(previous_sequence < integer(sequence, 1) <= counters["confirmed"], "yield sequence")
        require(1 <= integer(actor, 1) <= 101, "yield actor")
        require(previous_request <= integer(requested) <= integer(yielded) < 3_000_000_000
                and previous_yield <= yielded, "yield monotonic clock")
        previous_sequence = sequence
        previous_request = requested
        previous_yield = yielded
        delays.append(yielded - requested)
        by_actor[actor - 1].append(yielded - requested)
    require(not samples or samples[0][0] == 1, "yield first sample overwritten")
    require(counters["dropped"] != 0 or previous_sequence == counters["confirmed"], "yield missing sequence")
    return {**counters, **quantiles(delays), "per_actor": [
        {"actor": index + 1, **quantiles(values)} for index, values in enumerate(by_actor)],
        "complete": counters["dropped"] == 0,
        "has_data": bool(samples), "fairness_gate_proven": False}


def yield_model_tests():
    result = {"capacity": 64, "pending": False, "overflowed": False, "requests": 5,
              "coalesced": 1, "consumed": 3, "cleared": 1, "confirmed": 3,
              "retained": 0, "taken": 2, "dropped": 1, "published": 3,
              "samples": [[1, 1, 0, 10], [3, 101, 10, 30]]}
    summary = validate_yields(result)
    require(summary["max_ns"] == 20 and not summary["complete"], "yield sample loss not disclosed")
    overlap = copy.deepcopy(result)
    overlap["samples"][1][2] = 9
    require(validate_yields(overlap)["max_ns"] == 21, "later request may precede earlier confirmation")
    empty = {key: (False if type(value) is bool else [] if key == "samples" else 64 if key == "capacity" else 0)
             for key, value in result.items()}
    summary = validate_yields(empty)
    require(summary["p50_ns"] is None and summary["p999_ns"] is None and not summary["has_data"], "zero yield fabricated latency")
    forged = []
    for key, value in (("pending", True), ("overflowed", True), ("requests", 4), ("consumed", 2),
                       ("retained", 1), ("taken", 3), ("dropped", 0), ("published", 4), ("confirmed", True)):
        changed = copy.deepcopy(result)
        changed[key] = value
        forged.append(changed)
    for row, column, value in ((0, 0, 2), (1, 0, 1), (1, 0, 4), (0, 1, 0), (0, 1, 102),
                               (1, 2, -1), (1, 3, 9), (1, 3, 3_000_000_000), (0, 2, False)):
        changed = copy.deepcopy(result)
        changed["samples"][row][column] = value
        forged.append(changed)
    changed = copy.deepcopy(result)
    changed.update({"samples": [], "taken": 0, "dropped": 3})
    forged.append(changed)
    changed = copy.deepcopy(result)
    changed["samples"][0][2] = 5
    changed["samples"][1][2] = 4
    forged.append(changed)
    for changed in forged:
        try:
            validate_yields(changed)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted forged scheduler yield observation")
    print(f"Scheduler yield model: request/queue conservation, zero-data and {len(forged)} forged histories checked")


def validate(result, duration, runtime):
    count = duration // 10 * 100
    for key, expected in {"version": 7, "status": "ok", "mode": "smoke", "actors": 101,
                          "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": 255,
                          "deadline_ns": 100_000_000, "message_bytes": 32, "states_verified": True}.items():
        require(type(result.get(key)) is type(expected) and result[key] == expected, key)
    require(result["environment"]["runtime"] == runtime, "runtime")
    if runtime == "wings":
        receipts.validate(result)
        require(result.get("resources_released") is True, "resources")
        require(integer(result["environment"]["workers"]) == 1, "workers")
        require(integer(result["environment"]["slice_steps"]) == 4096, "slice steps")
        require(integer(result["environment"]["quantum_ns"]) == 1_000_000, "quantum")
    else:
        require(result["environment"]["otp"] in ("27", "28", "29") and integer(result["environment"]["schedulers"]) == 1, "OTP/schedulers")
    hot = result["hot"]
    require(type(hot) is list and 1 <= len(hot) <= 10000, "CPU count")
    previous = 0
    for index, sample in enumerate(hot):
        require(type(sample) is list and len(sample) == 4, "CPU shape")
        sequence, sent, observed, value = sample
        require(integer(sequence, 1) == index + 1 and integer(value) == 1000000, "CPU result")
        integer(sent)
        integer(observed)
        require(sent >= previous and observed >= sent and observed < 3_000_000_000, "CPU clock")
        previous = observed
    samples = result["samples"]
    require(type(samples) is list and len(samples) == count, "heartbeats")
    committed = [len(hot)] + [0] * 100
    counts = {"reply": 0, "timed_out": 0, "mailbox_full": 0, "pending_full": 0}
    by_actor = [{"actor": identity, "arrivals": duration // 10, "reply": 0, "timed_out": 0,
                 "mailbox_full": 0, "pending_full": 0, "max_wait_ns": None} for identity in range(2, 102)]
    previous_sent = 0
    outstanding = []
    latencies = []
    overlapping = 0
    for index, sample in enumerate(samples):
        require(type(sample) is list and len(sample) == 7, "heartbeat shape")
        sequence, actor, planned, sent, status, observed, value = sample
        require(integer(sequence, 1) == index + 1 and integer(actor, 2) == 2 + index % 100, "heartbeat route")
        require(integer(planned) == index // 100 * 10_000_000, "burst plan")
        integer(sent)
        require(previous_sent <= sent < 3_000_000_000 and sent >= planned, "heartbeat send clock")
        previous_sent = sent
        require(type(status) is str and status in counts, "heartbeat status")
        counts[status] += 1
        by_actor[actor - 2][status] += 1
        outstanding = [completion for completion in outstanding if completion > sent]
        if status in ("reply", "timed_out"):
            require(len(outstanding) < 255, "heartbeat pending overflow")
            integer(observed)
            require(sent <= observed < 3_000_000_000, "heartbeat observation")
            require(status == ("reply" if observed < planned + 100_000_000 else "timed_out"), "heartbeat deadline")
            committed[actor - 1] += 1
            require(integer(value, 1) == committed[actor - 1], "heartbeat FIFO/value")
            outstanding.append(observed)
            delay = observed - planned
            latencies.append(delay)
            by_actor[actor - 2]["max_wait_ns"] = max(by_actor[actor - 2]["max_wait_ns"] or 0, delay)
            overlapping += any(begin <= observed < end for _, begin, end, _ in hot)
        else:
            require(observed is None and value is None, "refused heartbeat executed")
            if status == "pending_full":
                require(len(outstanding) == 255, "false pending-full")
    require(hot[0][1] <= samples[0][3], "CPU not enqueued before first heartbeat")
    require(all(sample[1] <= samples[-1][3] for sample in hot), "CPU continued after final burst")
    require(type(result["committed"]) is list and all(type(value) is int for value in result["committed"])
            and result["committed"] == committed, "final committed")
    require(sum(counts.values()) == count, "accounting")
    return {"arrivals": count, **counts, **quantiles(latencies), "per_actor": by_actor,
            "CPU_completed": len(hot), "observed_while_CPU_pending": overlapping,
            "preemption_latency_proven": False, "OS_single_core_quota_proven": False,
            "fairness_gate_proven": False}


def model_tests():
    samples = [[index + 1, index + 2, 0, 1, "reply", 1000 + index, 1] for index in range(100)]
    result = {"version": 7, "status": "ok", "mode": "smoke", "actors": 101,
              "state_bytes": 256, "mailbox_capacity": 1, "pending_capacity": 255,
              "deadline_ns": 100_000_000, "message_bytes": 32, "states_verified": True,
              "resources_released": True, "bootstrap_bytes": 168, "remaining_bytes": 168,
              "samples": samples, "hot": [[1, 0, 2_000_000, 1000000]],
              "committed": [1] * 101, "environment": {"runtime": "wings", "workers": 1, "slice_steps": 4096, "quantum_ns": 1_000_000}}
    summary = validate(result, 10, "wings")
    require(summary["observed_while_CPU_pending"] == 100 and summary["p999_ns"] is None, "overlap/small sample")
    late = copy.deepcopy(result)
    late["samples"][-1][4:6] = ["timed_out", 100_000_000]
    summary = validate(late, 10, "wings")
    require(summary["timed_out"] == 1 and summary["per_actor"][-1]["max_wait_ns"] == 100_000_000, "deadline and per actor")
    late["samples"][-1][4:6] = ["reply", 99_999_999]
    validate(late, 10, "wings")
    refused = copy.deepcopy(result)
    refused["samples"][-1][4:] = ["mailbox_full", None, None]
    refused["committed"][-1] = 0
    summary = validate(refused, 10, "wings")
    require(summary["mailbox_full"] == 1 and summary["per_actor"][-1]["max_wait_ns"] is None, "refusal not zero latency")
    bounded = copy.deepcopy(result)
    bounded["samples"] = []
    values = [1] + [0] * 100
    for index in range(300):
        actor = 2 + index % 100
        planned = index // 100 * 10_000_000
        if index < 255:
            values[actor - 1] += 1
            sample = [index + 1, actor, planned, planned + 1, "timed_out", 200_000_000, values[actor - 1]]
        else:
            sample = [index + 1, actor, planned, planned + 1, "pending_full", None, None]
        bounded["samples"].append(sample)
    bounded["hot"] = [[1, 0, 250_000_000, 1000000]]
    bounded["committed"] = values
    summary = validate(bounded, 30, "wings")
    require(summary["timed_out"] == 255 and summary["pending_full"] == 45, "timed-out pending retained")
    overflow = copy.deepcopy(bounded)
    actor = overflow["samples"][255][1]
    overflow["samples"][255][4:] = ["timed_out", 200_000_000, values[actor - 1] + 1]
    overflow["committed"][actor - 1] += 1
    try:
        validate(overflow, 30, "wings")
    except ValueError:
        pass
    else:
        raise AssertionError("accepted pending reuse before actual reply")
    forged = []
    for column, value in [(0, True), (0, 2), (1, 1), (2, 1), (3, -1), (3, 0.0),
                          (4, "pending"), (4, "timed_out"), (4, "pending_full"), (5, 100_000_000), (6, 2), (6, True)]:
        changed = copy.deepcopy(result)
        changed["samples"][0][column] = value
        forged.append(changed)
    for column, value in [(0, 2), (1, 2), (2, -1), (3, 999999), (3, 1000000.0)]:
        changed = copy.deepcopy(result)
        changed["hot"][0][column] = value
        forged.append(changed)
    changed = copy.deepcopy(result)
    changed["samples"].pop()
    forged.append(changed)
    changed = copy.deepcopy(result)
    changed["committed"][0] = 2
    forged.append(changed)
    for changed in forged:
        try:
            validate(changed, 10, "wings")
        except ValueError:
            pass
        else:
            raise AssertionError("accepted forged fairness history")
    require(quantiles([7] * 10000)["p999_ns"] == 7, "nearest rank threshold")
    print(f"Fairness model: deadline/late/refusal/per-actor waits, retained pending and {len(forged) + 1} forged histories checked")


def verify(artifact, compare_beam, output):
    model_tests()
    yield_model_tests()
    commands = [("wings", [aot.TARGET / aot.PROFILE / "examples/beam_fairness", "--trusted", artifact])]
    if compare_beam:
        escript = shutil.which("escript")
        require(escript is not None, "OTP 29 escript required")
        commands.append(("beam", [escript, aot.ROOT / "scripts/beam_fairness.escript"]))
    if output:
        output.mkdir(parents=True, exist_ok=True)
    for duration in (10, 30):
        for runtime, command in commands:
            code, result = invoke([*command, duration], b"")
            if output:
                (output / f"{runtime}-{duration}.json").write_text(json.dumps(result) + "\n")
            require(code == 0, f"{runtime} exit {code}: {result}")
            summary = validate(result, duration, runtime)
            if output:
                (output / f"{runtime}-{duration}-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
            print(f"Fairness smoke {runtime} {duration}ms: {summary['CPU_completed']} million-add tasks, "
                  f"{summary['reply']} replies/{summary['timed_out']} timed out/{summary['mailbox_full']} mailbox full/"
                  f"{summary['pending_full']} pending full; not fairness gate")
        code, result = invoke([*commands[0][1], duration, "--observe"], b"")
        require(code == 0, f"observed scheduler exit {code}: {result}")
        validate(result, duration, "wings")
        summary = validate_yields(result["yield_observation"])
        if output:
            (output / f"wings-observed-{duration}.json").write_text(json.dumps(result) + "\n")
            (output / f"wings-observed-{duration}-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"Scheduler yield {duration}ms: {summary['samples']} samples/{summary['dropped']} dropped/"
              f"{summary['cleared']} cleared/{summary['coalesced']} coalesced; no fairness gate")
    for runtime, command in commands:
        invalid = command if runtime == "beam" else [*command[:2], artifact / "missing"]
        for duration in ("0", "1", "20", "030", "100", "-10", "10.0", "false", "18446744073709551616"):
            code, result = invoke([*invalid, duration], b"")
            require(code == 2 and result == {"version": 7, "status": "invalid"}, "invalid duration")
    for suffix in (("10", "--unknown"), ("10", "--observe", "--observe"), ("030", "--observe")):
        code, result = invoke([*commands[0][1][:2], artifact / "missing", *suffix], b"")
        require(code == 2 and result == {"version": 7, "status": "invalid"}, "invalid observed config")
    for budget, expected in ((100, 1), (10000000, 0)):
        result = aot.command([aot.VM, "run", "--trusted", artifact, "--heap-records", 4, "--max-steps", budget],
                             '{"message":"cpu","args":[1,2,0,0]}\n', expected=expected)
        require("budget" in result.stderr if expected else json.loads(result.stdout) == {"reply": 1000000}, "CPU budget bypass/value")
    code, observed = invoke([aot.TARGET / aot.PROFILE / "examples/observed_yield", "--trusted", artifact], b"")
    require(code == 0, f"observed yield exit {code}: {observed}")
    for key, expected in {"version": 1, "status": "ok", "states_verified": True,
                          "resources_released": True, "fairness_gate_proven": False}.items():
        require(type(observed.get(key)) is type(expected) and observed[key] == expected, f"observed {key}")
    require(type(observed["samples"]) is list and len(observed["samples"]) == 2, "observed yield samples")
    for index, sample in enumerate(observed["samples"]):
        require(type(sample["remote"]) is bool and sample["remote"] == (index == 1), "yield request origin")
        require(type(sample["observed"]) is bool, "yield observation flag")
        if sample["observed"]:
            require(integer(sample["latency_ns"]) < 3_000_000_000, "yield latency bound")
            require(integer(sample["steps_before_resume"], 1) == 1, "yield resume steps")
            require(integer(sample["steps_at_yield"], 1) >= 1, "yield consumed steps")
            require(index == 1 or sample["steps_at_yield"] == 1, "pre-request executed before yield")
        else:
            require(index == 1 and sample["latency_ns"] is None
                    and sample["reason"] == "request_not_consumed_before_completion", "missing yield invented latency")
    if output:
        (output / "observed-yield.json").write_text(json.dumps(observed, indent=2) + "\n")
    print("Atomic yield observation: pre-request and remote request checked after real coroutine return; no fairness gate")
    print(f"Fairness v7 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: "
          "two exact-burst schedules, strict duration and CPU budget checked; no performance claim")


def main():
    parser = argparse.ArgumentParser(description="Bounded million-add CPU and exact-burst heartbeats; not fairness acceptance")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam, arguments.output)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-fairness-") as temporary:
            artifact = Path(temporary) / "fairness"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-fairness.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam, arguments.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Fairness verification failed: {error}", file=sys.stderr)
        sys.exit(3)
