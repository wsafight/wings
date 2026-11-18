import os
from pathlib import Path, PurePosixPath
import platform
import re
import time


def read(path, maximum=65536):
    with path.open("rb") as source:
        data = source.read(maximum + 1)
    if len(data) > maximum:
        raise ValueError("CPU quota control file exceeds its bound")
    return data.decode("utf-8")


def number(value):
    if not re.fullmatch(r"[0-9]{1,20}", value) or int(value) >= 2**64:
        raise ValueError("invalid CPU quota integer")
    return int(value)


def counters(text):
    result = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) != 2 or fields[0] in result or len(result) == 32:
            raise ValueError("invalid CPU quota counters")
        result[fields[0]] = number(fields[1])
    return result


def identifiers(text, maximum):
    values = [number(value) for value in text.split()]
    if len(values) > maximum or len(values) != len(set(values)) or any(value == 0 for value in values):
        raise ValueError("invalid CPU quota member list")
    return sorted(values)


def is_descendant(pid, root):
    seen = set()
    while pid != root:
        if pid == 0 or pid in seen or len(seen) == 64:
            return False
        seen.add(pid)
        fields = read(Path(f"/proc/{pid}/stat"), 8192).rpartition(") ")[2].split()
        if len(fields) < 20:
            raise ValueError("invalid CPU cgroup process identity")
        pid = number(fields[1])
    return True


def current_group():
    lines = read(Path("/proc/self/cgroup"), 8192).splitlines()
    if len(lines) != 1 or not lines[0].startswith("0::/"):
        raise ValueError("CPU quota evidence requires a unified cgroup v2 hierarchy")
    path = lines[0][3:]
    if ".." in PurePosixPath(path).parts or "\0" in path:
        raise ValueError("invalid current CPU cgroup path")
    return path


def mount_path(value):
    substitutions = {"040": " ", "011": "\t", "012": "\n", "134": "\\"}
    return re.sub(r"\\(040|011|012|134)", lambda match: substitutions[match[1]], value)


def locate(path, mountinfo):
    matches = []
    for line in mountinfo.splitlines():
        before, separator, after = line.partition(" - ")
        fields = before.split()
        if not separator or len(fields) < 6 or not after.split():
            raise ValueError("invalid CPU cgroup mount table")
        if after.split()[0] != "cgroup2":
            continue
        root = PurePosixPath(mount_path(fields[3]))
        mount = Path(mount_path(fields[4]))
        if not root.is_absolute() or not mount.is_absolute():
            raise ValueError("invalid CPU cgroup mount path")
        try:
            relative = PurePosixPath(path).relative_to(root)
        except ValueError:
            continue
        matches.append((len(root.parts), mount / relative, mount))
    if not matches:
        raise ValueError("current CPU cgroup has no visible cgroup v2 mount")
    _, group, mount = max(matches, key=lambda item: item[0])
    return group, mount


def add_arguments(parser):
    parser.add_argument("--cpu-quota-us", type=int, help="require this aggregate cgroup v2 CPU budget; does not configure it")
    parser.add_argument("--cpu-period-us", type=int, help="require this cgroup v2 CPU period; use with --cpu-quota-us")


def from_arguments(arguments):
    quota, period = arguments.cpu_quota_us, arguments.cpu_period_us
    if quota is None and period is None:
        return None
    if quota is None or period is None or not 1000 <= period <= 1_000_000 or not 1000 <= quota <= 64 * period:
        raise ValueError("CPU quota requires a 1000..=1000000 us period and 1000 us..=64 periods of budget")
    if platform.system() != "Linux":
        raise ValueError("CPU quota evidence requires native Linux with cgroup v2")
    path = current_group()
    group, mount = locate(path, read(Path("/proc/self/mountinfo"), 1 << 20))
    return CpuQuota(group, mount, path, quota, period)


class CpuQuota:
    def __init__(self, group, mount, path, quota, period):
        self.group = group.resolve(strict=True)
        self.mount = mount.resolve(strict=True)
        self.group.relative_to(self.mount)
        self.path = path
        self.quota = quota
        self.period = period
        self.identity = self.file_identity()
        self.previous = None
        self.ancestors = self.configuration()
        self.initial = self.snapshot()
        if self.initial["processes"] != [os.getpid()]:
            raise ValueError("CPU quota measurement must start alone in its cgroup")

    def file_identity(self):
        stat = self.group.stat()
        return [stat.st_dev, stat.st_ino]

    def configuration(self):
        ancestors = []
        group = self.group
        while True:
            quota = read(group / "cpu.max", 128).split() if (group / "cpu.max").exists() else None
            if quota is not None:
                if len(quota) != 2 or (quota[0] != "max" and number(quota[0]) == 0) or number(quota[1]) == 0:
                    raise ValueError("invalid cpu.max")
            burst_path = group / "cpu.max.burst"
            burst = number(read(burst_path, 128).strip()) if burst_path.exists() else None
            ancestors.append({"path": str(group), "cpu_max": quota, "burst_us": burst})
            if group == self.mount:
                break
            if len(ancestors) == 64:
                raise ValueError("CPU cgroup ancestor limit")
            group = group.parent
        if ancestors[0]["cpu_max"] != [str(self.quota), str(self.period)] or ancestors[0]["burst_us"] not in (None, 0):
            raise ValueError("CPU cgroup budget/period mismatch or burst is enabled")
        return ancestors

    def snapshot(self):
        started = time.monotonic_ns()
        if current_group() != self.path or self.file_identity() != self.identity:
            raise ValueError("CPU cgroup identity or placement changed")
        if read(self.group / "cgroup.type", 128).strip() != "domain":
            raise ValueError("CPU quota evidence requires a domain cgroup")
        if self.configuration() != self.ancestors:
            raise ValueError("CPU cgroup or ancestor configuration changed")
        events = counters(read(self.group / "cgroup.events", 1024))
        if events.get("populated") != 1 or events.get("frozen") != 0:
            raise ValueError("CPU cgroup is empty or frozen")
        processes = identifiers(read(self.group / "cgroup.procs"), 512)
        threads = identifiers(read(self.group / "cgroup.threads"), 4096)
        if os.getpid() not in processes or not threads:
            raise ValueError("CPU cgroup membership is incomplete")
        for pid in processes:
            if not is_descendant(pid, os.getpid()):
                raise ValueError("CPU cgroup contains a process outside the measurement job")
        for index, entry in enumerate(self.group.iterdir()):
            if index >= 256 or entry.is_dir():
                raise ValueError("CPU quota measurement requires a leaf cgroup")
        policies = []
        for tid in threads:
            try:
                policies.append(os.sched_getscheduler(tid))
            except ProcessLookupError:
                raise ValueError("CPU cgroup thread exited during observation") from None
        if any(policy != os.SCHED_OTHER for policy in policies):
            raise ValueError("CPU quota evidence requires SCHED_OTHER threads")
        statistics = counters(read(self.group / "cpu.stat", 8192))
        required = {"usage_usec", "user_usec", "system_usec", "nr_periods", "nr_throttled", "throttled_usec"}
        if not required <= statistics.keys():
            raise ValueError("CPU cgroup accounting fields are missing")
        if self.previous is not None and any(statistics[key] < self.previous[key] for key in required):
            raise ValueError("CPU cgroup accounting regressed")
        self.previous = statistics
        return {"started_ns": started, "finished_ns": time.monotonic_ns(),
                "scope": "whole_measurement_job_including_driver_and_probe_helpers",
                "cgroup_path": self.path, "directory_identity": self.identity,
                "quota_us": self.quota, "period_us": self.period, "processes": processes,
                "threads": threads, "scheduler_policy": "SCHED_OTHER", "statistics": statistics,
                "ancestors": self.ancestors}
