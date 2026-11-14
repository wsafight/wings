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
from verify_multi_counter import invoke, limit_child, MIN_I64, MAX_I64


def decode(payload):
    if len(payload) > 65536:
        raise ValueError("input_bytes")
    workload = json.loads(payload.decode("utf-8"), object_pairs_hook=strict_object)
    if type(workload) is not dict or set(workload) != {"version", "actors", "state_bytes", "initial", "streams"}:
        raise ValueError("fields")
    if type(workload["version"]) is not int or workload["version"] != 3:
        raise ValueError("version")
    actors = workload["actors"]
    if type(actors) is not int or not 1 <= actors <= 100:
        raise ValueError("actors")
    if type(workload["state_bytes"]) is not int or workload["state_bytes"] not in (256, 4096, 65536):
        raise ValueError("state_bytes")
    initial = workload["initial"]
    if type(initial) is not int or not MIN_I64 <= initial <= MAX_I64:
        raise ValueError("initial")
    streams = workload["streams"]
    if type(streams) is not list or len(streams) != 2:
        raise ValueError("streams")
    bounds = [[initial, initial] for _ in range(actors)]
    for stream in streams:
        if type(stream) is not list or len(stream) > 128:
            raise ValueError("stream")
        for request in stream:
            if type(request) is not dict or set(request) != {"actor", "delta"}:
                raise ValueError("request fields")
            actor, delta = request["actor"], request["delta"]
            if type(actor) is not int or not 1 <= actor <= actors or type(delta) is not int or not MIN_I64 <= delta <= MAX_I64:
                raise ValueError("request")
            bounds[actor - 1][0] += min(0, delta)
            bounds[actor - 1][1] += max(0, delta)
    if any(low < MIN_I64 or high > MAX_I64 for low, high in bounds):
        raise ValueError("unsafe_interleaving")
    return workload


def bridge(workload):
    payload = struct.pack(">QQqQQQ", 3, workload["state_bytes"], workload["initial"], workload["actors"], *(len(stream) for stream in workload["streams"]))
    for stream in workload["streams"]:
        sequences = {}
        for request in stream:
            actor = request["actor"]
            sequences[actor] = sequences.get(actor, 0) + 1
            payload += struct.pack(">QQqQ", actor, sequences[actor], request["delta"], 0)
    return payload


def check(workload, result):
    assert type(result) is dict and all(type(result[key]) is int for key in ("version", "actors", "state_bytes")), result
    assert result["version"] == 3 and result["status"] == "ok", result
    assert result["actors"] == workload["actors"] and result["state_bytes"] == workload["state_bytes"], result
    assert result["states_verified"] is True, result
    assert type(result["message_bytes"]) is int and result["message_bytes"] == 32, result
    expected = {}
    for sender, stream in enumerate(workload["streams"], 1):
        sequences = {}
        for request in stream:
            actor = request["actor"]
            sequences[actor] = sequences.get(actor, 0) + 1
            expected[sender, actor, sequences[actor]] = request["delta"]
    observed = {}
    assert type(result["replies"]) is list and len(result["replies"]) == len(expected), result
    for reply in result["replies"]:
        assert type(reply) is dict and set(reply) == {"sender", "actor", "seq", "value"}, reply
        assert all(type(value) is int for value in reply.values()), reply
        key = reply["sender"], reply["actor"], reply["seq"]
        assert key in expected and key not in observed and MIN_I64 <= reply["value"] <= MAX_I64, reply
        observed[key] = reply["value"]
    assert type(result["committed"]) is list and len(result["committed"]) == workload["actors"], result
    for actor, final in enumerate(result["committed"], 1):
        deltas = [[request["delta"] for request in stream if request["actor"] == actor] for stream in workload["streams"]]
        prefixes = [[0], [0]]
        for sender in range(2):
            for delta in deltas[sender]:
                prefixes[sender].append(prefixes[sender][-1] + delta)
        assert type(final) is int and final == workload["initial"] + prefixes[0][-1] + prefixes[1][-1], (actor, final)
        reachable = {(0, 0)}
        for first in range(len(prefixes[0])):
            for second in range(len(prefixes[1])):
                if (first, second) not in reachable:
                    continue
                value = workload["initial"] + prefixes[0][first] + prefixes[1][second]
                assert MIN_I64 <= value <= MAX_I64
                if first < len(deltas[0]) and observed[1, actor, first + 1] == value + deltas[0][first]:
                    reachable.add((first + 1, second))
                if second < len(deltas[1]) and observed[2, actor, second + 1] == value + deltas[1][second]:
                    reachable.add((first, second + 1))
        assert (len(deltas[0]), len(deltas[1])) in reachable, (actor, "no legal FIFO interleaving", result)


def config(length=256, actors=1, initial=0, streams=((), ())):
    return {"version": 3, "actors": actors, "state_bytes": length, "initial": initial,
            "streams": [[{"actor": actor, "delta": delta} for actor, delta in stream] for stream in streams]}


def cases():
    result = []
    for length in (256, 4096, 65536):
        result.extend([
            config(length),
            config(length, streams=([(1, index % 7 - 3) for index in range(128)], [(1, index % 5 - 2) for index in range(128)])),
            config(length, 100, -7, [[(1 + index % 100, (index + sender) % 7 - 3) for index in range(128)] for sender in range(2)]),
            config(length, 2, streams=([(1 + index % 2, index % 3 - 1) for index in range(33)], [])),
            config(length, 2, streams=([], [(1 + index % 2, index % 3 - 1) for index in range(33)])),
            config(length, initial=MAX_I64, streams=([(1, -1)] * 17, [(1, -2)] * 17)),
            config(length, initial=MIN_I64, streams=([(1, 1)] * 17, [(1, 2)] * 17)),
        ])
    return result


def model_tests():
    workload = config(streams=([(1, 1), (1, 2)], [(1, 4), (1, 8)]))
    replies = [{"sender": sender, "actor": 1, "seq": sequence, "value": value}
               for sender, sequence, value in [(1, 1, 1), (2, 1, 5), (1, 2, 7), (2, 2, 15)]]
    valid = {"version": 3, "status": "ok", "actors": 1, "state_bytes": 256, "message_bytes": 32, "states_verified": True, "committed": [15], "replies": replies}
    check(workload, valid)
    alternate = copy.deepcopy(valid)
    for reply, value in zip(alternate["replies"], [13, 4, 15, 12]):
        reply["value"] = value
    check(workload, alternate)
    rejected = []
    for index, value in [(0, 999), (1, 4), (2, 3)]:
        changed = copy.deepcopy(valid)
        changed["replies"][index]["value"] = value
        rejected.append(changed)
    changed = copy.deepcopy(valid)
    changed["replies"][0]["seq"], changed["replies"][2]["seq"] = 2, 1
    rejected.append(changed)
    changed = copy.deepcopy(valid)
    changed["committed"] = [14]
    rejected.append(changed)
    changed = copy.deepcopy(valid)
    changed["replies"][0] = changed["replies"][1]
    rejected.append(changed)
    changed = copy.deepcopy(valid)
    changed["replies"][0]["value"] = True
    rejected.append(changed)
    changed = copy.deepcopy(valid)
    changed["version"] = 3.0
    rejected.append(changed)
    for changed in rejected:
        try:
            check(workload, changed)
        except AssertionError:
            pass
        else:
            raise AssertionError("model accepted a forged history")
    return len(rejected)


def invalid_json():
    sample = config(streams=([(1, 7)], []))
    encoded = json.dumps(sample).encode()
    rejected = [b"", b"null", b"[]", b"true", b"[", b"\xff", encoded + b"{}", b" " * 65537,
                b"[" * 1500 + b"]" * 1500, encoded.replace(b'"version": 3', b'"version": 3, "version": 3'),
                encoded.replace(b'"actor": 1', b'"actor": 1, "actor": 1')]
    for key, value in [("version", 2), ("version", True), ("version", 3.0), ("actors", 0), ("actors", 101),
                       ("actors", True), ("actors", 1.0), ("state_bytes", 257), ("state_bytes", 256.0),
                       ("initial", MIN_I64 - 1), ("initial", MAX_I64 + 1), ("initial", False), ("initial", 0.0),
                       ("streams", []), ("streams", [[], [], []]), ("streams", [None, []]),
                       ("streams", [[{"actor": 1, "delta": 0}] * 129, []]), ("extra", 0)]:
        rejected.append(json.dumps({**sample, key: value}).encode())
    rejected.extend(json.dumps({key: value for key, value in sample.items() if key != missing}).encode() for missing in sample)
    for key, value in [("actor", 0), ("actor", 2), ("actor", True), ("actor", 1.0), ("delta", False),
                       ("delta", 0.0), ("delta", MAX_I64 + 1), ("delta", MIN_I64 - 1), ("extra", 0)]:
        rejected.append(json.dumps({**sample, "streams": [[{**sample["streams"][0][0], key: value}], []]}).encode())
    for missing in ("actor", "delta"):
        rejected.append(json.dumps({**sample, "streams": [[{key: value for key, value in sample["streams"][0][0].items() if key != missing}], []]}).encode())
    rejected.extend(json.dumps(workload).encode() for workload in [config(initial=MAX_I64, streams=([(1, 1)], [])), config(initial=MIN_I64, streams=([], [(1, -1)]))])
    return rejected


def reject_sequence(artifact, sender, sequence, after_success=False):
    messages = [aot.message("Configure", 256, 0, 1), aot.message("Apply", sender, sequence, 7, 0)]
    if after_success:
        messages.insert(1, aot.message("Apply", sender, 1, 7, 0))
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
    expected = [b'{"reply":0}', b'{"reply":7}'] if after_success else [b'{"reply":0}']
    assert child.returncode == 1 and stdout.splitlines() == expected, (child.returncode, stdout, stderr)
    assert len(stderr) <= 65536 and b"division by zero" in stderr, stderr


def verify(artifact, compare_beam):
    wings = [aot.TARGET / aot.PROFILE / "examples/beam_senders_counter", "--trusted", artifact]
    escript = shutil.which("escript") if compare_beam else None
    if compare_beam and escript is None:
        raise RuntimeError("Erlang/OTP 29 escript is required")
    beam = [escript, aot.ROOT / "scripts/beam_senders_workload.escript"]
    valid = cases()
    forged = model_tests()
    for sender, sequence in [(1, 0), (1, 2), (2, 0), (2, 2)]:
        reject_sequence(artifact, sender, sequence)
    for sender in (1, 2):
        reject_sequence(artifact, sender, 1, after_success=True)
    for workload in valid:
        payload = json.dumps(workload, separators=(",", ":")).encode()
        decode(payload)
        for command, data, runtime in [(wings, bridge(workload), "wings")] + ([(beam, payload, "beam")] if compare_beam else []):
            code, result = invoke(command, data)
            assert code == 0, (code, result, workload)
            check(workload, result)
            assert result["environment"]["senders"] == 2 and result["environment"]["window_per_sender"] == 16, result
            if runtime == "wings":
                receipts.validate(result)
                assert result["resources_released"] is True and result["environment"]["workers"] == 2, result
            else:
                assert result["environment"]["otp"] == "29" and result["environment"]["schedulers"] == 2, result
    sample = bridge(config(streams=([(1, 7)], [])))
    rejected = [sample[:length] for length in (0, 1, 47, 48, 79)] + [sample + b"\0", b"\0" * 8241]
    for index, value in [(0, 2), (1, 257), (3, 0), (3, 101), (4, 129), (6, 0), (6, 2), (7, 2), (9, 1), (2, MAX_I64)]:
        changed = bytearray(sample)
        struct.pack_into(">Q", changed, index * 8, value)
        rejected.append(bytes(changed))
    for payload in rejected:
        code, result = invoke([*wings[:2], artifact / "missing"], payload)
        assert code == 2 and result["status"] == "invalid", (code, result)
    invalid = invalid_json()
    for payload in invalid:
        try:
            decode(payload)
        except (ValueError, RecursionError):
            pass
        else:
            raise AssertionError(f"accepted invalid JSON: {payload[:200]!r}")
        if compare_beam:
            code, result = invoke(beam, payload)
            assert code == 2 and result["status"] == "invalid", (code, result, payload[:200])
    print(f"Two-sender Counter v3 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: {len(valid)} FIFO/interleaving cases, {forged} forged histories, {len(rejected)} bridge and {len(invalid)} JSON rejections; no performance claim")


def main():
    parser = argparse.ArgumentParser(description="Two real senders with independent FIFO and legal-interleaving validation")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-senders-counter-") as directory:
            artifact = Path(directory) / "counter"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-senders-counter.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Two-sender Counter verification failed: {error}", file=sys.stderr)
        sys.exit(3)
