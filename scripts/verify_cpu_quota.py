import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import measure_cpu as cpu
import measure_open_loop as measure


STATISTICS = "usage_usec 100\nuser_usec 70\nsystem_usec 30\nnr_periods 10\nnr_throttled 2\nthrottled_usec 9\n"


class CpuQuotaVerification(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="wings-cpu-quota-model-")
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()
        self.mount = self.parent / "cgroup"
        self.group = self.mount / "job"
        self.group.mkdir(parents=True)
        self.files = {
            "cpu.max": "100000 100000\n", "cpu.max.burst": "0\n",
            "cpu.stat": STATISTICS, "cgroup.type": "domain\n",
            "cgroup.events": "populated 1\nfrozen 0\n",
            "cgroup.procs": f"{os.getpid()}\n", "cgroup.threads": f"{os.getpid()}\n",
        }
        for name, value in self.files.items():
            (self.group / name).write_text(value)
        self.context = ExitStack()
        self.addCleanup(self.context.close)
        self.context.enter_context(patch("measure_cpu.current_group", return_value="/job"))
        self.context.enter_context(patch("measure_cpu.os.SCHED_OTHER", 0, create=True))
        self.scheduler = self.context.enter_context(patch("measure_cpu.os.sched_getscheduler", return_value=0, create=True))

    def controller(self):
        return cpu.CpuQuota(self.group, self.mount, "/job", 100000, 100000)

    def test_fixed_budget_observations_preserve_all_control_files_and_ancestors(self):
        (self.mount / "cpu.max").write_text("max 100000\n")
        control = self.controller()
        for _ in range(2):
            sample = control.snapshot()
            self.assertEqual(sample["statistics"]["usage_usec"], 100)
            self.assertEqual(sample["processes"], [os.getpid()])
            self.assertEqual(sample["scope"], "whole_measurement_job_including_driver_and_probe_helpers")
            self.assertLessEqual(sample["started_ns"], sample["finished_ns"])
            self.assertEqual(len(sample["ancestors"]), 2)
        for name, value in self.files.items():
            self.assertEqual((self.group / name).read_text(), value)
        self.assertEqual((self.mount / "cpu.max").read_text(), "max 100000\n")

    def test_invalid_or_changed_kernel_controls_fail_without_rewriting_them(self):
        for name, value in [
            ("cpu.max", "max 100000\n"), ("cpu.max", "99999 100000\n"),
            ("cpu.max", "100000 99999\n"), ("cpu.max", "100000 0\n"),
            ("cpu.max.burst", "1\n"), ("cgroup.type", "threaded\n"),
            ("cgroup.events", "populated 1\nfrozen 1\n"),
            ("cgroup.events", "populated 0\nfrozen 0\n"),
            ("cgroup.procs", ""), ("cgroup.procs", "1\n1\n"),
            ("cgroup.threads", ""), ("cpu.stat", STATISTICS.replace("100\n", "99\n")),
            ("cpu.stat", "usage_usec 100\n"), ("cpu.max", "x" * 129),
        ]:
            with self.subTest(name=name, value=value):
                control = self.controller()
                (self.group / name).write_text(value)
                with self.assertRaises(ValueError):
                    control.snapshot()
                self.assertEqual((self.group / name).read_text(), value)
                (self.group / name).write_text(self.files[name])
        control = self.controller()
        (self.mount / "cpu.max").write_text("50000 100000\n")
        with self.assertRaisesRegex(ValueError, "configuration changed"):
            control.snapshot()

    def test_directory_placement_children_and_nonfair_threads_cannot_keep_valid_evidence(self):
        control = self.controller()
        with patch("measure_cpu.current_group", return_value="/elsewhere"):
            with self.assertRaisesRegex(ValueError, "placement changed"):
                control.snapshot()
        (self.group / "nested").mkdir()
        with self.assertRaisesRegex(ValueError, "leaf cgroup"):
            control.snapshot()
        (self.group / "nested").rmdir()
        self.scheduler.return_value = 1
        with self.assertRaisesRegex(ValueError, "SCHED_OTHER"):
            control.snapshot()
        self.scheduler.side_effect = ProcessLookupError
        with self.assertRaisesRegex(ValueError, "thread exited"):
            control.snapshot()
        self.scheduler.side_effect = None
        self.scheduler.return_value = 0
        old = self.mount / "retired"
        self.group.rename(old)
        self.group.mkdir()
        with self.assertRaisesRegex(ValueError, "identity"):
            control.snapshot()

    def test_shared_group_foreign_processes_and_missing_files_fail_closed(self):
        (self.group / "cgroup.procs").write_text(f"{os.getpid()}\n999999\n")
        with patch("measure_cpu.is_descendant", return_value=True):
            with self.assertRaisesRegex(ValueError, "start alone"):
                self.controller()
        (self.group / "cgroup.procs").write_text(self.files["cgroup.procs"])
        control = self.controller()
        (self.group / "cgroup.procs").write_text(f"{os.getpid()}\n999999\n")
        with patch("measure_cpu.is_descendant", side_effect=lambda pid, root: pid == root):
            with self.assertRaisesRegex(ValueError, "outside the measurement job"):
                control.snapshot()
        (self.group / "cgroup.procs").write_text(self.files["cgroup.procs"])
        (self.group / "cpu.max").unlink()
        with self.assertRaises(ValueError):
            control.snapshot()

    def test_default_and_nonlinux_options_do_not_access_control_files(self):
        with patch("measure_cpu.read", side_effect=AssertionError("unexpected file access")):
            self.assertIsNone(cpu.from_arguments(argparse.Namespace(cpu_quota_us=None, cpu_period_us=None)))
            for quota, period in [(100000, None), (None, 100000), (0, 100000), (100000, 0), (100000, 1000001)]:
                with self.assertRaises(ValueError):
                    cpu.from_arguments(argparse.Namespace(cpu_quota_us=quota, cpu_period_us=period))
            with patch("measure_cpu.platform.system", return_value="Darwin"):
                with self.assertRaisesRegex(ValueError, "native Linux"):
                    cpu.from_arguments(argparse.Namespace(cpu_quota_us=100000, cpu_period_us=100000))

    def test_mount_resolution_bounded_values_and_member_formats(self):
        mountinfo = "1 0 0:1 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n"
        self.assertEqual(cpu.locate("/job", mountinfo), (Path("/sys/fs/cgroup/job"), Path("/sys/fs/cgroup")))
        nested = mountinfo + "2 1 0:1 /job /delegated\\040group rw - cgroup2 cgroup rw\n"
        self.assertEqual(cpu.locate("/job/child", nested), (Path("/delegated group/child"), Path("/delegated group")))
        for value in ["-1", "1.0", "", "1" * 21, str(2**64), "nan"]:
            with self.assertRaises(ValueError):
                cpu.number(value)
        for value in ["0", "1 1", "1 2 3"]:
            with self.assertRaises(ValueError):
                cpu.identifiers(value, 2)
        for value in ["a 1\na 1\n", "a 1 2\n", "a nan\n"]:
            with self.assertRaises(ValueError):
                cpu.counters(value)
        with self.assertRaises(ValueError):
            cpu.locate("/job", "1 0 0:1 / /x rw - tmpfs tmpfs rw\n")

    def test_cpu_observation_failure_is_retained_before_a_measurement_child_is_started(self):
        control = self.controller()
        (self.group / "cpu.max.burst").write_text("1\n")
        destination = self.parent / "refused"
        with patch("measure_open_loop.subprocess.Popen") as launch:
            with self.assertRaises(ValueError):
                measure.measurement(["not-started"], self.parent / "trace", destination,
                                    "sparse-256", 1000, runtime="wings", cpu_control=control)
            launch.assert_not_called()
        record = json.loads(destination.with_suffix(".json").read_text())
        self.assertEqual(record["status"], "invalid_experiment")
        self.assertIn("burst", record["error"])

    def test_process_ancestry_handles_names_cycles_missing_identities_and_depth(self):
        root = os.getpid()

        def identity(path, maximum):
            self.assertEqual(maximum, 8192)
            pid = int(path.parent.name)
            parent = 33 if pid == 34 else root
            return f"{pid} (name ) with spaces) " + " ".join(["S", str(parent), *(["0"] * 17), "42"])

        with patch("measure_cpu.read", side_effect=identity):
            self.assertTrue(cpu.is_descendant(34, root))
        with patch("measure_cpu.read", return_value="7 (cycle) " + " ".join(["S", "7", *(["0"] * 18)])):
            self.assertFalse(cpu.is_descendant(7, root))
        with patch("measure_cpu.read", return_value="bad"):
            with self.assertRaises(ValueError):
                cpu.is_descendant(7, root)
        with patch("measure_cpu.read", side_effect=FileNotFoundError):
            with self.assertRaises(OSError):
                cpu.is_descendant(7, root)

    def test_measurement_collects_optional_cpu_evidence_and_preserves_midrun_rejection(self):
        events = [{"event": "start", "warmup_seconds": 30, "sample_seconds": 60},
                  {"event": "sampling", "elapsed_ns": 30_000_000_000},
                  {"environment": {"runtime": "wings"}, "resources_released": True,
                   "bootstrap_bytes": 232, "remaining_bytes": 232}]
        program = "import time; time.sleep(0.05); " + "; ".join(f"print({json.dumps(event)!r})" for event in events)
        initial = {"processes": [os.getpid()], "model_only": True}
        memory = {"rss_bytes": 0, "platform_memory": {"complete": False}}
        for fails in [False, True]:
            control = self.controller()
            values = [initial, ValueError("changed during sampling")] if fails else lambda: dict(initial)
            destination = self.parent / str(fails)
            with patch.object(control, "snapshot", side_effect=values), patch("measure_open_loop.memory_snapshot", return_value=memory):
                if fails:
                    with self.assertRaisesRegex(ValueError, "changed during sampling"):
                        measure.measurement([sys.executable, "-c", program], self.parent / "synthetic", destination,
                                            "sparse-256", 1000, runtime="wings", cpu_control=control)
                else:
                    result = measure.measurement([sys.executable, "-c", program], self.parent / "synthetic", destination,
                                                 "sparse-256", 1000, runtime="wings", cpu_control=control,
                                                 validator=lambda *arguments: {"model_only": True})
                    self.assertEqual(result["cpu_quota"]["status"], "observations_complete")
                    self.assertFalse(result["cpu_quota"]["performance_gate_proven"])
                    self.assertGreater(result["cpu_quota"]["samples"], 0)
            record = json.loads(destination.with_suffix(".json").read_text())
            self.assertEqual(record["status"], "invalid_experiment" if fails else "ok")
            self.assertEqual(record["cpu_quota"]["status"], "invalid_experiment" if fails else "observations_complete")

    def test_partial_cli_quota_fails_before_creating_either_experiment_directory(self):
        for script in ("measure_open_loop.py", "measure_fairness.py"):
            output = self.parent / script
            command = [sys.executable, str(Path(__file__).with_name(script)), "--artifact", str(self.parent / "absent"),
                       "--output", str(output), "--cpu-quota-us", "100000"]
            if script == "measure_open_loop.py":
                command += ["--case", "sparse-256"]
            result = subprocess.run(command, capture_output=True, text=True, timeout=10, preexec_fn=measure.aot.limit_child)
            self.assertEqual(result.returncode, 3, result.stderr)
            self.assertIn("CPU quota requires", result.stderr)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
