import argparse
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile

import measure_failure_soak as driver
import measure_recovery_soak as recovery
import verify_scalar_aot as aot
from verify_open_loop import require


def model(parent):
    used = {"bytes": 20000, "processes": 2, "messages": 17, "requests": 16}
    result = {"state_bytes": 4096, "queued": 16, "retained_request_slots": 16, "mailbox_full": True,
              "before_rejected": used, "after_rejected": used, "healthy_committed": 7, "stop_restarted": False,
              "outcomes": [{"sender": sender, "seq": sequence,
                            "status": "timed_out" if sequence <= 2 else "cancelled", "consumed_once": True}
                           for sender in (1, 2) for sequence in range(1, 9)],
              "failures": [{"attempt": attempt, "status": "cancelled", "handler": "fail",
                            "reason": "division_by_zero", "state_bytes": 4096, "committed": 0,
                            "restart": "restarted" if attempt == 1 else "intensity_exceeded",
                            "next_initial": 0 if attempt == 1 else None} for attempt in (1, 2)]}
    start = {"version": 11, "event": "start", "mode": "verify", "epochs": 3,
             "warmup_seconds": 0, "sample_seconds": 0, "period_ns": 0,
             "round_deadline_ns": 3_000_000_000, "trace_limit_bytes": driver.TRACE_BYTES,
             "cold_bytes": 10000, "stack_slot_bytes": 4096, "max_cached_stacks": 2,
             "environment": {"runtime": "wings", "workers": 1, "senders": 2,
                             "mailbox": 16, "result_slots": 17, "state_bytes": 4096}}
    rows = [{"version": 11, "epoch": index, "planned_ns": 0, "started_ns": index * 1000,
             "completed_ns": index * 1000 + 500, "stale_identities_checked": 4 if index == 1 else 8,
             "retained": {"bytes": 18192, "processes": 0, "messages": 0, "requests": 0},
             "cached_stacks": 2, "result": copy.deepcopy(result)} for index in range(1, 4)]
    final = {"version": 11, "event": "complete", "status": "ok", "mode": "verify", "epochs": 3,
             "measurement_elapsed_ns": 10000, "stale_identities_checked": 20,
             "resources_released": True, "bootstrap_bytes": 232, "remaining_bytes": 232}
    path = parent / "model.jsonl"

    def invoke(records, initial=start, complete=final, *, soak=False, workers=1):
        path.write_text("".join(json.dumps(record) + "\n" for record in records))
        events = [initial]
        if soak:
            events.append({"version": 11, "event": "sampling", "elapsed_ns": 30_000_000_000})
        events.append({**complete, "trace_bytes": path.stat().st_size})
        return recovery.replay(path, {"workers": workers}, events, "wings", soak=soak)

    invoke(rows)
    mixed = copy.deepcopy(rows)
    mixed[0]["result"]["outcomes"][-1]["status"] = "failed"
    mixed[1]["result"]["failures"][0]["status"] = "failed"
    invoke(mixed)
    two = {**start, "max_cached_stacks": 4, "environment": {**start["environment"], "workers": 2}}
    invoke(rows, two, workers=2)
    long_rows = [{**copy.deepcopy(rows[0]), "epoch": index, "planned_ns": (index - 1) * driver.PERIOD_NS,
                  "started_ns": (index - 1) * driver.PERIOD_NS + 1000,
                  "completed_ns": (index - 1) * driver.PERIOD_NS + 1500,
                  "stale_identities_checked": 4 if index == 1 else 8} for index in range(1, 901)]
    long_start = {**start, "mode": "soak", "epochs": 900, "warmup_seconds": 30,
                  "sample_seconds": 60, "period_ns": driver.PERIOD_NS}
    long_final = {**final, "mode": "soak", "epochs": 900, "measurement_elapsed_ns": 90_000_000_000,
                  "stale_identities_checked": 7196}
    invoke(long_rows, long_start, long_final, soak=True)
    changes = []

    def change(route, value):
        forged = copy.deepcopy(rows)
        target = forged[0]
        for key in route[:-1]:
            target = target[key]
        target[route[-1]] = value
        changes.append(forged)

    for key, value in [("version", True), ("epoch", 2), ("planned_ns", 1), ("started_ns", -1),
                       ("completed_ns", 3_000_001_001), ("stale_identities_checked", 0), ("cached_stacks", 3)]:
        change([key], value)
    for key, value in [("bytes", 18193), ("processes", 1), ("messages", 1), ("requests", 1)]:
        change(["retained", key], value)
    for key, value in [("state_bytes", 32), ("queued", 15), ("retained_request_slots", 8),
                       ("mailbox_full", 1), ("healthy_committed", 0), ("stop_restarted", True)]:
        change(["result", key], value)
    change(["result", "after_rejected"], {**used, "bytes": used["bytes"] + 1})
    for key in ("processes", "messages", "requests"):
        forged = copy.deepcopy(rows)
        for field in ("before_rejected", "after_rejected"):
            forged[0]["result"][field][key] -= 1
        changes.append(forged)
    change(["result", "outcomes"], result["outcomes"][:-1])
    change(["result", "outcomes"], result["outcomes"][:-1] + [result["outcomes"][0]])
    for key, value in [("sender", True), ("seq", 0), ("status", "reply"), ("consumed_once", False)]:
        change(["result", "outcomes", 0, key], value)
    change(["result", "outcomes", 2, "status"], "failed")
    change(["result", "failures"], result["failures"][:1])
    for index in (0, 1):
        for key, value in [("attempt", True), ("status", "timed_out"), ("handler", "add"),
                           ("reason", "cancelled"), ("state_bytes", 256), ("committed", 99),
                           ("restart", "disabled"), ("next_initial", 99)]:
            change(["result", "failures", index, key], value)
    changes.extend([rows[:-1], rows + [rows[-1]], [rows[1], rows[0], rows[2]]])
    rejected = 0
    for records in changes:
        try:
            invoke(records)
        except (ValueError, AssertionError):
            rejected += 1
        else:
            raise AssertionError("accepted forged recovery history")
    finals = [{**final, "remaining_bytes": 231}, {**final, "resources_released": False},
              {**final, "stale_identities_checked": 19}, {**final, "measurement_elapsed_ns": 1}]
    for complete in finals:
        try:
            invoke(rows, complete=complete)
        except (ValueError, AssertionError):
            rejected += 1
        else:
            raise AssertionError("accepted forged recovery final")
    for initial in [{**start, "version": True}, {**start, "max_cached_stacks": True},
                    {**start, "environment": {**start["environment"], "workers": True}}]:
        try:
            invoke(rows, initial)
        except (ValueError, AssertionError):
            rejected += 1
        else:
            raise AssertionError("accepted forged recovery start")
    try:
        invoke(long_rows, long_start, {**long_final, "measurement_elapsed_ns": 89_999_999_999}, soak=True)
    except (ValueError, AssertionError):
        rejected += 1
    else:
        raise AssertionError("accepted short recovery sample")
    path.write_bytes(b"{}")
    try:
        recovery.replay(path, {"workers": 1}, [start, {**final, "trace_bytes": 2}], "wings", soak=False)
    except (ValueError, AssertionError):
        rejected += 1
    else:
        raise AssertionError("accepted partial recovery line")
    return rejected


def beam_model(parent):
    result = {"state_bytes": 4096, "queued": 16, "retained_request_slots": 16, "mailbox_full": True,
              "before_rejected": {"queued": 16, "requests": 16}, "after_rejected": {"queued": 16, "requests": 16},
              "healthy_committed": 7, "stop_restarted": False,
              "outcomes": [{"sender": sender, "seq": sequence,
                            "status": "timed_out" if sequence <= 2 else "cancelled", "consumed_once": True}
                           for sender in (1, 2) for sequence in range(1, 9)],
              "failures": [{"attempt": attempt, "status": "failed", "handler": "fail", "reason": "division_by_zero",
                            "state_bytes": 4096, "committed": 0, "restart": "restarted" if attempt == 1 else "intensity_exceeded",
                            "next_initial": 0 if attempt == 1 else None} for attempt in (1, 2)]}
    start = {"version": 11, "event": "start", "mode": "verify", "epochs": 3,
             "warmup_seconds": 0, "sample_seconds": 0, "period_ns": 0, "round_deadline_ns": 3_000_000_000,
             "trace_limit_bytes": driver.TRACE_BYTES, "cold_bytes": None, "stack_slot_bytes": None,
             "max_cached_stacks": None, "environment": {"runtime": "beam", "otp": "29", "schedulers": 1,
             "senders": 2, "mailbox": 16, "result_slots": 17, "state_bytes": 4096}}
    rows = [{"version": 11, "epoch": index, "planned_ns": 0, "started_ns": index * 1000, "completed_ns": index * 1000 + 500,
             "stale_identities_checked": 4 if index == 1 else 8, "retained": None, "cached_stacks": None,
             "live_protocol_slots": 0, "beam_process_count": 40, "beam_port_count": 3, "result": copy.deepcopy(result)}
            for index in range(1, 4)]
    final = {"version": 11, "event": "complete", "status": "ok", "mode": "verify", "epochs": 3,
             "measurement_elapsed_ns": 10000, "stale_identities_checked": 20,
             "actors_and_senders_terminated": True, "protocol_slots_released": True}
    path = parent / "beam-model.jsonl"

    def invoke(records=rows, initial=start, complete=final, *, soak=False, workers=1):
        path.write_text("".join(json.dumps(row) + "\n" for row in records))
        events = [initial]
        if soak:
            events.append({"version": 11, "event": "sampling", "elapsed_ns": 30_000_000_000})
        events.append({**complete, "trace_bytes": path.stat().st_size})
        return recovery.replay(path, {"workers": workers}, events, "beam", soak=soak)

    invoke()
    invoke(initial={**start, "environment": {**start["environment"], "schedulers": 2}}, workers=2)
    long_rows = [{**copy.deepcopy(rows[0]), "epoch": index, "planned_ns": (index - 1) * driver.PERIOD_NS,
                  "started_ns": (index - 1) * driver.PERIOD_NS + 1000,
                  "completed_ns": (index - 1) * driver.PERIOD_NS + 1500,
                  "stale_identities_checked": 4 if index == 1 else 8} for index in range(1, 901)]
    long_start = {**start, "mode": "soak", "epochs": 900, "warmup_seconds": 30, "sample_seconds": 60,
                  "period_ns": driver.PERIOD_NS}
    long_final = {**final, "mode": "soak", "epochs": 900, "measurement_elapsed_ns": 90_000_000_000,
                  "stale_identities_checked": 7196}
    invoke(long_rows, long_start, long_final, soak=True)
    changes = []
    for route, value in [(["live_protocol_slots"], 1), (["live_protocol_slots"], False), (["retained"], {}),
                         (["cached_stacks"], 0), (["beam_process_count"], 1025), (["beam_port_count"], -1),
                         (["result", "before_rejected", "requests"], 8), (["result", "after_rejected", "queued"], 15),
                         (["result", "retained_request_slots"], 8), (["result", "failures", 0, "committed"], 99),
                         (["result", "failures", 1, "restart"], "restarted"),
                         (["result", "outcomes", 0, "consumed_once"], False), (["stale_identities_checked"], 0)]:
        forged = copy.deepcopy(rows)
        target = forged[0]
        for key in route[:-1]:
            target = target[key]
        target[route[-1]] = value
        changes.append((forged, start, final))
    changes += [(rows[:-1], start, final), (rows, {**start, "cold_bytes": 1}, final),
                (rows, {**start, "environment": {**start["environment"], "schedulers": True}}, final),
                (rows, start, {**final, "actors_and_senders_terminated": False}),
                (rows, start, {**final, "protocol_slots_released": False})]
    for records, initial, complete in changes:
        try:
            invoke(records, initial, complete)
        except (ValueError, AssertionError):
            continue
        raise AssertionError("accepted forged BEAM recovery")
    return len(changes)


def verify(artifact, parent, compare_beam=False):
    rejected = model(parent)
    rejected += beam_model(parent)
    runner = aot.TARGET / aot.PROFILE / "examples/concurrent_recovery"
    for workers in (1, 2):
        trace = parent / f"workers-{workers}.epochs.jsonl"
        command = [runner, "--trusted", artifact, "--verify-reuse", workers, trace]
        driver.trial(command, b"", trace, parent / f"workers-{workers}", {"workers": workers}, "wings", soak=False, replayer=recovery.replay)
        if compare_beam:
            trace = parent / f"beam-workers-{workers}.epochs.jsonl"
            command = recovery.beam_command(workers, aot.ROOT / "scripts/beam_recovery_soak.escript", "--verify-reuse", trace)
            driver.trial(command, b"", trace, parent / f"beam-workers-{workers}", {"workers": workers}, "beam", soak=False, replayer=recovery.replay)
    sentinel = b"existing recovery trace must survive\n"
    path = parent / "existing.jsonl"
    path.write_bytes(sentinel)
    child = subprocess.run([str(item) for item in [runner, "--trusted", artifact, "--verify-reuse", 1, path]],
                           capture_output=True, timeout=10, preexec_fn=driver.limits(False))
    require(child.returncode != 0 and not child.stdout and path.read_bytes() == sentinel, "recovery trace was overwritten")
    if compare_beam:
        child = subprocess.run([str(item) for item in recovery.beam_command(1, aot.ROOT / "scripts/beam_recovery_soak.escript", "--verify-reuse", path)],
                               capture_output=True, timeout=10, preexec_fn=driver.limits(False))
        require(child.returncode != 0 and not child.stdout and path.read_bytes() == sentinel, "BEAM recovery trace was overwritten")
        prefix = recovery.beam_command(1, aot.ROOT / "scripts/beam_recovery_soak.escript", "--verify-reuse", path)[:-3]
        for index, arguments in enumerate([[], ["--unknown", 1], ["--verify-reuse", 0],
                                            ["--verify-reuse", 3], ["--verify-reuse", 2]]):
            destination = parent / f"beam-invalid-{index}.jsonl"
            command = [*prefix, *arguments, destination]
            child = subprocess.run([str(item) for item in command], capture_output=True, timeout=10,
                                   preexec_fn=driver.limits(False))
            require(child.returncode != 0 and not child.stdout and not destination.exists(), "BEAM recovery invalid CLI created output")
    for index, (mode, workers) in enumerate([("--unknown", 1), ("--verify-reuse", 0), ("--verify-reuse", 3)]):
        trace = parent / f"invalid-{index}.jsonl"
        child = subprocess.run([str(item) for item in [runner, "--trusted", artifact, mode, workers, trace]],
                               capture_output=True, timeout=10, preexec_fn=driver.limits(False))
        require(child.returncode != 0 and not child.stdout and not trace.exists(), "invalid recovery arguments touched trace")
    runtimes = "Wings/BEAM/model" if compare_beam else "Wings/model"
    print(f"Recovery reuse {runtimes}: one/two workers, three persistent epochs each, {rejected} forged histories, exclusive trace; no long run or complete fault/performance acceptance")


def main():
    parser = argparse.ArgumentParser(description="Short persistent cancellation/recovery verification")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="wings-recovery-reuse-") as directory:
        parent = Path(directory)
        artifact = arguments.artifact.resolve() if arguments.artifact else parent / "artifact"
        if arguments.artifact is None:
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/concurrent-recovery.wings", "-o", artifact])
        verify(artifact, parent, arguments.compare_beam)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Recovery reuse verification failed: {error}", file=sys.stderr)
        sys.exit(3)
