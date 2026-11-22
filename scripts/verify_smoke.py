import argparse
import json
from pathlib import Path
import subprocess
import tempfile

import verify_scalar_aot as aot


def main():
    parser = argparse.ArgumentParser(description="Run bounded counter, session, TCP and HTTP AOT smoke checks")
    parser.add_argument("--target-dir", type=Path, default=aot.ROOT / "target")
    arguments = parser.parse_args()
    compiler = arguments.target_dir.resolve() / "debug" / "wings-compile"
    vm = compiler.with_name("wings-vm")
    runner = compiler.parent / "examples" / "mvp_runtime"
    for executable in (compiler, vm, runner):
        if not executable.is_file():
            raise ValueError(f"build the smoke binaries first: {executable}")
    with tempfile.TemporaryDirectory(prefix="wings-incremental-smoke-") as temporary:
        parent = Path(temporary)
        counter = parent / "counter"
        aot.command([compiler, "build", aot.ROOT / "examples/counter.wings", "-o", counter])
        result = aot.command([vm, "run", "--trusted", counter], (aot.ROOT / "examples/counter.messages.jsonl").read_text())
        expected = [{"reply": 0}, {"reply": 3}, {"reply": 7}, {"accepted": True},
                    {"accepted": True}, {"reply": 12}, {"reply": None}]
        if [json.loads(line) for line in result.stdout.splitlines()] != expected:
            raise ValueError(f"counter smoke replies differ: {result.stdout}")
        bundle = parent / "session"
        aot.command([compiler, "build-app", aot.ROOT / "examples/session-service.wings", "-o", bundle])
        print(aot.command([runner, "--trusted", bundle, vm]).stdout.strip())
    print("AOT smoke passed; full AOT and performance acceptance were not run")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error)) from error
