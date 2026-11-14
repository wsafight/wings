import copy
import json
import os
from pathlib import Path
import platform
import select
import subprocess
import sys
import tempfile

import measure_memory as memory
import verify_scalar_aot as aot


def require(condition, message):
    if not condition:
        raise ValueError(message)


def rejected(function):
    try:
        function()
    except (ValueError, KeyError, TypeError):
        return
    raise ValueError("accepted malformed memory observation")


def models():
    rows = {1: (0, 9), 2: (1, 8), 3: (2, 7), 4: (3, 6), 5: (99, 5)}
    require(set(memory.tree(rows, 2)) == {"2", "3", "4"}, "transitive process tree")
    require(set(memory.tree(rows, 2, {3})) == {"2"}, "excluded observer subtree")
    rejected(lambda: memory.tree(rows, 6))
    rejected(lambda: memory.tree({index: (index - 1, 0) for index in range(1, 258)}, 1))
    rejected(lambda: json.loads('{"version":1,"version":1}', object_pairs_hook=memory.strict_object))
    for system, fields in memory.FIELDS.items():
        record = {"pid": 10, "status": "ok", "start_id": "99", **{field: 1024 for field in fields}}
        host = {"status": "ok", "swap_total_bytes": 4096, "swap_used_bytes": 0}
        if system == "Darwin":
            host.update({field: 0 for field in ("compressor_resident_bytes", "compressed_logical_bytes", "compressor_swapped_page_bytes")})
        sample = {"version": 1, "platform": system, "processes": [record], "host": host,
                  "helper": {"pid": 20, "user_cpu_us": 1, "system_cpu_us": 1, "max_rss_bytes": 4096}}
        require(memory.validate(sample, {"10"})["10"] == record, "valid platform values")
        if system == "Darwin":
            historical = copy.deepcopy(sample)
            historical["host"]["swapped_logical_bytes"] = historical["host"].pop("compressor_swapped_page_bytes")
            rejected(lambda: memory.validate(historical, {"10"}))
            distinct = copy.deepcopy(sample)
            distinct["host"].update({"compressor_resident_bytes": 4096, "compressed_logical_bytes": 16384,
                                     "compressor_swapped_page_bytes": 8192, "swap_used_bytes": 1024})
            require(memory.validate(distinct, {"10"})["10"] == record,
                    "swapped compressor pages not decompressed logical or disk swap bytes")
        before = {"started_ns": 1, "finished_ns": 2, "processes": {"10": {"parent": 1, "rss_bytes": 2048}}}
        combined = memory.combine(before, before, sample, 3, 4)
        require(combined["rss_bytes"] == 6144 and combined["platform_memory"]["totals"]["rss_bytes"] == 1024,
                "census RSS and platform sample not conflated")
        identities = {}
        require(memory.track_identities(combined["platform_memory"], identities) == 0, "initial pid identity")
        require(memory.track_identities(combined["platform_memory"], identities) == 0, "stable pid identity")
        changed = copy.deepcopy(combined["platform_memory"])
        changed["processes"]["10"]["start_id"] = "100"
        require(memory.track_identities(changed, identities) == 1 and not changed["complete"]
                and all(value is None for value in changed["totals"].values()), "pid reuse null totals")
        rejected(lambda: memory.track_identities(copy.deepcopy(combined["platform_memory"]),
                                                 {str(index + 1000): "1" for index in range(memory.MAX_IDENTITIES)}))
        for after in ({**before, "processes": {}},
                      {**before, "processes": {"10": {"parent": 2, "rss_bytes": 2048}}}):
            combined = memory.combine(before, after, sample, 3, 4)["platform_memory"]
            require(not combined["complete"] and all(value is None for value in combined["totals"].values()), "churn not zero totals")
        for error in ({**sample, "host": {"status": "error", "errno": 13}},
                      {**sample, "processes": [{"pid": 10, "status": "error", "errno": 3}]}):
            combined = memory.combine(before, before, error, 3, 4)["platform_memory"]
            require(not combined["complete"] and all(value is None for value in combined["totals"].values()), "missing data not partial totals")
        for key, invalid in (("pid", True), ("pid", 11), ("status", "missing"), ("start_id", "0"),
                             ("start_id", "１２"), ("start_id", 99), ("rss_bytes", True),
                             ("rss_bytes", -1), ("rss_bytes", 2**64)):
            changed = copy.deepcopy(sample)
            changed["processes"][0][key] = invalid
            rejected(lambda: memory.validate(changed, {"10"}))
        for field in fields:
            changed = copy.deepcopy(sample)
            del changed["processes"][0][field]
            rejected(lambda: memory.validate(changed, {"10"}))
        for records in ([], [record, record], [{"pid": 10, "status": "error", "errno": 0}]):
            rejected(lambda: memory.validate({**sample, "processes": records}, {"10"}))
        for invalid in (True, "1", 0):
            rejected(lambda: memory.validate({**sample, "version": invalid}, {"10"}))
        for host in ({"status": "ok"}, {"status": "error"}, {"status": "error", "errno": True},
                     {**sample["host"], "swap_used_bytes": 4097}):
            rejected(lambda: memory.validate({**sample, "host": host}, {"10"}))
        error = {**sample, "processes": [{"pid": 10, "status": "error", "errno": 3}]}
        require(memory.validate(error, {"10"})["10"]["status"] == "error", "missing pid preserved")
        for helper in ({}, {**sample["helper"], "pid": 10}, {**sample["helper"], "max_rss_bytes": -1}):
            rejected(lambda: memory.validate({**sample, "helper": helper}, {"10"}))
    print("Memory model: bounded descendant census, distinct platform metrics, identity/errors and malformed observations checked")


def verify():
    models()
    with tempfile.TemporaryDirectory(prefix="wings-memory-probe-") as directory:
        parent = Path(directory)
        executable = parent / "probe"
        memory.build(aot.ROOT / "scripts/measure_memory.c", executable)
        rejected(lambda: memory.build(aot.ROOT / "scripts/measure_memory.c", executable))
        for arguments in ([], ["0"], ["-1"], ["1.0"], ["2147483648"], ["1", "1"], ["1"] * 257):
            result = subprocess.run([str(executable), *arguments], capture_output=True, timeout=2)
            require(result.returncode == 2 and not result.stdout and not result.stderr, "probe invalid pid input")
        missing = 2**31 - 1
        result = subprocess.run([str(executable), str(missing)], capture_output=True, timeout=2)
        require(result.returncode == 0 and not result.stderr, "probe missing pid result")
        payload = json.loads(result.stdout, object_pairs_hook=memory.strict_object)
        require(memory.validate(payload, {str(missing)})[str(missing)]["status"] == "error", "missing pid not zero")
        program = ("import os, subprocess, sys; "
                   "child = subprocess.Popen([sys.executable, '-c', 'import sys; data=bytearray(2097152); sys.stdin.buffer.read(1)'], stdin=subprocess.PIPE); "
                   "print(child.pid, flush=True); sys.stdin.buffer.read(1); child.stdin.close(); child.wait(timeout=2)")
        child = subprocess.Popen([sys.executable, "-c", program], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            require(select.select([child.stdout], [], [], 2)[0], "memory descendant startup")
            descendant = int(child.stdout.readline())
            sample = memory.snapshot(os.getpid(), executable)
            details = sample["platform_memory"]
            require(details["complete"] and details["census_stable"], "stable live platform snapshot")
            require({str(os.getpid()), str(child.pid), str(descendant)} <= set(details["processes"]), "child and grandchild included")
            require(details["host"]["status"] == "ok", "host memory observed")
            require(details["totals"]["rss_bytes"] > 0 and sample["rss_bytes"] > 0, "live resident bytes")
            field = "physical_footprint_bytes" if platform.system() == "Darwin" else "pss_bytes"
            require(details["totals"][field] > 0, "native platform bytes")
            require(details["helper"]["max_rss_bytes"] > 0, "observer cost retained")
            child.stdin.close()
            child.wait(timeout=3)
            require(child.returncode == 0 and not child.stderr.read(), "descendant cleanup")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            child.stdout.close()
            child.stderr.close()
            if not child.stdin.closed:
                child.stdin.close()
        print(f"Memory probe {platform.system()}: real native values, child/grandchild, observer cost, missing pid and argument rejection; not memory acceptance")


if __name__ == "__main__":
    try:
        verify()
    except (OSError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print(f"Memory verification failed: {error}", file=sys.stderr)
        sys.exit(3)
