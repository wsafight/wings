import json
import os
from pathlib import Path
import shlex
import shutil
import struct
import sys
import tempfile

import verify_scalar_aot as aot


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
