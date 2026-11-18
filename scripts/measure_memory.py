import json
import os
from pathlib import Path
import platform
import shlex
import subprocess
import time


MAX_PROCESSES = 256
MAX_IDENTITIES = 4096
FIELDS = {
    "Darwin": ("rss_bytes", "physical_footprint_bytes"),
    "Linux": ("rss_bytes", "pss_bytes", "private_clean_bytes", "private_dirty_bytes",
              "shared_clean_bytes", "shared_dirty_bytes", "swap_bytes", "swap_pss_bytes"),
}


def build(source, destination):
    compiler = shlex.split(os.environ.get("CC", "cc"))
    if not compiler or destination.exists():
        raise ValueError("memory probe compiler or destination")
    command = [*compiler, "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", str(source), "-o", str(destination)]
    subprocess.run(command, check=True, capture_output=True, timeout=30)
    return command


def tree(rows, root, excluded=()):
    if root not in rows:
        raise ValueError("memory census root missing")
    included = {root}
    while True:
        children = {pid for pid, (parent, _rss) in rows.items() if parent in included and pid not in excluded}
        if children <= included:
            break
        included |= children
        if len(included) > MAX_PROCESSES:
            raise ValueError("memory process tree limit")
    return {str(pid): {"parent": rows[pid][0], "rss_bytes": rows[pid][1]} for pid in sorted(included)}


def census(root):
    started = time.monotonic_ns()
    with subprocess.Popen(["ps", "-axo", "pid=,ppid=,rss="], stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
        try:
            stdout, stderr = child.communicate(timeout=2)
        except BaseException:
            child.kill()
            child.wait()
            raise
        if child.returncode or stderr or len(stdout) > 8 << 20:
            raise ValueError("memory census failed or exceeded output limit")
        rows = {}
        for line in stdout.splitlines():
            pid, parent, rss = map(int, line.split())
            if pid <= 0 or parent < 0 or rss < 0 or pid in rows:
                raise ValueError("memory census malformed")
            rows[pid] = (parent, rss * 1024)
        processes = tree(rows, root, {child.pid})
        observer_rss = rows.get(child.pid, (0, 0))[1]
    return {"started_ns": started, "finished_ns": time.monotonic_ns(), "processes": processes,
            "observer": {"pid": child.pid, "rss_bytes": observer_rss}}


def validate(payload, expected):
    if (type(payload) is not dict or type(payload.get("version")) is not int
            or payload["version"] != 1 or payload.get("platform") not in FIELDS):
        raise ValueError("memory probe protocol")
    records = payload.get("processes")
    if type(records) is not list or len(records) != len(expected):
        raise ValueError("memory probe process count")
    processes = {}
    for record in records:
        if type(record) is not dict or type(record.get("pid")) is not int:
            raise ValueError("memory probe pid")
        pid = str(record["pid"])
        if pid not in expected or pid in processes:
            raise ValueError("memory probe foreign or duplicate pid")
        if record.get("status") == "ok":
            identity = record.get("start_id")
            if type(identity) is not str or not identity.isascii() or not identity.isdecimal() or int(identity) <= 0:
                raise ValueError("memory probe process identity")
            for field in FIELDS[payload["platform"]]:
                if type(record.get(field)) is not int or not 0 <= record[field] <= 2**64 - 1:
                    raise ValueError("memory probe value")
        elif record.get("status") == "error":
            if type(record.get("errno")) is not int or record["errno"] <= 0:
                raise ValueError("memory probe errno")
        else:
            raise ValueError("memory probe process status")
        processes[pid] = record
    host = payload.get("host")
    if type(host) is not dict or host.get("status") not in ("ok", "error"):
        raise ValueError("memory probe host status")
    if host["status"] == "ok":
        fields = ("swap_total_bytes", "swap_used_bytes")
        if payload["platform"] == "Darwin":
            fields += ("compressor_resident_bytes", "compressed_logical_bytes", "compressor_swapped_page_bytes")
        if any(type(host.get(field)) is not int or not 0 <= host[field] <= 2**64 - 1 for field in fields):
            raise ValueError("memory probe host value")
        if host["swap_used_bytes"] > host["swap_total_bytes"]:
            raise ValueError("memory probe swap totals")
    elif not ((type(host.get("errno")) is int and host["errno"] > 0)
              or (type(host.get("mach_status")) is int and host["mach_status"] != 0)):
        raise ValueError("memory probe host error")
    helper = payload.get("helper")
    if (type(helper) is not dict or type(helper.get("pid")) is not int or helper["pid"] <= 0
            or str(helper["pid"]) in expected):
        raise ValueError("memory probe helper pid")
    for field in ("user_cpu_us", "system_cpu_us", "max_rss_bytes"):
        if type(helper.get(field)) is not int or not 0 <= helper[field] <= 2**64 - 1:
            raise ValueError("memory probe helper value")
    return processes


def strict_object(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValueError("memory probe duplicate JSON field")
        result[name] = value
    return result


def snapshot(root, probe, timeout=2):
    before = census(root)
    started = time.monotonic_ns()
    result = subprocess.run([str(probe), *before["processes"]], capture_output=True, timeout=timeout)
    if result.returncode or result.stderr or len(result.stdout) > 1 << 20:
        raise ValueError("memory probe failed or exceeded output limit")
    payload = json.loads(result.stdout, object_pairs_hook=strict_object)
    validate(payload, before["processes"])
    if payload["platform"] != platform.system():
        raise ValueError("memory probe platform mismatch")
    finished = time.monotonic_ns()
    after = census(root)
    return combine(before, after, payload, started, finished)


def combine(before, after, payload, started, finished):
    processes = validate(payload, before["processes"])
    stable = {pid: row["parent"] for pid, row in before["processes"].items()} == {
        pid: row["parent"] for pid, row in after["processes"].items()}
    complete = (stable and payload["host"]["status"] == "ok"
                and all(record["status"] == "ok" for record in processes.values()))
    totals = {field: sum(record[field] for record in processes.values()) if complete else None
              for field in FIELDS[payload["platform"]]}
    rss = {pid: row["rss_bytes"] for pid, row in before["processes"].items()}
    observer = before.get("observer", {"rss_bytes": 0})
    safety_rss = sum(rss.values()) + max(observer["rss_bytes"], payload["helper"]["max_rss_bytes"])
    return {"rss_bytes": safety_rss, "processes": rss,
            "rss_safety_scope": "census tree plus larger census RSS/probe peak; conservative non-simultaneous helper allowance",
            "platform_memory": {"version": 1, "platform": payload["platform"], "complete": complete,
                                "census_stable": stable, "census_before": before, "census_after": after,
                                "probe_started_ns": started, "probe_finished_ns": finished,
                                "totals": totals, "processes": processes, "host": payload["host"],
                                "helper": payload["helper"],
                                "helper_wall_ns": finished - started,
                                "scope": "driver plus captured descendants; probe and census helpers excluded; not atomic or whole-host attribution"}}


def track_identities(details, identities):
    changed = []
    for pid, record in details.get("processes", {}).items():
        if record["status"] != "ok":
            continue
        previous = identities.get(pid)
        if previous is None and len(identities) >= MAX_IDENTITIES:
            raise ValueError("memory process identity history limit")
        if previous is not None and previous != record["start_id"]:
            changed.append(pid)
        identities[pid] = record["start_id"]
    details["identity_changes"] = changed
    if changed:
        details["complete"] = False
        details["totals"] = dict.fromkeys(details["totals"])
    return len(changed)
