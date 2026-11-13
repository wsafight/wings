import json
import os
from pathlib import Path
import resource
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
PROFILE = os.environ.get("WINGS_PROFILE", "debug")
TARGET = Path(os.environ.get("WINGS_TARGET_DIR", ROOT / "target"))
COMPILER = TARGET / PROFILE / "wings-compile"
VM = TARGET / PROFILE / "wings-vm"


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


def verify_tcp_service(directory):
    executable = TARGET / PROFILE / "examples/tcp_counter"

    def frame(sequence, value, operation=1, reserved=0):
        return struct.pack(">QQqQ", sequence, operation, value, reserved)

    def reply(connection):
        received = bytearray()
        while len(received) < 32:
            block = connection.recv(32 - len(received))
            assert block, "service closed before completing the reply"
            received.extend(block)
        return struct.unpack(">QQqQ", received)

    def require_closed(connection):
        try:
            assert connection.recv(1) == b""
        except ConnectionResetError:
            pass

    def scenario(connections, frames, exercise, expected):
        child = subprocess.Popen(
            [str(executable), "--trusted", str(directory), str(connections), str(frames)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            preexec_fn=limit_child, cwd=ROOT,
        )
        try:
            assert select.select([child.stdout], [], [], 5)[0], "TCP service did not start"
            ready = json.loads(child.stdout.readline())
            host, port = ready["listening"].split(":")
            assert host == "127.0.0.1"

            def connect():
                connection = socket.create_connection((host, int(port)), timeout=2)
                connection.settimeout(2)
                return connection

            exercise(connect)
            output, errors = child.communicate(timeout=5)
            assert child.returncode == 0 and not errors, (child.returncode, output, errors)
            report = json.loads(output)
            assert report["resources_released"] is True
            assert report["wake_errors"] == 0
            assert 0 <= report["max_socket_buffer"] <= 65536
            if report["connections"]:
                assert report["max_socket_buffer"] > 0
            for key, value in expected.items():
                assert report[key] == value, (key, report, value)
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)

    def normal(connect):
        with connect() as connection:
            payload = frame(1, 3)
            connection.sendall(payload[:3])
            connection.sendall(payload[3:])
            assert reply(connection) == (1, 3, 3, 0)
            connection.sendall(frame(2, 1) + frame(3, 5) + frame(4, -1))
            assert [reply(connection) for _ in range(3)] == [
                (2, 3, 4, 0), (3, 3, 9, 0), (4, 3, 8, 0),
            ]

    scenario(1, 4, normal, {"connections": 1, "published": 4, "replies": 4,
                          "committed": 8, "failed": 0, "rejected": 0, "io_errors": 0})

    def failing(connect, overflow):
        with connect() as connection:
            first = 2**63 - 1 if overflow else 7
            connection.sendall(frame(1, first) + frame(2, 1, 1 if overflow else 2) + frame(3, 99))
            assert reply(connection) == (1, 3, first, 0)
            assert reply(connection) == (2, 4, 3, 0)
            assert reply(connection) == (3, 4, 4, 0)

    for overflow in [False, True]:
        scenario(1, 3, lambda connect: failing(connect, overflow), {
            "published": 2, "replies": 1, "failed": 1, "rejected": 1,
            "committed": 2**63 - 1 if overflow else 7, "timed_out": 0,
        })

    def invalid(connect):
        for payload in [frame(0, 1), frame(1, 1, 9), frame(1, 1, reserved=1)]:
            with connect() as connection:
                connection.sendall(payload)
                require_closed(connection)
        with connect() as connection:
            connection.sendall(frame(9, 4))
            assert reply(connection) == (9, 3, 4, 0)

    scenario(4, 1, invalid, {"connections": 4, "invalid": 3, "published": 1,
                           "replies": 1, "committed": 4})

    def bounded_read(connect):
        with connect() as connection:
            connection.sendall(frame(1, 1)[:5])
        with connect() as connection:
            connection.sendall(frame(1, 1)[:8])
            time.sleep(0.15)
            require_closed(connection)
        with connect() as connection:
            connection.sendall(frame(3, 9))
            assert reply(connection) == (3, 3, 9, 0)

    scenario(3, 1, bounded_read, {"connections": 3, "io_errors": 1,
                                "published": 1, "committed": 9})

    def frame_limit(connect):
        with connect() as connection:
            connection.sendall(frame(1, 2))
            assert reply(connection) == (1, 3, 2, 0)
            require_closed(connection)

    scenario(1, 1, frame_limit, {"connections": 1, "published": 1, "committed": 2})
    def disconnected(connect):
        with connect() as connection:
            connection.sendall(frame(1, 6))
            connection.shutdown(socket.SHUT_WR)
            assert reply(connection) == (1, 3, 6, 0)
            require_closed(connection)

    scenario(1, 2, disconnected, {"connections": 1, "published": 1,
                                 "replies": 1, "committed": 6})
    scenario(1, 1, lambda connect: None, {"connections": 0, "published": 0, "committed": 0})
    for arguments in [(0, 1), (9, 1), (1, 0), (1, 65)]:
        command([executable, "--trusted", directory, *arguments], expected=1)


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
            ("stack_limit", ctypes.c_size_t),
            ("roots", ctypes.c_void_p),
            ("heap_call", ctypes.c_void_p),
            ("heap_data", ctypes.c_void_p),
            ("effect_call", ctypes.c_void_p),
        ]

    def control(budget, slice_steps=2**64 - 1, callback=None):
        return Context(ctypes.pointer(flag), budget, ctypes.pointer(request), slice_steps, callback, None, 1, None, None, None)

    library_name = "program.dylib" if os.uname().sysname == "Darwin" else "program.so"
    library = ctypes.CDLL(str(directory / library_name))
    manifest = json.loads((directory / "manifest.json").read_text())
    assert manifest["abi"] == 14
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
    for function in [initializer, entry]:
        for stack_limit in [0, 2**63, 2**64 - 1]:
            output = Output(71, 91)
            context = control(4, 0, ctypes.cast(replenish, ctypes.c_void_p).value)
            context.stack_limit = stack_limit
            before = list(observations)
            assert function(4, arguments, ctypes.byref(output), ctypes.byref(context)) == 6
            assert (output.state, output.reply, context.remaining_steps) == (71, 91, 4)
            assert observations == before
    print("Scalar ABI v14 verified: stack boundary, polls, budgets, callback, cancellation, output commit")


def cargo_example(package, name, *arguments):
    if os.environ.get("WINGS_PREBUILT_EXAMPLES") == "1":
        return command([TARGET / PROFILE / "examples" / name, *arguments])
    options = ["--release"] if PROFILE == "release" else []
    return command(["cargo", "run", "--locked", *options, "-p", package, "--example", name, "--", *arguments])


def example(name, *arguments):
    return cargo_example("wings-vm", name, "--trusted", *arguments)


def verify_persistent_roots(directory, branches=False, loops=False, record_branches=False, record_loops=False, functions=False, lists=False, self_send=False, local_processes=False, requests=False, record_messages=False, list_messages=False, byte_buffers=False):
    import ctypes

    class Output(ctypes.Structure):
        _fields_ = [("state", ctypes.c_int64), ("reply", ctypes.c_int64)]

    class Frame(ctypes.Structure):
        pass

    Frame._fields_ = [("previous", ctypes.POINTER(Frame)), ("slots", ctypes.POINTER(ctypes.c_uint64)), ("count", ctypes.c_size_t)]

    class Context(ctypes.Structure):
        _fields_ = [
            ("cancellation", ctypes.POINTER(ctypes.c_uint32)), ("remaining_steps", ctypes.c_uint64),
            ("yield_request", ctypes.POINTER(ctypes.c_uint32)), ("slice_remaining", ctypes.c_uint64),
            ("slow_path", ctypes.c_void_p), ("data", ctypes.c_void_p), ("stack_limit", ctypes.c_size_t),
            ("roots", ctypes.POINTER(Frame)), ("heap_call", ctypes.c_void_p), ("heap_data", ctypes.c_void_p),
            ("effect_call", ctypes.c_void_p),
        ]

    records = {}
    committed = None
    sequence = 0
    errors = []
    observations = []
    frame_depths = []
    cancel_depth = None

    def collect(current):
        roots = set()
        frame = current.roots
        depth = 0
        count = 0
        while frame:
            depth += 1
            count += frame.contents.count
            assert depth <= 64 and count <= 64
            roots.update(frame.contents.slots[index] for index in range(frame.contents.count))
            frame = frame.contents.previous
        frame_depths.append(depth)
        assert roots <= records.keys(), (roots, records)
        retained = set(roots)
        if committed is not None:
            retained.add(committed)
        pending = list(retained)
        while pending:
            reference = pending.pop()
            value = records[reference]
            if isinstance(value, tuple) and value[2] not in retained:
                retained.add(value[2])
                pending.append(value[2])
        for reference in list(records):
            if reference not in retained:
                del records[reference]
        return roots

    heap_type = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.POINTER(Context), ctypes.c_uint32, ctypes.c_uint64, ctypes.POINTER(ctypes.c_uint64))

    @heap_type
    def heap_call(pointer, operation, argument, result):
        nonlocal sequence
        try:
            collect(pointer.contents)
            if operation == 0:
                sequence += 1
                records[sequence] = argument
                result[0] = sequence
            elif operation == 1:
                result[0] = records[argument]
            elif operation == 3:
                sequence += 1
                records[sequence] = None
                result[0] = sequence
            elif operation == 4:
                tail = result[0]
                assert tail in records
                sequence += 1
                records[sequence] = ("cons", argument, tail)
                result[0] = sequence
            elif operation in [5, 6, 7]:
                node = records[argument]
                if operation == 7:
                    result[0] = int(node is None)
                elif node is None:
                    return 9
                else:
                    result[0] = node[1 if operation == 5 else 2]
            elif operation in [8, 9, 10, 11, 12, 13]:
                values = list(ctypes.cast(argument, ctypes.POINTER(ctypes.c_int64))[:3])
                if operation == 8:
                    length, value = values[:2]
                    if not 0 <= length <= 65536 or not 0 <= value <= 255:
                        return 13
                    data = bytes([value]) * length
                else:
                    data = records[values[0]]
                    assert isinstance(data, bytes)
                    if operation == 9:
                        result[0] = len(data)
                        return 0
                    index = values[1]
                    width = 8 if operation in [12, 13] else 1
                    if not 0 <= index <= len(data) - width:
                        return 13
                    if operation == 10:
                        result[0] = data[index]
                        return 0
                    if operation == 12:
                        result[0] = int.from_bytes(data[index:index + 8], "big", signed=True) % (1 << 64)
                        return 0
                    if operation == 11 and not 0 <= values[2] <= 255:
                        return 13
                    replacement = values[2].to_bytes(8, "big", signed=True) if operation == 13 else bytes([values[2]])
                    data = data[:index] + replacement + data[index + width:]
                sequence += 1
                records[sequence] = data
                result[0] = sequence
            else:
                assert operation == 2
                result[0] = 0
            return 0
        except Exception as error:
            errors.append(repr(error))
            return 8

    slow_type = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.POINTER(Context))

    @slow_type
    def slow_path(pointer):
        try:
            observations.append((pointer.contents.remaining_steps, collect(pointer.contents)))
            if cancel_depth is not None and frame_depths[-1] >= cancel_depth:
                flag.value = 1
            pointer.contents.slice_remaining = 1
            return 0
        except Exception as error:
            errors.append(repr(error))
            return 8

    effects = []
    effect_status = 0
    reject_operation = None
    class EffectRequest(ctypes.Structure):
        _fields_ = [("operation", ctypes.c_uint32), ("handler", ctypes.c_uint32), ("target", ctypes.c_int64), ("arguments", ctypes.POINTER(ctypes.c_int64)), ("count", ctypes.c_size_t), ("callback", ctypes.c_uint32), ("timeout_ms", ctypes.c_int64), ("record_mask", ctypes.c_uint64), ("list_mask", ctypes.c_uint64)]

    effect_type = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.POINTER(Context), ctypes.POINTER(EffectRequest), ctypes.POINTER(ctypes.c_int64))

    @effect_type
    def effect_call(pointer, request, output):
        try:
            roots = collect(pointer.contents)
            current = request.contents
            if record_messages:
                assert current.operation in [0, 2, 3, 4]
                values = list(current.arguments[:current.count])
                if current.operation == 2:
                    assert current.count == current.record_mask == current.list_mask == 0
                    effects.append((current.operation, None, 0, 0, []) if list_messages else (current.operation, None, 0, []))
                else:
                    schema = manifest["handlers"][current.handler]
                    expected_mask = sum(1 << index for index, parameter in enumerate(schema["parameters"]) if parameter[1] == "Ref")
                    expected_lists = sum(1 << index for index, parameter in enumerate(schema["parameters"]) if parameter[1] == "ListRef")
                    assert current.record_mask == expected_mask
                    assert current.list_mask == expected_lists
                    for index, value in enumerate(values):
                        if expected_mask & (1 << index):
                            assert value in roots
                            values[index] = records[value]
                        elif expected_lists & (1 << index):
                            assert value in roots
                            copied = []
                            while records[value] is not None:
                                node = records[value]
                                copied.append(node[1] if node[1] < 1 << 63 else node[1] - (1 << 64))
                                value = node[2]
                                assert len(copied) <= 64
                            values[index] = copied
                    effects.append((current.operation, schema["name"], current.record_mask, current.list_mask, values) if list_messages else (current.operation, schema["name"], current.record_mask, values))
                pointer.contents.slice_remaining = 1
                output[0] = 42 if current.operation == 2 else (43 if current.operation == 4 else 0)
                return 0
            assert len(roots) == 2
            if requests:
                assert current.operation in [2, 4, 5]
                assert current.target == (42 if current.operation == 4 else (43 if current.operation == 5 else 0))
                if current.operation == 4:
                    assert current.timeout_ms == 60000
                    assert manifest["handlers"][current.callback]["name"] == "Completed"
                effects.append((current.operation, list(current.arguments[:current.count])))
            elif local_processes:
                assert current.operation in [2, 3]
                assert current.target == (42 if current.operation == 3 else 0)
                effects.append((current.operation, list(current.arguments[:current.count])))
            else:
                assert current.operation == 0 and current.target == 0
                effects.append((current.handler, list(current.arguments[:current.count])))
            pointer.contents.slice_remaining = 1
            output[0] = 42 if current.operation == 2 else (43 if current.operation == 4 else 0)
            return effect_status if reject_operation is None or current.operation == reject_operation else 0
        except Exception as error:
            errors.append(repr(error))
            return 8

    library_name = "program.dylib" if os.uname().sysname == "Darwin" else "program.so"
    library = ctypes.CDLL(str(directory / library_name))
    manifest = json.loads((directory / "manifest.json").read_text())
    assert manifest["abi"] == 14 and manifest["state"] == ("BytesRef" if byte_buffers else ("ListRef" if lists or list_messages else ("Int" if branches or loops else "Ref")))
    if self_send or local_processes or requests or record_messages:
        name = "Start"
    elif byte_buffers:
        name = "Fill"
    elif lists:
        name = "Build"
    elif record_loops or functions:
        name = "Count"
    elif record_branches:
        name = "Nested"
    elif loops == "nested":
        name = "NestedHeap"
    elif loops == "branch":
        name = "BranchHeap"
    else:
        name = "Heap" if branches or loops else "Replace"
    index = next(index for index, handler in enumerate(manifest["handlers"]) if handler["name"] == name)
    flag = ctypes.c_uint32(0)
    arguments = (ctypes.c_int64 * 1)(19)
    handler = getattr(library, f"wings_scalar_handler_{index}")
    if record_branches or record_loops or functions or lists or self_send or local_processes or requests or record_messages or byte_buffers:
        def execute(entry, values):
            arguments = (ctypes.c_int64 * max(1, len(values)))(*values)
            entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
            entry.restype = ctypes.c_uint32
            context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
            if self_send or local_processes or requests or record_messages:
                context.effect_call = ctypes.cast(effect_call, ctypes.c_void_p).value
            output = Output(71, 91)
            observations.clear()
            state = committed or 0
            status = entry(state, arguments, ctypes.byref(output), ctypes.byref(context))
            assert status == 0 and not errors and not context.roots, (status, errors)
            assert output.state in observations[-1][1], (output.state, observations)
            return output

        output = execute(library.wings_scalar_init, [])
        if byte_buffers:
            assert records[output.state] == b""
            committed = output.state
            for name, values, expected_state, expected_reply in [
                ("Fill", [65536, 3], bytes([3]) * 65536, 65536),
                ("Set", [65535, 255], bytes([3]) * 65535 + bytes([255]), 1023),
                ("Branch", [0], bytes([3]) * 65535 + bytes([255]), 6),
                ("Branch", [1], bytes([3]) * 65535 + bytes([255]), 10),
                ("Read", [65535], bytes([3]) * 65535 + bytes([255]), 255),
                ("Loop", [7], bytes([7]) + bytes(511), 7),
                ("Length", [], bytes([7]) + bytes(511), 512),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert (records[output.state], output.reply) == (expected_state, expected_reply), (name, output.reply)
                committed = output.state
            assert max(frame_depths) >= 2, frame_depths
            previous = 0
            for number in [-(1 << 63), -1, 0, 1, (1 << 63) - 1, 0]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "SetWord")
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), [504, number])
                assert output.reply == previous
                committed = output.state
                assert records[committed][504:] == number.to_bytes(8, "big", signed=True)
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "ReadWord")
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), [504])
                assert output.reply == number
                previous = number
            for name, values, expected_status in [
                ("Fill", [-1, 0], 13), ("Fill", [65537, 0], 13),
                ("Fill", [1, 256], 13), ("Set", [512, 0], 13), ("Fail", [], 2),
                ("ReadWord", [505], 13), ("SetWord", [-1, 0], 13), ("FailWord", [], 2),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                entry = getattr(library, f"wings_scalar_handler_{index}")
                entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
                entry.restype = ctypes.c_uint32
                context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
                rejected = Output(71, 91)
                arguments = (ctypes.c_int64 * max(1, len(values)))(*values)
                assert entry(committed, arguments, ctypes.byref(rejected), ctypes.byref(context)) == expected_status, (name, errors)
                assert (rejected.state, rejected.reply) == (71, 91)
                assert records[committed] == bytes([7]) + bytes(511)
                assert not errors and not context.roots, errors
            print("Compiler roots verified: byte source liveness, fn/branch/loop roots, immutable state and failed candidate unlinking")
            return
        if list_messages:
            committed = output.state
            for name, values, expected_reply in [
                ("Start", [23, 0], 23), ("Self", [29], 29),
                ("Mixed", [31], 31), ("Ask", [37], -1), ("Empty", [], 0),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert output.reply == expected_reply and records[output.state] is None
            assert effects == [
                (2, None, 0, 0, []), (3, "Store", 0, 1, [[23, 24]]),
                (0, "Store", 0, 1, [[29, 30]]),
                (2, None, 0, 0, []), (3, "StoreMixed", 4, 25, [[31, 32], 1, 33, [32], []]),
                (2, None, 0, 0, []), (4, "Compute", 0, 1, [[37, 38]]),
                (0, "Store", 0, 1, [[]]),
            ], effects
            print("Compiler roots verified: list source graphs and mixed record/list last-use arguments at every effect")
            return
        if record_messages:
            committed = output.state
            for name, values, expected_reply in [
                ("Start", [23, 0], 23), ("Self", [29], 29),
                ("Pair", [31], 31), ("Ask", [37], 0),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert output.reply == expected_reply and records[output.state] == 0
            assert effects == [
                (2, None, 0, []), (3, "Store", 1, [23]), (0, "Store", 1, [29]),
                (2, None, 0, []), (3, "StorePair", 5, [31, 1, 32]),
                (2, None, 0, []), (4, "Compute", 1, [37]),
            ], effects
            print("Compiler roots verified: record message copies, mixed masks and last-use references at every effect")
            return
        if requests:
            committed = output.state
            output = execute(handler, [4, 0])
            assert records[output.state] == 9 and output.reply == 9
            assert effects == [(2, []), (4, [4]), (5, [])], effects
            for reject_operation, effect_status in [(4, 12), (5, 12)]:
                context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None, ctypes.cast(effect_call, ctypes.c_void_p).value)
                rejected = Output(71, 91)
                values = (ctypes.c_int64 * 2)(4, 0)
                assert handler(committed, values, ctypes.byref(rejected), ctypes.byref(context)) == effect_status, errors
                assert (rejected.state, rejected.reply) == (71, 91)
                assert not errors and not context.roots
            print("Compiler roots verified: request/cancel, result capabilities, candidate roots and failure unlinking")
            return
        if local_processes:
            committed = output.state
            output = execute(handler, [4, 0])
            assert records[output.state] == 9 and output.reply == 9
            assert effects == [(2, []), (3, [4])], effects
            for reject_operation, effect_status in [(2, 11), (3, 10)]:
                context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None, ctypes.cast(effect_call, ctypes.c_void_p).value)
                rejected = Output(71, 91)
                values = (ctypes.c_int64 * 2)(4, 0)
                assert handler(committed, values, ctypes.byref(rejected), ctypes.byref(context)) == effect_status, errors
                assert (rejected.state, rejected.reply) == (71, 91)
                assert not errors and not context.roots
            print("Compiler roots verified: spawn/send result, candidate, outer record and rejection unlinking")
            return
        if self_send:
            committed = output.state
            output = execute(handler, [])
            assert records[output.state] == 9 and output.reply == 9
            add = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "Add")
            assert effects == [(add, [3])], effects
            for effect_status in [3, 10]:
                context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None, ctypes.cast(effect_call, ctypes.c_void_p).value)
                rejected = Output(71, 91)
                assert handler(committed, arguments, ctypes.byref(rejected), ctypes.byref(context)) == effect_status, errors
                assert (rejected.state, rejected.reply) == (71, 91)
                assert not errors and not context.roots
            assert effects == [(add, [3])] * 3
            print("Compiler roots verified: self-send callback, candidate and outer live record")
            return
        if lists:
            assert records[output.state] is None
            committed = output.state
            for name, values, expected_reply in [
                ("Empty", [], 1), ("Push", [7], 7), ("Push", [11], 11),
                ("Shared", [1], 18), ("Shared", [0], 106), ("Pop", [], 11), ("Pop", [], 7),
                ("Empty", [], 1), ("Build", [7], 7), ("Drain", [], 0), ("Empty", [], 1),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert output.reply == expected_reply, (name, output.reply)
                committed = output.state
            print("Compiler roots verified: transitive list tails, merges, loops and inline calls")
            return
        assert records[output.state] == (0 if record_loops or functions else 7)
        committed = output.state
        if functions:
            for name, values, expected_state, expected_reply in [
                ("Count", [7], 7, 7), ("Choose", [0], 7, 7),
                ("Choose", [1], 8, 8), ("Order", [], 8, 12), ("Once", [], 8, 38),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert records[output.state] == expected_state and output.reply == expected_reply, (name, records, output.reply)
                committed = output.state
            assert max(frame_depths) >= 3, frame_depths
            sentinel_slots = (ctypes.c_uint64 * 1)(committed)
            sentinel = Frame(None, sentinel_slots, 1)
            cancel_depth = 3
            index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "Choose")
            entry = getattr(library, f"wings_scalar_handler_{index}")
            context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
            context.roots = ctypes.pointer(sentinel)
            rejected = Output(71, 91)
            assert entry(committed, (ctypes.c_int64 * 1)(1), ctypes.byref(rejected), ctypes.byref(context)) == 3, errors
            assert not errors and ctypes.addressof(context.roots.contents) == ctypes.addressof(sentinel) and flag.value == 1
            assert (rejected.state, rejected.reply) == (71, 91)
            cancel_depth = None
            flag.value = 0
            for name, values, expected_status in [("Fail", [], 2), ("FirstFailure", [], 2), ("SecondFailure", [], 1)]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                entry = getattr(library, f"wings_scalar_handler_{index}")
                entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
                entry.restype = ctypes.c_uint32
                arguments = (ctypes.c_int64 * max(1, len(values)))(*values)
                previous = records[committed]
                for budget in [0, 1, 4, 16, 128]:
                    context = Context(ctypes.pointer(flag), budget, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
                    context.roots = ctypes.pointer(sentinel)
                    rejected = Output(71, 91)
                    status = entry(committed, arguments, ctypes.byref(rejected), ctypes.byref(context))
                    assert status in [expected_status, 4] and not errors and ctypes.addressof(context.roots.contents) == ctypes.addressof(sentinel), (name, status, errors)
                    assert (rejected.state, rejected.reply) == (71, 91) and records[committed] == previous
            print("Compiler roots verified: nested managed frames, arguments, returns, collections and failure unlinking")
            return
        if record_loops:
            for name, values, expected_state, expected_reply in [
                ("Count", [0], 0, 0), ("Count", [5], 5, 5), ("Count", [3], 5, 5),
                ("Preserve", [0, 10], 10, 10), ("Nested", [15], 17, 27),
                ("Advance", [12, 8], 12, 12), ("Advance", [5, 9], 9, 9),
                ("Retain", [15], 15, 15),
            ]:
                index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
                output = execute(getattr(library, f"wings_scalar_handler_{index}"), values)
                assert records[output.state] == expected_state and output.reply == expected_reply, (name, records, output.reply)
                committed = output.state
            index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "Fail")
            entry = getattr(library, f"wings_scalar_handler_{index}")
            entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
            entry.restype = ctypes.c_uint32
            previous = records[committed]
            arguments[0] = previous + 3
            for budget in [0, 1, 8, 24, 48, 256]:
                context = Context(ctypes.pointer(flag), budget, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
                output = Output(71, 91)
                status = entry(committed, arguments, ctypes.byref(output), ctypes.byref(context))
                assert status in [2, 4] and not errors and not context.roots, (status, errors)
                assert (output.state, output.reply) == (71, 91) and records[committed] == previous
            print("Compiler roots verified: record headers, backedges, zero exits, fresh initial candidates and nested aliases")
            return
        for first, second in [(1, 1), (1, 0), (0, 1), (0, 0), (1, 1)]:
            previous = records[committed]
            output = execute(handler, [first, second, 19])
            expected = 19 if first and second else (20 if not first and not second else previous)
            assert output.reply == expected and records[output.state] == expected
            committed = output.state
        for name in ["Local", "Discard", "Replace"]:
            index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == name)
            entry = getattr(library, f"wings_scalar_handler_{index}")
            for chosen in [1, 0]:
                previous = records[committed]
                output = execute(entry, [chosen] if name == "Discard" else [chosen, 23])
                expected = previous if name == "Discard" else ((23 if chosen else 24) + previous if name == "Local" else (23 if chosen else previous))
                assert output.reply == expected, (name, chosen, output.reply, expected)
                committed = output.state
        index = next(index for index, schema in enumerate(manifest["handlers"]) if schema["name"] == "Fail")
        entry = getattr(library, f"wings_scalar_handler_{index}")
        entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
        entry.restype = ctypes.c_uint32
        previous = records[committed]
        for budget in range(32):
            for chosen in [0, 1]:
                arguments[0] = chosen
                context = Context(ctypes.pointer(flag), budget, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
                output = Output(71, 91)
                status = entry(committed, arguments, ctypes.byref(output), ctypes.byref(context))
                assert status in [2, 4] and not errors and not context.roots, (status, errors)
                assert (output.state, output.reply) == (71, 91)
                assert records[committed] == previous
        print("Compiler roots verified: reference merges, both paths, nested candidates, aliases and dead results")
        return
    if loops:
        limit, expected = (7, 9) if loops == "nested" else (30, 30)
        cases = [(handler, 0, limit, expected), (handler, 0, 0, 0)]
    elif branches:
        cases = [(handler, 7, 1, 18), (handler, 7, 0, 29)]
    else:
        cases = [(library.wings_scalar_init, 0, 19, 7), (handler, 1, 19, 19)]
    for entry, state, argument, expected in cases:
        arguments[0] = argument
        entry.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(Output), ctypes.POINTER(Context)]
        entry.restype = ctypes.c_uint32
        context = Context(ctypes.pointer(flag), 4096, ctypes.pointer(flag), 1, ctypes.cast(slow_path, ctypes.c_void_p).value, None, 1, None, ctypes.cast(heap_call, ctypes.c_void_p).value, None)
        output = Output(71, 91)
        observations.clear()
        assert entry(state, arguments, ctypes.byref(output), ctypes.byref(context)) == 0, errors
        assert not errors and not context.roots
        if branches or loops:
            assert output.state == (expected if loops else 7) and output.reply == expected, (output.state, output.reply, expected)
            assert not records and not observations[-1][1], (records, observations)
        else:
            assert records[output.state] == expected
            assert observations[-1][1] == {output.state}, observations
            committed = output.state
    if loops:
        print("Compiler roots verified: loop backedge GC")
    elif branches:
        print("Compiler roots verified: chosen branch GC")
    else:
        print("Persistent compiler roots verified: initializer, candidate-only output and collection at final checkpoint")


def main():
    if not COMPILER.is_file() or not VM.is_file():
        raise SystemExit("Build wings-compile and wings-vm for the selected WINGS_PROFILE first")
    with tempfile.TemporaryDirectory(prefix="wings-aot-") as temporary:
        parent = Path(temporary)
        counter = parent / "counter"
        command([COMPILER, "build", ROOT / "examples/counter.wings", "-o", counter])
        for count in (16, 64, 256):
            density = json.loads(example("scheduler_density", counter, count).stdout)
            assert density["actors"] == count and density["workers"] == 2
            assert density["requests"] == 64
            assert density["metric"] == "logical_reservation_not_rss"
            assert 0 < density["empty_bytes"] <= 12 << 20
            assert 0 < density["actor_bytes"] <= count * 4096
            assert density["populated_bytes"] == density["empty_bytes"] + density["actor_bytes"]
            assert density["retired_bytes"] == density["empty_bytes"]
            assert density["resources_released"] is True
        print("Scheduler idle actor reservations verified: shared reply capacity, bounded metadata and cleanup; not RSS")
        for count in (1000, 10000):
            sparse = json.loads(example("scheduler_sparse", counter, count).stdout)
            assert sparse["actors"] == count and sparse["active"] == 100
            assert sparse["workers"] == 2 and sparse["replies"] == 600
            assert sparse["metric"] == "logical_reservation_not_rss"
            assert 0 < sparse["empty_bytes"] < sparse["populated_bytes"] <= 256 << 20
            assert sparse["resources_released"] is True
        print("Sparse scheduler verified: 1000/10000 actors, 100 active, FIFO, generation reuse and cleanup; not BEAM/performance")
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
        example("parallel_modules", counter)
        library_name = "program.dylib" if os.uname().sysname == "Darwin" else "program.so"
        invalid_module_manifest = parent / "module-bad-manifest"
        invalid_module_manifest.mkdir()
        (invalid_module_manifest / "manifest.json").write_text("{")
        shutil.copyfile(counter / library_name, invalid_module_manifest / library_name)
        invalid_module_library = parent / "module-bad-library"
        invalid_module_library.mkdir()
        shutil.copyfile(counter / "manifest.json", invalid_module_library / "manifest.json")
        (invalid_module_library / library_name).write_text("not a native library")
        invalid_module_symbol = parent / "module-missing-symbol"
        invalid_module_symbol.mkdir()
        shutil.copyfile(counter / "manifest.json", invalid_module_symbol / "manifest.json")
        symbol_source = parent / "module-empty.c"
        symbol_source.write_text("int wings_module_fixture(void) { return 0; }\n")
        command([os.environ.get("CC", "cc"), "-dynamiclib" if os.uname().sysname == "Darwin" else "-shared", "-fPIC", symbol_source, "-o", invalid_module_symbol / library_name])
        example("module_accounting", counter, invalid_module_manifest, invalid_module_library, invalid_module_symbol)
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
  on EchoBool(value: Bool) -> Bool { value }
  on FailRequest -> Int { count = 99; 1 / 0 }
}
""")
        worker_artifact = parent / "worker"
        command([COMPILER, "build", worker_source, "-o", worker_artifact])
        example("scalar_worker", worker_artifact)
        example("worker_ingress", worker_artifact)
        example("worker_ingress_requests", worker_artifact)
        record_messages = parent / "record-messages"
        command([COMPILER, "build", ROOT / "examples/record-messages.wings", "-o", record_messages])
        example("record_messages", record_messages)
        command([sys.executable, Path(__file__), "--check-record-message-roots", record_messages])
        result = command(
            [VM, "run", "--trusted", record_messages, "--heap-records", 16],
            json.dumps(message("Store", 1)) + "\n",
            expected=1,
        )
        assert "not JSON arguments" in result.stderr
        list_messages = parent / "list-messages"
        command([COMPILER, "build", ROOT / "examples/list-messages.wings", "-o", list_messages])
        example("list_messages", list_messages)
        command([sys.executable, Path(__file__), "--check-list-message-roots", list_messages])
        for name in ["Store", "Compute"]:
            result = command(
                [VM, "run", "--trusted", list_messages, "--heap-records", 16],
                json.dumps(message(name, 2 << 32)) + "\n",
                expected=1,
            )
            assert "not JSON arguments" in result.stderr
        tcp_artifact = parent / "tcp-counter"
        command([COMPILER, "build", ROOT / "examples/tcp-counter.wings", "-o", tcp_artifact])
        verify_tcp_service(tcp_artifact)
        command([TARGET / PROFILE / "examples/tcp_counter", "--trusted", counter, 1, 1], expected=1)
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
        command([TARGET / PROFILE / "examples/frame_limits", parent / "frame-limit-rejected"])
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
        example("stack_boundary", counter, pressure_artifact)
        records = parent / "aot-records"
        records.mkdir()
        cargo_example("wings-compile", "build_aot_records", records)
        example("aot_records", records)
        language_records = parent / "language-records"
        command([COMPILER, "build", ROOT / "examples/records.wings", "-o", language_records])
        heap_run = [VM, "run", "--trusted", language_records, "--heap-records", 2]
        result = command(heap_run, json.dumps(message("Work", 11)) + "\n" + json.dumps(message("Get")) + "\n")
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [11, 11]
        for failure, expected in [(message("Full"), "heap exhausted"), (message("Fail"), "division by zero")]:
            result = command(heap_run, json.dumps(message("Work", 7)) + "\n" + json.dumps(failure) + "\n", expected=1)
            assert result.stdout == '{"reply":7}\n' and expected in result.stderr
        result = command([VM, "run", "--trusted", language_records, "--max-steps", 128, "--heap-records", 2], json.dumps(message("Work", 5)) + "\n")
        assert result.stdout == '{"reply":5}\n'
        assert "callback" in invoke(language_records, [message("Work", 5)], expected=1).stderr
        for invalid in ["0", "1025", "-1", "1.5", "not-a-number", "18446744073709551616"]:
            assert "1..=1024" in command([VM, "run", "--trusted", language_records, "--heap-records", invalid], expected=2).stderr
        for options in [["--heap-records", 2, "--heap-records", 2], ["--max-steps", 4, "--max-steps", 8], ["--unknown", 1]]:
            assert "option" in command([VM, "run", "--trusted", language_records, *options], expected=2).stderr
        initializer_source = parent / "record-initializer.wings"
        initializer_source.write_text("process P { state n: Int = read_int(record_int(7)); on Get -> Int { n } }")
        initializer_artifact = parent / "record-initializer"
        command([COMPILER, "build", initializer_source, "-o", initializer_artifact])
        assert "callback" in invoke(initializer_artifact, [], expected=1).stderr
        assert command([VM, "run", "--trusted", initializer_artifact, "--heap-records", 1], json.dumps(message("Get")) + "\n").stdout == '{"reply":7}\n'
        example("language_records", language_records)
        record_state = parent / "record-state"
        command([COMPILER, "build", ROOT / "examples/record-state.wings", "-o", record_state])
        state_run = [VM, "run", "--trusted", record_state, "--heap-records", 2]
        messages = [message("Get"), message("Replace", 17), message("Get"), message("Set", 23), message("Get")]
        result = command(state_run, "".join(json.dumps(item) + "\n" for item in messages))
        assert [json.loads(line) for line in result.stdout.splitlines()] == [{"reply": 7}, {"accepted": True}, {"reply": 17}, {"reply": 23}, {"reply": 23}]
        result = command(state_run, json.dumps(message("Conditional", True)) + "\n" + json.dumps(message("Conditional", False)) + "\n")
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [18, 14]
        assert "accounted heap" in invoke(record_state, [], expected=1).stderr
        result = command(state_run, json.dumps(message("Fail")) + "\n", expected=1)
        assert not result.stdout and "division by zero" in result.stderr
        result = command([VM, "run", "--trusted", record_state, "--heap-records", 1], json.dumps(message("Set", 1)) + "\n", expected=1)
        assert not result.stdout and "heap exhausted" in result.stderr
        example("persistent_records", record_state)
        example("record_supervisor", record_state)
        command([sys.executable, Path(__file__), "--check-record-roots", record_state])
        branches = parent / "branches"
        command([COMPILER, "build", ROOT / "examples/branches.wings", "-o", branches])
        result = invoke(branches, [message("Select", True, 9), message("Select", False, 11), message("Guard", True), message("OptionalHeap", True), message("Unit", True), message("Bool", False), message("Overflow", False)])
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [9, 9, 17, 9, None, True, 9]
        result = invoke(branches, [message("Nested", first, second) for first in [True, False] for second in [True, False]])
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [11, 22, 33, 44]
        for request, reason in [(message("Guard", False), "division by zero"), (message("OptionalHeap", False), "callback"), (message("Overflow", True), "overflow")]:
            result = invoke(branches, [request], expected=1)
            assert not result.stdout and reason in result.stderr
        for flag, budget in [(True, 7), (False, 6)]:
            text = json.dumps(message("Select", flag, 17)) + "\n"
            result = command([VM, "run", "--trusted", branches, "--max-steps", budget], text)
            assert json.loads(result.stdout)["reply"] == (17 if flag else 7)
            result = command([VM, "run", "--trusted", branches, "--max-steps", budget - 1], text, expected=1)
            assert not result.stdout and "step budget exhausted" in result.stderr
        example("conditional_execution", branches)
        command([sys.executable, Path(__file__), "--check-branch-roots", branches])
        loops = parent / "loops"
        command([COMPILER, "build", ROOT / "examples/loops.wings", "-o", loops])
        scheduler = parent / "scheduler"
        command([COMPILER, "build", ROOT / "examples/scheduler.wings", "-o", scheduler])
        example("multi_worker", scheduler, loops)
        byte_buffers = parent / "bytes"
        command([COMPILER, "build", ROOT / "examples/bytes.wings", "-o", byte_buffers])
        example("byte_buffers", byte_buffers)
        command([sys.executable, Path(__file__), "--check-byte-roots", byte_buffers])
        counter_bytes = parent / "beam-counter"
        command([COMPILER, "build", ROOT / "examples/beam-counter.wings", "-o", counter_bytes])
        command([sys.executable, ROOT / "scripts/verify_counter_equivalence.py", "--artifact", counter_bytes])
        multi_counter = parent / "beam-multi-counter"
        command([COMPILER, "build", ROOT / "examples/beam-multi-counter.wings", "-o", multi_counter])
        command([sys.executable, ROOT / "scripts/verify_multi_counter.py", "--artifact", multi_counter])
        command([sys.executable, ROOT / "scripts/verify_open_loop.py", "--artifact", multi_counter])
        command([sys.executable, ROOT / "scripts/verify_measurement.py"])
        fairness = parent / "beam-fairness"
        command([COMPILER, "build", ROOT / "examples/beam-fairness.wings", "-o", fairness])
        command([sys.executable, ROOT / "scripts/verify_fairness.py", "--artifact", fairness])
        senders_counter = parent / "beam-senders-counter"
        command([COMPILER, "build", ROOT / "examples/beam-senders-counter.wings", "-o", senders_counter])
        command([sys.executable, ROOT / "scripts/verify_senders_counter.py", "--artifact", senders_counter])
        mailbox_boundary = parent / "mailbox-boundary"
        command([COMPILER, "build", ROOT / "examples/mailbox-boundary.wings", "-o", mailbox_boundary])
        example("scheduler_overload", mailbox_boundary)
        command([sys.executable, ROOT / "scripts/verify_admission_counter.py", "--artifact", mailbox_boundary])
        result = invoke(loops, [message("Count", 0), message("Count", 10), message("Count", 5), message("Count", 20), message("Nested", 25)])
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == [0, 10, 10, 20, 26]
        result = command([VM, "run", "--trusted", loops, "--heap-records", 2], json.dumps(message("Heap", 30)) + "\n")
        assert json.loads(result.stdout) == {"reply": 30}
        for request, budget, reason in [(message("Forever"), 128, "step budget exhausted"), (message("Count", 100), 32, "step budget exhausted"), (message("Fail", 10), 1024, "division by zero")]:
            result = command([VM, "run", "--trusted", loops, "--max-steps", budget], json.dumps(request) + "\n", expected=1)
            assert not result.stdout and reason in result.stderr
        example("loop_execution", loops)
        record_branches = parent / "record-branches"
        command([COMPILER, "build", ROOT / "examples/record-branches.wings", "-o", record_branches])
        example("record_conditionals", record_branches)
        command([sys.executable, Path(__file__), "--check-record-branch-roots", record_branches])
        command([sys.executable, Path(__file__), "--check-loop-branch-roots", loops])
        record_loops = parent / "record-loops"
        command([COMPILER, "build", ROOT / "examples/record-loops.wings", "-o", record_loops])
        example("record_loops", record_loops)
        command([sys.executable, Path(__file__), "--check-record-loop-roots", record_loops])
        functions = parent / "functions"
        command([COMPILER, "build", ROOT / "examples/functions.wings", "-o", functions])
        example("inline_functions", functions)
        command([sys.executable, Path(__file__), "--check-function-roots", functions])
        managed_stack_source = parent / "managed-stack.wings"
        managed_bindings = "\n".join(f"let value_{index} = seed + {index}" for index in range(32))
        managed_total = " + ".join(f"value_{index}" for index in range(32))
        managed_functions = ["fn f0(seed: Int) -> Int { seed + 1 }"]
        for index in range(1, 32):
            managed_functions.append(f"fn f{index}(seed: Int) -> Int {{ {managed_bindings}; let nested = f{index - 1}(seed); {managed_total} + nested }}")
        managed_stack_source.write_text("process ManagedStack { state count: Int = 0; " + "\n".join(managed_functions) + " on Work(seed: Int) -> Int { count = 99; f31(seed) } }")
        managed_stack = parent / "managed-stack"
        command([COMPILER, "build", managed_stack_source, "-o", managed_stack])
        example("managed_stack", managed_stack)
        chunked_gc = parent / "chunked-gc"
        command([COMPILER, "build", ROOT / "examples/chunked-gc.wings", "-o", chunked_gc])
        example("chunked_gc", chunked_gc)
        lists = parent / "lists"
        command([COMPILER, "build", ROOT / "examples/lists.wings", "-o", lists])
        example("int_lists", lists)
        example("detached_process", lists)
        command([sys.executable, Path(__file__), "--check-list-roots", lists])
        self_send = parent / "self-send"
        self_send_roots = parent / "self-send-roots"
        command([COMPILER, "build", ROOT / "examples/self-send.wings", "-o", self_send])
        command([COMPILER, "build", ROOT / "examples/self-send-roots.wings", "-o", self_send_roots])
        example("self_send", self_send, self_send_roots)
        command([sys.executable, Path(__file__), "--check-self-send-roots", self_send_roots])
        local_processes = parent / "local-processes"
        local_process_roots = parent / "local-process-roots"
        command([COMPILER, "build", ROOT / "examples/local-processes.wings", "-o", local_processes])
        command([COMPILER, "build", ROOT / "examples/local-process-roots.wings", "-o", local_process_roots])
        example("local_processes", local_processes, local_process_roots)
        command([sys.executable, Path(__file__), "--check-local-process-roots", local_process_roots])
        request_events = parent / "request-events"
        command([COMPILER, "build", ROOT / "examples/request-events.wings", "-o", request_events])
        example("request_events", request_events)
        example("worker_wait", request_events)
        request_roots = parent / "request-roots"
        command([COMPILER, "build", ROOT / "examples/request-roots.wings", "-o", request_roots])
        command([sys.executable, Path(__file__), "--check-request-roots", request_roots])
        command([sys.executable, Path(__file__), "--check-loop-roots", loops])
        command([sys.executable, Path(__file__), "--check-nested-loop-roots", loops])
        for limit, budget in [(0, 6), (1, 12), (10, 66)]:
            for exact in [True, False]:
                result = command([VM, "run", "--trusted", loops, "--max-steps", budget if exact else budget - 1], json.dumps(message("Count", limit)) + "\n", expected=0 if exact else 1)
                assert (json.loads(result.stdout)["reply"] == limit) if exact else (not result.stdout and "step budget exhausted" in result.stderr)
        comparisons = parent / "comparisons"
        command([COMPILER, "build", ROOT / "examples/comparisons.wings", "-o", comparisons])
        checks = []
        expected = []
        for left, right in [(-2**63, 2**63 - 1), (2**63 - 1, -2**63), (-1, 0), (0, -1), (-2**63, -2**63), (2**63 - 1, 2**63 - 1), (0, 0)]:
            for name, value in [("Eq", left == right), ("Ne", left != right), ("Lt", left < right), ("Le", left <= right), ("Gt", left > right), ("Ge", left >= right), ("Max", max(left, right))]:
                checks.append(message(name, left, right))
                expected.append(value)
        for left in [False, True]:
            for right in [False, True]:
                checks.append(message("BoolEq", left, right))
                expected.append(left == right)
        checks.append(message("UnitEq", None, None))
        expected.append(True)
        result = invoke(comparisons, checks)
        assert [json.loads(line)["reply"] for line in result.stdout.splitlines()] == expected
        result = command([VM, "run", "--trusted", comparisons, "--heap-records", 1], json.dumps(message("Heap", -2**63)) + "\n")
        assert json.loads(result.stdout) == {"reply": True}
        for budget, success in [(5, True), (4, False)]:
            result = command([VM, "run", "--trusted", comparisons, "--max-steps", budget], json.dumps(message("Lt", -1, 0)) + "\n", expected=0 if success else 1)
            assert (json.loads(result.stdout)["reply"] is True) if success else (not result.stdout and "step budget exhausted" in result.stderr)
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
        for field, value in [("abi", 1), ("abi", 2), ("abi", 3), ("abi", 4), ("abi", 5), ("abi", 6), ("abi", 7), ("abi", 8), ("abi", 9), ("abi", 999), ("format", 999), ("target", "wrong-target"), ("library", "/tmp/foreign.so")]:
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
    print("Scalar AOT verification passed: execution/stack boundaries, live stacks, quota accounting, compiler record roots/GC, worker mailboxes/requests/deadlines, bounded supervision, OS wakeup and artifact rejection")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--check-control":
        verify_control_abi(Path(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-record-roots":
        verify_persistent_roots(Path(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-branch-roots":
        verify_persistent_roots(Path(sys.argv[2]), branches=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-loop-roots":
        verify_persistent_roots(Path(sys.argv[2]), loops=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-nested-loop-roots":
        verify_persistent_roots(Path(sys.argv[2]), loops="nested")
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-record-branch-roots":
        verify_persistent_roots(Path(sys.argv[2]), record_branches=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-loop-branch-roots":
        verify_persistent_roots(Path(sys.argv[2]), loops="branch")
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-record-loop-roots":
        verify_persistent_roots(Path(sys.argv[2]), record_loops=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-function-roots":
        verify_persistent_roots(Path(sys.argv[2]), functions=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-list-roots":
        verify_persistent_roots(Path(sys.argv[2]), lists=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-byte-roots":
        verify_persistent_roots(Path(sys.argv[2]), byte_buffers=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-record-message-roots":
        verify_persistent_roots(Path(sys.argv[2]), record_messages=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-list-message-roots":
        verify_persistent_roots(Path(sys.argv[2]), record_messages=True, list_messages=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-self-send-roots":
        verify_persistent_roots(Path(sys.argv[2]), self_send=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-local-process-roots":
        verify_persistent_roots(Path(sys.argv[2]), local_processes=True)
    elif len(sys.argv) == 3 and sys.argv[1] == "--check-request-roots":
        verify_persistent_roots(Path(sys.argv[2]), requests=True)
    else:
        main()
