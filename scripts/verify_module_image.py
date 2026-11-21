import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tempfile

import verify_scalar_aot as aot


def verify_manifest_files(parent, counter):
    source = (counter / "manifest.json").read_bytes()
    cases = []
    sockets = []
    report = {"status": "failed", "cases": [], "commands": []}

    def execute(name, operation):
        record = {"name": name}
        report["commands"].append(record)
        streams = {}
        try:
            result = operation()
            record["exit"] = result.returncode
            streams = {stream: getattr(result, stream) for stream in ("stdout", "stderr")}
            return result
        except BaseException as error:
            record["error"] = str(error)
            if isinstance(error, subprocess.TimeoutExpired):
                streams = {"stdout": error.stdout or b"", "stderr": error.stderr or b""}
            raise
        finally:
            for stream, value in streams.items():
                path = parent / ("manifest-" + name + "." + stream)
                path.write_bytes(value if isinstance(value, bytes) else value.encode())
                record[stream] = {"path": path.name,
                                  "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}

    try:
        for name in ("regular", "exact", "oversized", "empty", "directory", "fifo", "socket",
                     "symlink-regular", "symlink-fifo", "symlink-directory", "symlink-missing"):
            directory = parent / ("manifest-" + name)
            shutil.copytree(counter, directory)
            path = directory / "manifest.json"
            path.unlink()
            if name == "regular":
                path.write_bytes(source)
            elif name in ("exact", "oversized"):
                path.write_bytes(source + b" " * (65536 + (name == "oversized") - len(source)))
            elif name == "empty":
                path.write_bytes(b"")
            elif name == "directory":
                path.mkdir()
            elif name == "fifo":
                os.mkfifo(path, 0o600)
            elif name == "socket":
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sockets.append(listener)
                previous = Path.cwd()
                try:
                    os.chdir(directory)
                    listener.bind("manifest.json")
                finally:
                    os.chdir(previous)
            else:
                target = directory / "target"
                if name == "symlink-regular":
                    target.write_bytes(source)
                elif name == "symlink-fifo":
                    os.mkfifo(target, 0o600)
                elif name == "symlink-directory":
                    target.mkdir()
                path.symlink_to("target")
            expected = ("ok" if name in ("regular", "exact", "symlink-regular") else
                        "exceeds 64 KiB" if name == "oversized" else
                        "EOF" if name == "empty" else
                        "No such file" if name == "symlink-missing" else "must be a regular file")
            cases.append((directory, expected))
            report["cases"].append({"name": name, "directory": str(directory), "expected": expected})
        arguments = [value for pair in cases for value in pair]
        result = json.loads(execute("accounting", lambda: aot.example("manifest_loading", *arguments)).stdout)
        report["accounting"] = result
        assert result["status"] == "ok" and result["resources_released"], result
        assert result["bootstrap_bytes"] == result["remaining_bytes"], result
        assert result["accepted"] == 6 and result["rejected"] == 16, result
        for directory, expected in cases:
            result = execute(directory.name, lambda: aot.invoke(
                directory, [aot.message("inc", 7)], expected=0 if expected == "ok" else 1))
            if expected == "ok":
                assert json.loads(result.stdout) == {"reply": 7} and not result.stderr, result
            else:
                assert not result.stdout and expected in result.stderr, result
        report["status"] = "ok"
        print("Manifest loading verified: 11 file/target cases, accounted/trusted cleanup and real CLI execution")
    finally:
        for listener in sockets:
            listener.close()
        (parent / "manifest-report.json").write_text(json.dumps(report, indent=2) + "\n")


def build_with_linker(wrapper, artifact, compiler, extra):
    wrapper.write_text("#!/bin/sh\nexec " + " ".join(map(shlex.quote, [compiler, *extra])) + ' "$@"\n')
    wrapper.chmod(0o700)
    previous = os.environ.get("CC")
    os.environ["CC"] = str(wrapper)
    try:
        aot.command([aot.COMPILER, "build", aot.ROOT / "examples/counter.wings", "-o", artifact])
    finally:
        if previous is None:
            del os.environ["CC"]
        else:
            os.environ["CC"] = previous


def verify(parent):
    counter = parent / "counter"
    aot.command([aot.COMPILER, "build", aot.ROOT / "examples/counter.wings", "-o", counter])
    library = "program.dylib" if os.uname().sysname == "Darwin" else "program.so"
    invalid = []
    for name in ("bad-manifest", "bad-library", "missing-symbol"):
        directory = parent / name
        shutil.copytree(counter, directory)
        invalid.append(directory)
    (invalid[0] / "manifest.json").write_text("{")
    (invalid[1] / library).write_text("not a native library")
    source = parent / "empty.c"
    source.write_text("int wings_module_fixture(void) { return 0; }\n")
    aot.command([os.environ.get("CC", "cc"), "-dynamiclib" if os.uname().sysname == "Darwin" else "-shared",
                 "-fPIC", source, "-o", invalid[2] / library])
    baseline = aot.module_image_budget(counter)
    print(aot.example("module_accounting", counter, *invalid, baseline).stdout, end="")
    verify_manifest_files(parent, counter)

    assembly = parent / "padding.s"
    padding = parent / "padding.o"
    if os.uname().sysname == "Darwin":
        assembly.write_text(".zerofill __WINGS,__padding,_wings_image_padding,1048576,14\n")
        flags = ["-Wl,-segprot,__WINGS,r,r"]
    else:
        assembly.write_text('.section .wings_padding,"a",@nobits\n.balign 4096\n.space 1048576\n.section .note.GNU-stack,"",@progbits\n')
        flags = []
    compiler = os.environ.get("CC", "cc")
    aot.command([compiler, "-c", assembly, "-o", padding])
    wrapper = parent / "linker"
    expanded = parent / "expanded"
    build_with_linker(wrapper, expanded, compiler, [str(padding), *flags])
    budget = aot.module_image_budget(expanded)
    file_bytes = (expanded / library).stat().st_size
    assert budget >= baseline + (1 << 20)
    assert file_bytes < 1 << 20
    oversized = parent / "oversized"
    shutil.copytree(expanded, oversized)
    data = bytearray((expanded / library).read_bytes())
    if os.uname().sysname == "Darwin":
        offset = 32
        for _ in range(struct.unpack_from("<I", data, 16)[0]):
            kind, size = struct.unpack_from("<II", data, offset)
            if kind == 0x19 and data[offset + 8:offset + 24].rstrip(b"\0") == b"__WINGS":
                struct.pack_into("<Q", data, offset + 32, 65 << 20)
                break
            offset += size
        else:
            raise AssertionError("missing read-only zero-fill segment")
    else:
        offset = struct.unpack_from("<Q", data, 32)[0]
        size, count = struct.unpack_from("<HH", data, 54)
        for index in range(count):
            base = offset + index * size
            if struct.unpack_from("<I", data, base)[0] == 1 and struct.unpack_from("<Q", data, base + 40)[0] >= 1 << 20:
                struct.pack_into("<Q", data, base + 40, 65 << 20)
                break
        else:
            raise AssertionError("missing read-only zero-fill segment")
    (oversized / library).write_bytes(data)
    dependency = parent / ("dependency.dylib" if os.uname().sysname == "Darwin" else "dependency.so")
    if os.uname().sysname == "Darwin":
        aot.command([compiler, "-dynamiclib", padding, *flags, "-o", dependency])
        dependency_flags = ["-Wl,-needed_library," + str(dependency)]
    else:
        aot.command([compiler, "-shared", "-nostdlib", padding, "-o", dependency])
        dependency_flags = ["-Wl,--no-as-needed", str(dependency)]
    dependent = parent / "dependent"
    build_with_linker(wrapper, dependent, compiler, dependency_flags)
    print(aot.example("module_accounting", expanded, *invalid, oversized, dependent, budget).stdout, end="")
    print(json.dumps({"status": "ok", "baseline_budget": baseline, "expanded_budget": budget,
                      "expanded_file_bytes": file_bytes, "zero_fill_bytes": 1 << 20,
                      "unsupported_dependency_rejected": True,
                      "metric": "logical_image_span_not_rss_or_mvp_acceptance"}))


def main():
    if len(sys.argv) == 2:
        parent = Path(sys.argv[1]).resolve()
        parent.mkdir()
        verify(parent)
    else:
        with tempfile.TemporaryDirectory(prefix="wings-module-image-") as temporary:
            verify(Path(temporary))


if __name__ == "__main__":
    main()
