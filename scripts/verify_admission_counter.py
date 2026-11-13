import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import verify_scalar_aot as aot
from verify_multi_counter import invoke


def verify(artifact, compare_beam):
    wings = [aot.TARGET / aot.PROFILE / "examples/beam_admission_counter", "--trusted", artifact]
    escript = shutil.which("escript") if compare_beam else None
    if compare_beam and escript is None:
        raise RuntimeError("Erlang/OTP 29 escript is required")
    beam = [escript, aot.ROOT / "scripts/beam_admission_workload.escript"]
    if compare_beam:
        code, result = invoke([*beam, "--gate-tests"], b"")
        assert code == 0 and result == {"version": 4, "status": "ok", "gate_tests": 3, "senders": 2}, (code, result)
    for capacity in (1, 100, 256):
        expected = {"version": 4, "status": "ok", "state_bytes": 4096, "mailbox_capacity": capacity,
                    "accepted": capacity, "rejected_full": 1, "failed": capacity, "healthy_value": 7,
                    "initial_value": 0, "queue_released": True, "stale_rejected": True}
        for command, runtime in [(wings, "wings")] + ([(beam, "beam")] if compare_beam else []):
            code, result = invoke([*command, capacity], b"")
            assert code == 0, (code, result)
            for key, value in expected.items():
                assert type(result.get(key)) is type(value) and result[key] == value, (runtime, key, result)
            if runtime == "wings":
                assert result["resources_released"] is True and result["environment"]["workers"] == 2, result
            else:
                assert result["environment"]["otp"] == "29" and result["environment"]["schedulers"] == 2, result
    for capacity in ("0", "2", "257", "-1", "18446744073709551616", "false", "1.0"):
        for command in [[*wings[:2], artifact / "missing"]] + ([beam] if compare_beam else []):
            code, result = invoke([*command, capacity], b"")
            assert code == 2 and result == {"version": 4, "status": "invalid"}, (code, result)
    print(f"Admission Counter v4 {'Wings/BEAM/Python' if compare_beam else 'Wings/Python (BEAM not executed)'}: 1/100/256 bounded queues, full rejection, healthy progress, stop and cleanup; no open-loop/performance claim")


def main():
    parser = argparse.ArgumentParser(description="Bounded pre-send BEAM admission against Wings full queues")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--compare-beam", action="store_true")
    arguments = parser.parse_args()
    if arguments.artifact:
        verify(arguments.artifact.resolve(), arguments.compare_beam)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-admission-counter-") as directory:
            artifact = Path(directory) / "counter"
            aot.command([aot.COMPILER, "build", aot.ROOT / "examples/mailbox-boundary.wings", "-o", artifact])
            verify(artifact, arguments.compare_beam)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
        print(f"Admission Counter verification failed: {error}", file=sys.stderr)
        sys.exit(3)
