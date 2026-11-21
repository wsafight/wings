import argparse
import copy
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tempfile

import verify_scalar_aot as aot
import verify_resource_receipt as receipts
from verify_counter_equivalence import strict_object
from verify_multi_counter import invoke, limit_child


MIN_I64 = -(2**63)
MAX_I64 = 2**63 - 1


def integer(value, minimum=MIN_I64, maximum=MAX_I64):
    assert type(value) is int and minimum <= value <= maximum, ("integer", value)
    return value


def decode(payload):
    if len(payload) > 65536:
        raise ValueError("input_bytes")
    workload = json.loads(payload.decode("utf-8"), object_pairs_hook=strict_object)
    if type(workload) is not dict or set(workload) != {"version", "actors", "state_bytes", "initial", "streams"}:
        raise ValueError("fields")
    integer(workload["version"], 9, 9)
    actors = integer(workload["actors"], 1, 100)
    assert type(workload["state_bytes"]) is int and workload["state_bytes"] in (256, 4096, 65536)
    initial = integer(workload["initial"])
    streams = workload["streams"]
    assert type(streams) is list and len(streams) == 2
    bounds = [[initial, initial] for _ in range(actors)]
    for stream in streams:
        assert type(stream) is list and len(stream) <= 128
        for request in stream:
            assert type(request) is dict and set(request) == {"actor", "delta", "fail"}
            actor = integer(request["actor"], 1, actors)
            delta = integer(request["delta"])
            assert type(request["fail"]) is bool
            bounds[actor - 1][0] += min(0, delta)
            bounds[actor - 1][1] += max(0, delta)
    for low, high in bounds:
        integer(low)
        integer(high)
    return workload


def bridge(workload):
    payload = struct.pack(">QQqQQQ", 9, workload["state_bytes"], workload["initial"], workload["actors"],
                          *(len(stream) for stream in workload["streams"]))
    for stream in workload["streams"]:
        sequences = {}
        for request in stream:
            actor = request["actor"]
            sequences[actor] = sequences.get(actor, 0) + 1
            payload += struct.pack(">QQqQ", actor, sequences[actor], request["delta"], 2 if request["fail"] else 1)
    return payload


def check(workload, result, runtime=None):
    assert type(result) is dict
    for key, expected in {"version": 9, "actors": workload["actors"], "state_bytes": workload["state_bytes"],
                          "message_bytes": 32, "states_verified": True}.items():
        assert type(result.get(key)) is type(expected) and result[key] == expected, key
    expected = {}
    routes = [[[], []] for _ in range(workload["actors"])]
    for sender, stream in enumerate(workload["streams"], 1):
        for request in stream:
            route = routes[request["actor"] - 1][sender - 1]
            route.append(request)
            expected[sender, request["actor"], len(route)] = request
    assert type(result.get("outcomes")) is list and len(result["outcomes"]) == len(expected)
    observed = {}
    for outcome in result["outcomes"]:
        assert type(outcome) is dict and set(outcome) == {"sender", "actor", "seq", "status", "value"}
        key = tuple(integer(outcome[field], 1, 128) for field in ("sender", "actor", "seq"))
        assert key in expected and key not in observed, key
        assert type(outcome["status"]) is str and outcome["status"] in ("reply", "failed", "closed")
        if outcome["status"] == "reply":
            integer(outcome["value"])
        else:
            assert outcome["value"] is None
        observed[key] = outcome
    assert type(result.get("failures")) is list
    failed = set()
    for failure in result["failures"]:
        assert type(failure) is dict and set(failure) == {"actor", "reason"}
        actor = integer(failure["actor"], 1, workload["actors"])
        assert actor not in failed and failure["reason"] == "injected_failure"
        failed.add(actor)
    expected_failed = {request["actor"] for stream in workload["streams"] for request in stream if request["fail"]}
    assert failed == expected_failed
    assert result.get("status") == ("failure" if failed else "ok")
    assert type(result.get("committed")) is list and len(result["committed"]) == workload["actors"]
    assert type(result.get("sequences")) is list and len(result["sequences"]) == workload["actors"]
    for actor, streams in enumerate(routes, 1):
        counts, sums = [], []
        for sender, stream in enumerate(streams, 1):
            count, prefix, closed = 0, [0], False
            for sequence, request in enumerate(stream, 1):
                outcome = observed[sender, actor, sequence]
                assert not closed or outcome["status"] == "closed", (actor, sender, "accept after closed")
                closed = closed or outcome["status"] == "closed"
                if outcome["status"] == "reply":
                    assert sequence == count + 1 and not request["fail"], (actor, sender, sequence)
                    count += 1
                    prefix.append(prefix[-1] + request["delta"])
            counts.append(count)
            sums.append(prefix)
        sequences = result["sequences"][actor - 1]
        assert type(sequences) is list and len(sequences) == 2
        assert [integer(sequence, 0, 128) for sequence in sequences] == counts
        final = integer(result["committed"][actor - 1])
        assert final == workload["initial"] + sums[0][-1] + sums[1][-1], actor
        reachable = {(0, 0)}
        for first in range(counts[0] + 1):
            for second in range(counts[1] + 1):
                if (first, second) not in reachable:
                    continue
                value = integer(workload["initial"] + sums[0][first] + sums[1][second])
                if first < counts[0] and observed[1, actor, first + 1]["value"] == value + streams[0][first]["delta"]:
                    reachable.add((first + 1, second))
                if second < counts[1] and observed[2, actor, second + 1]["value"] == value + streams[1][second]["delta"]:
                    reachable.add((first, second + 1))
        assert tuple(counts) in reachable, (actor, "no legal successful prefix")
        if actor in failed:
            assert any(counts[sender] < len(streams[sender])
                       and streams[sender][counts[sender]]["fail"]
                       and observed[sender + 1, actor, counts[sender] + 1]["status"] == "failed"
                       for sender in range(2)), (actor, "no accepted failure at prefix boundary")
        else:
            assert counts == list(map(len, streams)), (actor, "healthy request lost")
    if runtime is not None:
        environment = result["environment"]
        assert environment["runtime"] == runtime
        assert integer(environment["senders"], 2, 2) == 2
        assert integer(environment["window_per_sender"], 16, 16) == 16
        if runtime == "wings":
            receipts.validate(result)
            assert result["resources_released"] is True
            integer(environment["workers"], 2, 2)
        else:
            assert runtime == "beam" and environment["otp"] == "29"
            integer(environment["schedulers"], 2, 2)


def config(length=256, actors=1, initial=0, streams=((), ())):
    return {"version": 9, "actors": actors, "state_bytes": length, "initial": initial,
            "streams": [[{"actor": actor, "delta": delta, "fail": fail} for actor, delta, fail in stream]
                        for stream in streams]}


def cases():
    result = []
    for length in (256, 4096, 65536):
        result.extend([
            config(length),
            config(length, 3, -7, [[(1 + index % 3, (index + sender) % 7 - 3, False)
                                   for index in range(128)] for sender in range(2)]),
            config(length, 2, streams=([(1, 7, True)] * 17, [(2, 1, False)] * 33)),
            config(length, streams=([(1, 1, False), (1, 2, True), (1, 16, False)],
                                    [(1, 4, False), (1, -2, False), (1, 8, True)])),
            config(length, 3, streams=[[(1 + index % 3, sender + 1, index in (7, 15))
                                       for index in range(128)] for sender in range(2)]),
            config(length, initial=MIN_I64, streams=([], [(1, 1, index == 17) for index in range(33)])),
            config(length, initial=MAX_I64, streams=([(1, -1, index == 17) for index in range(33)], [])),
            config(length, 100, -7, [[(1 + index % 100, (index + sender) % 7 - 3, index in (0, 99, 127))
                                     for index in range(128)] for sender in range(2)]),
            config(length, 2, streams=([(1 + index % 2, 1, index == 0) for index in range(128)],
                                      [(1 + index % 2, -1, False) for index in range(128)])),
        ])
    return result


def enumerate_histories(workload):
    streams = workload["streams"]

    def visit(counts, value, replies):
        if counts == tuple(map(len, streams)):
            yield counts, value, replies, None
        for sender in range(2):
            sequence = counts[sender]
            if sequence == len(streams[sender]):
                continue
            request = streams[sender][sequence]
            if request["fail"]:
                yield counts, value, replies, (sender + 1, sequence + 1)
            else:
                next_counts = list(counts)
                next_counts[sender] += 1
                next_value = value + request["delta"]
                yield from visit(tuple(next_counts), next_value,
                                 {**replies, (sender + 1, sequence + 1): next_value})

    for counts, value, replies, trigger in visit((0, 0), workload["initial"], {}):
        outcomes = [{"sender": sender + 1, "actor": 1, "seq": sequence + 1,
                     "status": "reply" if (sender + 1, sequence + 1) in replies else "failed",
                     "value": replies.get((sender + 1, sequence + 1))}
                    for sender, stream in enumerate(streams) for sequence in range(len(stream))]
        yield {"version": 9, "status": "failure" if trigger else "ok", "actors": 1,
               "state_bytes": workload["state_bytes"], "message_bytes": 32, "states_verified": True,
               "committed": [value], "sequences": [list(counts)], "outcomes": outcomes,
               "failures": [{"actor": 1, "reason": "injected_failure"}] if trigger else []}


def model_tests():
    positive = 0
    failing = config(streams=([(1, 1, False), (1, 2, True), (1, 16, False)],
                              [(1, 4, False), (1, -2, False), (1, 8, True)]))
    successful = config(streams=([(1, 1, False), (1, 2, False)], [(1, 4, False), (1, 8, False)]))
    exhaustive = [config(streams=([(1, delta, index == first) for index, delta in enumerate((1, 2))],
                                  [(1, delta, index == second) for index, delta in enumerate((4, 8))]))
                  for first in (None, 0, 1) for second in (None, 0, 1)]
    for workload in [failing, successful, *exhaustive]:
        for result in enumerate_histories(workload):
            check(workload, result)
            positive += 1
            if result["failures"]:
                closed = copy.deepcopy(result)
                for outcome in closed["outcomes"]:
                    if outcome["status"] == "failed" and outcome["seq"] > closed["sequences"][0][outcome["sender"] - 1] + 1:
                        outcome["status"] = "closed"
                check(workload, closed)
                positive += 1
    valid = next(result for result in enumerate_histories(failing) if all(result["sequences"][0]))
    rejected = []
    for field, value in [("version", 9.0), ("status", "ok"), ("states_verified", 1),
                         ("message_bytes", 32.0), ("committed", [999]), ("sequences", [[0, 0]]),
                         ("failures", []), ("failures", [{"actor": True, "reason": "injected_failure"}]),
                         ("failures", [{"actor": 1, "reason": "overflow"}])]:
        rejected.append({**copy.deepcopy(valid), field: value})
    for field, value in [("sender", 0), ("actor", 2), ("seq", True), ("status", "timed_out"),
                         ("value", True), ("value", MAX_I64 + 1), ("value", 999), ("extra", 0)]:
        forged = copy.deepcopy(valid)
        forged["outcomes"][0][field] = value
        rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["outcomes"].pop()
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["outcomes"][-1] = forged["outcomes"][0]
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["failures"] *= 2
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["committed"][0] += 2
    forged["sequences"][0][0] += 1
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["outcomes"][-1]["status"] = "reply"
    forged["outcomes"][-1]["value"] = 999
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    forged["outcomes"][1]["status"] = "closed"
    rejected.append(forged)
    forged = copy.deepcopy(valid)
    for outcome in forged["outcomes"]:
        if outcome["status"] == "failed":
            outcome["status"] = "closed"
    rejected.append(forged)
    for forged in rejected:
        try:
            check(failing, forged)
        except AssertionError:
            pass
        else:
            raise AssertionError("accepted forged concurrent failure prefix")
    return positive, len(rejected)


def invalid_json():
    sample = config(streams=([(1, 7, True)], []))
    encoded = json.dumps(sample).encode()
    rejected = [b"", b"null", b"[]", b"true", b"[", b"\xff", encoded + b"{}", b" " * 65537,
                b"[" * 1500 + b"]" * 1500,
                encoded.replace(b'"version": 9', b'"version": 9, "version": 9'),
                encoded.replace(b'"fail": true', b'"fail": true, "fail": true')]
    for key, value in [("version", 3), ("version", True), ("version", 9.0), ("actors", 0),
                       ("actors", 101), ("actors", True), ("actors", 1.0), ("state_bytes", 257),
                       ("state_bytes", 256.0), ("initial", MIN_I64 - 1), ("initial", MAX_I64 + 1),
                       ("initial", False), ("initial", 0.0), ("streams", []), ("streams", [[], [], []]),
                       ("streams", [None, []]), ("streams", [[sample["streams"][0][0]] * 129, []]), ("extra", 0)]:
        rejected.append(json.dumps({**sample, key: value}).encode())
    rejected.extend(json.dumps({key: value for key, value in sample.items() if key != missing}).encode()
                    for missing in sample)
    for key, value in [("actor", 0), ("actor", 2), ("actor", True), ("actor", 1.0), ("delta", False),
                       ("delta", 0.0), ("delta", MIN_I64 - 1), ("delta", MAX_I64 + 1),
                       ("fail", 0), ("fail", 1), ("fail", None), ("extra", 0)]:
        rejected.append(json.dumps({**sample, "streams": [[{**sample["streams"][0][0], key: value}], []]}).encode())
    for missing in ("actor", "delta", "fail"):
        rejected.append(json.dumps({**sample, "streams": [[{key: value for key, value in sample["streams"][0][0].items()
                                                           if key != missing}], []]}).encode())
    rejected.extend(json.dumps(workload).encode() for workload in [
        config(initial=MAX_I64, streams=([(1, 1, True)], [])),
        config(initial=MIN_I64, streams=([], [(1, -1, False)])),
    ])
    return rejected


def reject_protocol(artifact, sender, sequence, operation, reason):
    messages = [aot.message("configure", 256, 0, 1), aot.message("apply", sender, sequence, 7, operation)]
    child = subprocess.Popen([str(aot.VM), "run", "--trusted", str(artifact), "--heap-records", "4"],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             cwd=aot.ROOT, start_new_session=True, preexec_fn=limit_child)
    try:
        stdout, stderr = child.communicate("".join(json.dumps(message) + "\n" for message in messages).encode(), timeout=10)
    except BaseException:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.communicate()
        raise
    assert child.returncode == 1 and stdout.splitlines() == [b'{"reply":0}'], (child.returncode, stdout, stderr)
    assert len(stderr) <= 65536 and reason in stderr, stderr


def verify(artifact, compare_beam):
    positive, forged = model_tests()
    wings = [aot.TARGET / aot.PROFILE / "examples/beam_failure_counter", "--trusted", artifact]
    commands = [(wings, "wings")]
    if compare_beam:
        escript = shutil.which("escript")
        if escript is None:
            raise RuntimeError("Erlang/OTP 29 escript is required")
        commands.append(([escript, aot.ROOT / "scripts/beam_failure_workload.escript"], "beam"))
    for sender in (1, 2):
        for sequence in (0, 2):
            reject_protocol(artifact, sender, sequence, 1, b"overflow")
        reject_protocol(artifact, sender, 1, 2, b"division by zero")
    valid = cases()
    for workload in valid:
        payload = json.dumps(workload, separators=(",", ":")).encode()
        decode(payload)
        expected_code = int(any(request["fail"] for stream in workload["streams"] for request in stream))
        for command, runtime in commands:
            code, result = invoke(command, bridge(workload) if runtime == "wings" else payload)
            assert code == expected_code, (code, result, workload)
            check(workload, result, runtime)
    sample = bridge(config(streams=([(1, 7, True)], [])))
    rejected = [sample[:length] for length in (0, 1, 47, 48, 79)] + [sample + b"\0", b"\0" * 8241]
    for index, value in [(0, 3), (1, 257), (3, 0), (3, 101), (4, 129), (6, 0), (6, 2),
                         (7, 2), (9, 0), (9, 3), (2, MAX_I64)]:
        changed = bytearray(sample)
        struct.pack_into(">Q", changed, index * 8, value)
        rejected.append(bytes(changed))
    for payload in rejected:
        code, result = invoke([*wings[:2], artifact / "missing"], payload)
        assert code == 2 and result == {"version": 9, "status": "invalid"}, (code, result)
    invalid = invalid_json()
    for payload in invalid:
        try:
            decode(payload)
        except (ValueError, AssertionError, RecursionError):
            pass
        else:
            raise AssertionError("accepted invalid concurrent failure input")
        if compare_beam:
            code, result = invoke(commands[1][0], payload)
            assert code == 2 and result == {"version": 9, "status": "invalid"}, (code, result)
    print(f"Concurrent failure Counter v9 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: "
          f"{len(valid)} cases, {positive} enumerated histories, {forged} forged histories, "
          f"{len(rejected)} bridge and {len(invalid)} JSON rejections; no performance claim")


def main():
    parser = argparse.ArgumentParser(description="Two real senders with failure-prefix and cleanup validation")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-concurrent-failure-") as directory:
            artifact = Path(directory) / "counter"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-failure-counter.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Concurrent failure Counter verification failed: {error}", file=sys.stderr)
        sys.exit(3)
