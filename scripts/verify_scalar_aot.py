import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
PROFILE = os.environ.get("WINGS_PROFILE", "debug")
COMPILER = ROOT / f"target/{PROFILE}/wings-compile"
VM = ROOT / f"target/{PROFILE}/wings-vm"


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


def verify_control_abi(directory):
    import ctypes

    class Output(ctypes.Structure):
        _fields_ = [("state", ctypes.c_int64), ("reply", ctypes.c_int64)]

    class Context(ctypes.Structure):
        _fields_ = [
            ("cancellation", ctypes.POINTER(ctypes.c_uint32)),
            ("remaining_steps", ctypes.c_uint64),
            ("yield_request", ctypes.POINTER(ctypes.c_uint32)),
            ("slice_remaining", ctypes.c_uint64),
            ("slow_path", ctypes.c_void_p),
            ("data", ctypes.c_void_p),
        ]

    def control(budget, slice_steps=2**64 - 1, callback=None):
        return Context(ctypes.pointer(flag), budget, ctypes.pointer(request), slice_steps, callback, None)

    library_name = "program.dylib" if os.uname().sysname == "Darwin" else "program.so"
    library = ctypes.CDLL(str(directory / library_name))
    manifest = json.loads((directory / "manifest.json").read_text())
    assert manifest["abi"] == 3
    index = next(index for index, handler in enumerate(manifest["handlers"]) if handler["name"] == "Inc")
    entry = getattr(library, f"wings_scalar_handler_{index}")
    initializer = library.wings_scalar_init
    for function in [entry, initializer]:
        function.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
        function.restype = ctypes.c_uint32
    flag = ctypes.c_uint32(0)
    request = ctypes.c_uint32(0)
    arguments = (ctypes.c_int64 * 1)(3)
    for budget in range(4):
        output = Output(71, 91)
        context = control(budget)
        assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 4
        assert (output.state, output.reply, context.remaining_steps) == (71, 91, 0)
    for budget in [0, 4]:
        flag.value = 1
        output = Output(71, 91)
        context = control(budget)
        assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 3
        assert (output.state, output.reply, context.remaining_steps) == (71, 91, budget)
    output = Output(71, 91)
    context = control(3)
    assert initializer(0, arguments, ctypes.byref(output), ctypes.byref(context)) == 3
    assert (output.state, output.reply, context.remaining_steps) == (71, 91, 3)
    flag.value = 0
    for budget in [4, 2**64 - 1]:
        context = control(budget)
        assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 0
        assert (output.state, output.reply, context.remaining_steps) == (7, 7, budget - 4)
    for requested, slice_steps in [(0, 0), (1, 4)]:
        request.value = requested
        output = Output(71, 91)
        context = control(4, slice_steps)
        assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 5
        assert (output.state, output.reply, context.remaining_steps) == (71, 91, 4)
    request.value = 0
    observations = []
    callback_type = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.POINTER(Context))

    @callback_type
    def replenish(context_pointer):
        current = context_pointer.contents
        observations.append((current.remaining_steps, output.state, output.reply))
        current.slice_remaining = 1
        return 0

    output = Output(71, 91)
    context = control(4, 1, ctypes.cast(replenish, ctypes.c_void_p).value)
    assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 0
    assert observations == [(3, 71, 91), (2, 71, 91), (1, 71, 91)]
    assert (output.state, output.reply, context.remaining_steps) == (7, 7, 0)

    @callback_type
    def cancel_on_resume(context_pointer):
        flag.value = 1
        context_pointer.contents.slice_remaining = 1
        return 0

    output = Output(71, 91)
    context = control(4, 1, ctypes.cast(cancel_on_resume, ctypes.c_void_p).value)
    assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 3
    assert (output.state, output.reply, context.remaining_steps) == (71, 91, 3)
    flag.value = 0

    @callback_type
    def fail_slow_path(context_pointer):
        return 3

    context = control(4, 1, ctypes.cast(fail_slow_path, ctypes.c_void_p).value)
    assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 3
    assert (output.state, output.reply, context.remaining_steps) == (71, 91, 3)

    @callback_type
    def exhaust_on_resume(context_pointer):
        context_pointer.contents.remaining_steps = 0
        context_pointer.contents.slice_remaining = 1
        return 0

    context = control(4, 1, ctypes.cast(exhaust_on_resume, ctypes.c_void_p).value)
    assert entry(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 4
    assert (output.state, output.reply, context.remaining_steps) == (71, 91, 0)
    print("Scalar ABI v3 verified: polls, budgets, callback, cancellation, output commit")


def example(name, *arguments):
    options = ["--release"] if PROFILE == "release" else []
    return command(["cargo", "run", "--locked", *options, "-p", "wings-vm", "--example", name, "--", "--trusted", *arguments])


def main():
    if not COMPILER.is_file() or not VM.is_file():
        raise SystemExit("Build wings-compile and wings-vm for the selected WINGS_PROFILE first")
    with tempfile.TemporaryDirectory(prefix="wings-aot-") as temporary:
        parent = Path(temporary)
        counter = parent / "counter"
        command([COMPILER, "build", ROOT / "examples/counter.wings", "-o", counter])
        messages = [message("Get"), message("Inc", 3), message("Inc", 4), message("Reset"), message("Set", 12), message("Get"), message("Confirm")]
        actual = [json.loads(line) for line in invoke(counter, messages).stdout.splitlines()]
        assert actual == [{"reply": 0}, {"reply": 3}, {"reply": 7}, {"accepted": True}, {"accepted": True}, {"reply": 12}, {"reply": None}]
        controlled = [VM, "run", "--trusted", counter, "--max-steps"]
        for budget in range(4):
            result = command(controlled + [budget], json.dumps(message("Inc", 3)) + "\n", expected=1)
            assert not result.stdout and "step budget exhausted" in result.stderr
        result = command(controlled + [4], "".join(json.dumps(item) + "\n" for item in messages[:3]))
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [0, 3, 7]
        result = command(controlled + [2], json.dumps(message("Get")) + "\n" + json.dumps(message("Inc", 3)) + "\n" + json.dumps(message("Get")) + "\n", expected=1)
        assert result.stdout == '{"reply":0}\n' and "step budget exhausted" in result.stderr
        for invalid in ["-1", "1.5", "18446744073709551616", "not-a-number"]:
            assert "unsigned 64-bit" in command(controlled + [invalid], expected=2).stderr
        command([sys.executable, Path(__file__), "--check-control", counter])
        example("execution_control", counter)
        example("stack_pool", counter)
        example("accounted_stacks", counter)
        example("shared_module", counter)
        example("resource_accounts")
        example("record_heap")
        example("worker_wakeup")
        worker_source = parent / "worker.wings"
        worker_source.write_text("""process Queue {
  state count: Int = 0
  on Add(value: Int) { count = count + value }
  on Set(value: Int) { count = value }
  on Fail { count = 99; let failure = 1 / 0 }
  on Get -> Int { count }
  on Bump(value: Int) -> Int { count = count + value; count }
  on Ack -> Unit { () }
  on FailRequest -> Int { count = 99; 1 / 0 }
}
""")
        worker_artifact = parent / "worker"
        command([COMPILER, "build", worker_source, "-o", worker_artifact])
        example("scalar_worker", worker_artifact)
        example("scalar_requests", worker_artifact)
        example("scalar_supervisor", worker_artifact)
        timer_source = parent / "timer.wings"
        timer_bindings = "\n".join(f"let value_{index} = seed + {index}" for index in range(128))
        timer_groups = "\n".join(
            f"let group_{group} = " + " + ".join(f"value_{index}" for index in range(group * 64, (group + 1) * 64))
            for group in range(2)
        )
        timer_total = " + ".join(f"group_{group}" for group in range(2))
        timer_source.write_text(f"""process Timer {{
  state count: Int = 0
  on Work(seed: Int) -> Int {{
    {timer_bindings}
    {timer_groups}
    let total = {timer_total}
    count = count + total
    total
  }}
  on Get -> Int {{ count }}
  on Fail -> Int {{ count = 99; 1 / 0 }}
}}
""")
        timer_artifact = parent / "timer"
        command([COMPILER, "build", timer_source, "-o", timer_artifact])
        example("worker_timer", timer_artifact)
        command([ROOT / f"target/{PROFILE}/examples/frame_limits", parent / "frame-limit-rejected"])
        resume_source = parent / "resume.wings"
        bindings = "\n".join(f"let value_{index} = seed + {index}" for index in range(48))
        total = " + ".join(f"value_{index}" for index in range(48))
        resume_source.write_text(f"""process Resume {{
  state count: Int = 0
  on Work(seed: Int) -> Int {{
    {bindings}
    let total = {total}
    count = count + total
    total
  }}
  on Get -> Int {{ count }}
  on Fail -> Int {{ count = 99; 1 / 0 }}
}}
""")
        resume_artifact = parent / "resume"
        command([COMPILER, "build", resume_source, "-o", resume_artifact])
        pressure_source = parent / "pressure.wings"
        pressure_bindings = "\n".join(f"let value_{index} = seed + {index}" for index in range(128))
        pressure_total = " + ".join(f"value_{index}" for index in range(128))
        pressure_source.write_text(f"process Pressure {{ state count: Int = 0; on Work(seed: Int) -> Int {{ {pressure_bindings}; {pressure_total} }} }}")
        pressure_artifact = parent / "pressure"
        command([COMPILER, "build", pressure_source, "-o", pressure_artifact])
        assert json.loads(invoke(pressure_artifact, [message("Work", 2)]).stdout) == {"reply": 8384}
        example("stackful_resume", counter, resume_artifact)
        example("owned_tasks", counter, resume_artifact)
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
        for field, value in [("abi", 1), ("abi", 2), ("abi", 999), ("format", 999), ("target", "wrong-target"), ("library", "/tmp/foreign.so")]:
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
        print("Scalar AOT verification passed: scalar execution, live stacks, quota accounting, precise heap roots, worker mailboxes/requests/deadlines, bounded supervision, OS wakeup and artifact rejection")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--check-control":
        verify_control_abi(Path(sys.argv[2]))
    else:
        main()
