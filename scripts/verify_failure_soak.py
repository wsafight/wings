import argparse
import copy
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import traceback

import measure_failure_soak as soak
import verify_concurrent_failure as failure
import verify_scalar_aot as aot
from verify_open_loop import require


DIAGNOSTIC_BYTES = 8192


def failure_diagnostic(error):
    text = "".join(traceback.format_exception(type(error), error, error.__traceback__, limit=12))
    encoded = text.encode("utf-8", errors="replace")
    if len(encoded) > DIAGNOSTIC_BYTES:
        marker = b"\n... failure-soak diagnostic truncated ...\n"
        prefix = encoded[:2048]
        suffix = encoded[-(DIAGNOSTIC_BYTES - len(prefix) - len(marker)):]
        text = (prefix + marker + suffix).decode("utf-8", errors="ignore")
    return f"Failure soak verification failed:\n{text}"


def model(parent):
    config = failure.config(256, 2, 0, ([(1, 7, True), (2, 1, False)], [(1, 3, False), (2, 2, False)]))
    result = {"version": 9, "status": "failure", "actors": 2, "state_bytes": 256,
              "message_bytes": 32, "states_verified": True, "committed": [0, 3], "sequences": [[0, 0], [1, 1]],
              "outcomes": [{"sender": 1, "actor": 1, "seq": 1, "status": "failed", "value": None},
                           {"sender": 1, "actor": 2, "seq": 1, "status": "reply", "value": 1},
                           {"sender": 2, "actor": 1, "seq": 1, "status": "failed", "value": None},
                           {"sender": 2, "actor": 2, "seq": 1, "status": "reply", "value": 3}],
              "failures": [{"actor": 1, "reason": "injected_failure"}],
              "environment": {"runtime": "wings", "workers": 2, "senders": 2, "window_per_sender": 16}}
    start = {"version": 10, "event": "start", "mode": "verify", "epochs": 3,
             "warmup_seconds": 0, "sample_seconds": 0, "period_ns": 0,
             "round_deadline_ns": 3_000_000_000, "trace_limit_bytes": soak.TRACE_BYTES,
             "cold_bytes": 10000, "stack_slot_bytes": 4096, "max_cached_stacks": 4,
             "environment": result["environment"]}
    records = [{"version": 10, "epoch": index, "planned_ns": 0, "started_ns": index * 1000,
                "completed_ns": index * 1000 + 500, "stale_identities_checked": 2 if index == 1 else 4,
                "retained_bytes": 18192, "retained_processes": 0, "retained_messages": 0,
                "retained_requests": 0, "cached_stacks": 2, "result": copy.deepcopy(result)}
               for index in range(1, 4)]
    final = {"version": 10, "event": "complete", "status": "ok", "mode": "verify", "epochs": 3,
             "stale_identities_checked": 10, "measurement_elapsed_ns": 4000,
             "resources_released": True, "bootstrap_bytes": 232, "remaining_bytes": 232}
    path = parent / "model.jsonl"

    def invoke(rows=records, beginning=start, end=final, runtime="wings", long=False):
        path.write_bytes(b"".join(json.dumps(row).encode() + b"\n" for row in rows))
        end = {**end, "trace_bytes": path.stat().st_size}
        controls = [beginning, end]
        if long:
            controls.insert(1, {"version": 10, "event": "sampling", "elapsed_ns": 30_000_000_000})
        return soak.replay(path, config, controls, runtime, soak=long)

    require(invoke()["epochs"] == 3, "short model")
    changes = []
    for key, value in (("version", 10.0), ("epoch", 2), ("planned_ns", 1), ("started_ns", False),
                       ("completed_ns", 4_000_000_000), ("stale_identities_checked", 1),
                       ("retained_bytes", 18193), ("retained_processes", 1), ("retained_messages", 1),
                       ("retained_requests", 1), ("cached_stacks", 5)):
        rows = copy.deepcopy(records)
        rows[0][key] = value
        changes.append((rows, start, final))
    for key, value in (("committed", [7, 3]), ("sequences", [[1, 0], [1, 1]]), ("failures", []), ("outcomes", [])):
        rows = copy.deepcopy(records)
        rows[0]["result"][key] = value
        changes.append((rows, start, final))
    for key, value in (("version", True), ("epochs", 900), ("mode", "soak"), ("period_ns", 1),
                       ("round_deadline_ns", 4_000_000_000), ("max_cached_stacks", 5), ("cold_bytes", 0)):
        changes.append((records, {**start, key: value}, final))
    for key, value in (("status", "failure"), ("epochs", 2), ("measurement_elapsed_ns", 2999),
                       ("stale_identities_checked", 9), ("resources_released", False), ("remaining_bytes", 233)):
        changes.append((records, start, {**final, key: value}))
    changes += [(records[:-1], start, final), ([*records, records[-1]], start, final), (list(reversed(records)), start, final)]
    for rows, beginning, end in changes:
        try:
            invoke(rows, beginning, end)
        except (ValueError, AssertionError, KeyError, TypeError):
            continue
        raise AssertionError("accepted forged persistent failure history")
    long_start = {**start, "mode": "soak", "epochs": 900, "warmup_seconds": 30, "sample_seconds": 60, "period_ns": soak.PERIOD_NS}
    long_rows = []
    for index in range(1, 901):
        row = copy.deepcopy(records[0])
        planned = (index - 1) * soak.PERIOD_NS
        row.update({"epoch": index, "planned_ns": planned, "started_ns": planned + 1000,
                    "completed_ns": planned + 2000, "stale_identities_checked": 2 if index == 1 else 4})
        long_rows.append(row)
    long_final = {**final, "mode": "soak", "epochs": 900, "measurement_elapsed_ns": 90_000_000_000,
                  "stale_identities_checked": 3598}
    require(invoke(long_rows, long_start, long_final, long=True)["phases"]["sample"]["epochs"] == 600, "long model phases")
    for end in ({**long_final, "measurement_elapsed_ns": 89_999_999_999}, {**long_final, "epochs": 899}):
        try:
            invoke(long_rows, long_start, end, long=True)
        except (ValueError, AssertionError):
            continue
        raise AssertionError("accepted short long-mode measurement")
    beam_start = copy.deepcopy(start)
    beam_environment = {"runtime": "beam", "otp": "29", "schedulers": 2, "senders": 2, "window_per_sender": 16}
    beam_start.update({"cold_bytes": None, "stack_slot_bytes": None, "max_cached_stacks": None, "environment": beam_environment})
    beam_rows = copy.deepcopy(records)
    for row in beam_rows:
        for key in ("retained_bytes", "retained_processes", "retained_messages", "retained_requests", "cached_stacks"):
            row[key] = None
        row.update({"beam_process_count": 40, "beam_port_count": 3})
        row["result"]["environment"] = beam_environment
    beam_final = {key: value for key, value in final.items() if key not in ("resources_released", "bootstrap_bytes", "remaining_bytes")}
    beam_final["actors_and_senders_terminated"] = True
    invoke(beam_rows, beam_start, beam_final, "beam")
    path.write_bytes(b"{}")
    try:
        soak.replay(path, config, [start, {**final, "trace_bytes": 2}], "wings", soak=False)
    except (ValueError, AssertionError):
        pass
    else:
        raise AssertionError("accepted truncated trace")
    return len(changes) + 3


def verify(artifact, compare_beam, parent):
    forged = model(parent)
    commands = [("wings", [aot.TARGET / aot.PROFILE / "examples/beam_failure_soak", "--trusted", artifact])]
    if compare_beam:
        escript = shutil.which("escript")
        require(escript is not None, "OTP 29 required; no fallback")
        commands.append(("beam", [escript, aot.ROOT / "scripts/beam_failure_soak.escript"]))
    for length in (256, 4096, 65536):
        config = soak.workload(length)
        for runtime, command in commands:
            trace = parent / f"{runtime}-{length}.epochs.jsonl"
            payload = failure.bridge(config) if runtime == "wings" else json.dumps(config).encode()
            soak.trial([*command, "--verify-soak", trace], payload, trace, parent / f"{runtime}-{length}", config, runtime, soak=False)
    for runtime, command in commands:
        path = parent / f"{runtime}-existing.jsonl"
        sentinel = b"existing trace must survive\n"
        path.write_bytes(sentinel)
        config = soak.workload()
        payload = failure.bridge(config) if runtime == "wings" else json.dumps(config).encode()
        child = subprocess.run([str(item) for item in [*command, "--verify-soak", path]], input=payload,
                               capture_output=True, timeout=10, preexec_fn=soak.limits(False))
        require(child.returncode == 3 and path.read_bytes() == sentinel, "existing trace changed")
        require(len(child.stdout) <= 65536 and len(child.stderr) <= 65536, "exclusive trace diagnostics")
    invalid = [failure.config(), failure.config(streams=([(1, 1, True)], [])),
               failure.config(streams=([(1, 1, True)], [(1, 1, False)])),
               failure.config(actors=2, streams=([(1, 1, False)], [(2, 1, False)]))]
    for index, config in enumerate(invalid):
        for runtime, command in commands:
            path = parent / f"{runtime}-invalid-{index}.jsonl"
            payload = failure.bridge(config) if runtime == "wings" else json.dumps(config).encode()
            child = subprocess.run([str(item) for item in [*command, "--verify-soak", path]], input=payload,
                                   capture_output=True, timeout=10, preexec_fn=soak.limits(False))
            require(child.returncode == 2 and json.loads(child.stdout) == {"version": 10, "status": "invalid"}
                    and not child.stderr and not path.exists(), "invalid failure-soak workload accepted or touched trace")
    broken = parent / "failed-driver"
    try:
        soak.trial([sys.executable, "-c", 'import sys; print("partial", flush=True); print("retained diagnostic", file=sys.stderr); sys.exit(7)'],
                   b"", parent / "missing-trace.jsonl", broken, soak.workload(), "wings", soak=False)
    except (ValueError, AssertionError):
        manifest = json.loads((broken / "run.json").read_text())
        require(manifest["status"] == "invalid_experiment", "failed trial lost manifest")
        require((broken / "stdout.jsonl").read_bytes() == b"partial\n"
                and b"retained diagnostic" in (broken / "stderr.txt").read_bytes(), "failed trial lost raw evidence")
    else:
        raise AssertionError("accepted failed subprocess")
    print(f"Failure soak v10 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: three-state persistent reuse, 3 epochs each, {forged} forged histories, four workload rejections, exclusive trace and retained driver failure; no long run or complete fault/performance acceptance")


def main():
    parser = argparse.ArgumentParser(description="Bounded persistent-failure runner/model verification; not a long measurement")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="wings-failure-soak-") as directory:
        parent = Path(directory)
        artifact = arguments.artifact.resolve() if arguments.artifact else parent / "artifact"
        if not arguments.artifact:
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/beam-failure-counter.wings", "-o", artifact])
        verify(artifact, arguments.compare_beam, parent)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(failure_diagnostic(error), file=sys.stderr)
        sys.exit(3)
