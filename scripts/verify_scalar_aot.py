import json
import os
from pathlib import Path
import resource
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
COMPILER = ROOT / "target/debug/wings-compile"
VM = ROOT / "target/debug/wings-vm"


def limit_child():
    resource.setrlimit(resource.RLIMIT_CPU, (20, 20))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def command(arguments, text="", expected=0):
    result = subprocess.run(
        [str(argument) for argument in arguments],
        input=text,
        text=True,
        capture_output=True,
        timeout=30,
        cwd=ROOT,
        preexec_fn=limit_child,
    )
    if result.returncode != expected:
        raise AssertionError(
            f"{arguments}: expected exit {expected}, got {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
    return result


def invoke(directory, messages, expected=0):
    text = "".join(json.dumps(message) + "\n" for message in messages)
    return command([VM, "run", "--trusted", directory], text, expected)


def message(name, *arguments):
    return {"message": name, "args": list(arguments)}


def main():
    if not COMPILER.is_file() or not VM.is_file():
        raise SystemExit("Run cargo build --locked -p wings-compile -p wings-vm first")
    with tempfile.TemporaryDirectory(prefix="wings-aot-") as temporary:
        parent = Path(temporary)
        counter = parent / "counter"
        command([COMPILER, "build", ROOT / "examples/counter.wings", "-o", counter])
        messages = [message("Get"), message("Inc", 3), message("Inc", 4), message("Reset"), message("Set", 12), message("Get"), message("Confirm")]
        actual = [json.loads(line) for line in invoke(counter, messages).stdout.splitlines()]
        assert actual == [{"reply": 0}, {"reply": 3}, {"reply": 7}, {"accepted": True}, {"accepted": True}, {"reply": 12}, {"reply": None}]
        source_path = parent / "arithmetic.wings"
        source_path.write_text("""process Arithmetic {
  state count: Int = 0
  on Get -> Int { count }
  on Add(value: Int) -> Int { count = count + value; count }
  on Fail -> Int { count = 99; 1 / 0 }
  on Sum(left: Int, right: Int) -> Int { left + right }
  on Difference(left: Int, right: Int) -> Int { left - right }
  on Product(left: Int, right: Int) -> Int { left * right }
  on Divide(left: Int, right: Int) -> Int { left / right }
  on Negate(value: Int) -> Int { -value }
  on Previous(value: Int) -> Int {
    let previous = count
    count = value
    previous
  }
}
""")
        arithmetic = parent / "arithmetic"
        command([COMPILER, "build", source_path, "-o", arithmetic])
        minimum = -(2**63)
        maximum = 2**63 - 1
        result = invoke(arithmetic, [message("Sum", 6, 7), message("Difference", 6, 7), message("Product", -6, 7), message("Divide", -7, 2), message("Negate", -6), message("Add", 4), message("Previous", 8), message("Get")])
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [13, -1, -42, -3, 6, 4, 4, 8]
        failing = [
            message("Sum", maximum, 1),
            message("Difference", minimum, 1),
            message("Product", maximum, 2),
            message("Divide", 1, 0),
            message("Divide", minimum, -1),
            message("Negate", minimum),
            message("Fail"),
        ]
        for failure in failing:
            result = invoke(arithmetic, [message("Add", 7), failure, message("Get")], expected=1)
            assert result.stdout == '{"reply":7}\n'
            assert "managed" in result.stderr
        for invalid in [message("Missing"), message("Add"), message("Add", True), message("Add", 2**63), message("Add", 1, 2)]:
            assert not invoke(arithmetic, [invalid], expected=1).stdout
        assert "--trusted" in command([VM, "run", arithmetic], expected=2).stderr
        assert "64 KiB" in command([VM, "run", "--trusted", arithmetic], "x" * (64 * 1024 + 1), expected=1).stderr
        assert command([VM, "run", "--trusted", arithmetic], "not json\n", expected=1).stderr
        assert "new artifact directory" in command([COMPILER, "build", source_path, "-o", arithmetic], expected=1).stderr
        flag_source = parent / "flag.wings"
        flag_source.write_text("""process Flag {
  state enabled: Bool = false
  on Set(value: Bool) -> Bool { let previous = enabled; enabled = value; previous }
  on Get -> Bool { enabled }
  on Confirm(value: Unit) -> Unit { value }
  on Notify(value: Unit) { value }
}
""")
        flag = parent / "flag"
        command([COMPILER, "build", flag_source, "-o", flag])
        result = invoke(flag, [message("Set", True), message("Get"), message("Confirm", None), message("Notify", None)])
        assert [json.loads(line) for line in result.stdout.splitlines()] == [{"reply": False}, {"reply": True}, {"reply": None}, {"accepted": True}]
        assert not invoke(flag, [message("Set", 1)], expected=1).stdout
        manifest_path = counter / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        for field, value in [("abi", 999), ("format", 999), ("target", "wrong-target"), ("library", "/tmp/foreign.so")]:
            changed = dict(manifest)
            changed[field] = value
            manifest_path.write_text(json.dumps(changed))
            assert command([VM, "run", "--trusted", counter], expected=1).stderr
        changed = dict(manifest)
        changed["handlers"] = manifest["handlers"] + [manifest["handlers"][0]]
        manifest_path.write_text(json.dumps(changed))
        assert "duplicate" in command([VM, "run", "--trusted", counter], expected=1).stderr
        manifest_path.write_text(" " * (64 * 1024 + 1))
        assert "64 KiB" in command([VM, "run", "--trusted", counter], expected=1).stderr
        manifest_path.write_text(json.dumps(manifest))
        library = counter / ("program.dylib" if os.uname().sysname == "Darwin" else "program.so")
        saved_library = counter / "saved-library"
        library.rename(saved_library)
        library.symlink_to(saved_library)
        assert "regular file" in command([VM, "run", "--trusted", counter], expected=1).stderr
        failure_source = parent / "bad-initializer.wings"
        failure_source.write_text("process Broken { state count: Int = 1 / 0; on Get -> Int { count } }")
        failure_artifact = parent / "bad-initializer"
        command([COMPILER, "build", failure_source, "-o", failure_artifact])
        assert "division by zero" in invoke(failure_artifact, [], expected=1).stderr
        invalid_source = parent / "invalid.wings"
        invalid_source.write_text("process Invalid { state count: Int = true }")
        invalid_artifact = parent / "invalid"
        command([COMPILER, "build", invalid_source, "-o", invalid_artifact], expected=1)
        assert not invalid_artifact.exists()
        invalid_source.write_text("process First { state value: Unit = () } process Second { state value: Unit = () }")
        assert "exactly one process" in command([COMPILER, "build", invalid_source, "-o", invalid_artifact], expected=1).stderr
        print("Scalar AOT verification passed: state, replies, Bool/Unit, arithmetic boundaries, input and artifact rejection")


if __name__ == "__main__":
    main()
