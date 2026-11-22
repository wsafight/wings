import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

import measure_lock_statistics as diagnostic
import verify_scalar_aot as aot
from verify_multi_counter import invoke


def model():
    stats = {"acquisitions": 1, "wait_ns": 100, "held_ns": 200,
             "max_wait_ns": 100, "max_held_ns": 200, "saturated": False}
    workers = [{"worker": worker, **{phase: copy.deepcopy(stats) for phase in diagnostic.PHASES}} for worker in (0, 1)]
    return {"actors": 1, "committed": [1], "worker_lock_statistics": {
        "version": 1, "scope": "whole_scheduler_lifetime", "metric": "wall_time_not_cpu",
        "after_worker_join": True, "workers": workers}}


def cpu_model():
    return {"worker_cpu_statistics": {"version": 1, "scope": "worker_function_including_setup_cleanup",
        "metric": "thread_cpu_time_not_wall", "after_worker_join": True,
        "workers": [{"worker": worker, "status": "ok", "nanoseconds": 10} for worker in (0, 1)]}}


def contention_model():
    metrics = {"contended_acquisitions": 1, "failed_attempts": 3, "yield_calls": 0,
               "wait_ns": 10, "max_wait_ns": 10, "saturated": False}
    return {"scheduler_contention_statistics": {
        "version": 1, "scope": "whole_scheduler_lifetime", "after_worker_join": True,
        "fast_slot": copy.deepcopy(metrics), "mailbox": copy.deepcopy(metrics)}}


class LockStatistics(unittest.TestCase):
    def test_cpu_switch_scope_and_version_are_explicit(self):
        value = cpu_model()
        self.assertEqual(diagnostic.validate_cpu(value, True), value["worker_cpu_statistics"])
        self.assertIsNone(diagnostic.validate_cpu({}, False))
        for result, enabled in ((value, False), ({}, True), (value, 1)):
            with self.assertRaises(ValueError):
                diagnostic.validate_cpu(result, enabled)
        for key, forged in (("version", True), ("version", 2), ("scope", "whole_process"),
                            ("metric", "wall_time"), ("after_worker_join", False), ("after_worker_join", 1)):
            value = cpu_model()
            value["worker_cpu_statistics"][key] = forged
            with self.assertRaises(ValueError):
                diagnostic.validate_cpu(value, True)

    def test_cpu_missing_foreign_or_duplicate_workers_are_rejected(self):
        for identity in (0, 2, True, -1):
            value = cpu_model()
            value["worker_cpu_statistics"]["workers"][1]["worker"] = identity
            with self.assertRaises(ValueError):
                diagnostic.validate_cpu(value, True)
        for count in (0, 1, 3):
            value = cpu_model()
            value["worker_cpu_statistics"]["workers"] = (value["worker_cpu_statistics"]["workers"] * 2)[:count]
            with self.assertRaises(ValueError):
                diagnostic.validate_cpu(value, True)

    def test_cpu_unavailable_or_incomplete_is_not_zero(self):
        for status, nanoseconds in (("pending", None), ("unavailable", None), ("ok", None),
                                    ("ok", True), ("ok", -1), ("ok", (1 << 64) - 2), ("ok", 1 << 64)):
            value = cpu_model()
            value["worker_cpu_statistics"]["workers"][0].update({"status": status, "nanoseconds": nanoseconds})
            with self.assertRaises(ValueError):
                diagnostic.validate_cpu(value, True)
        value = cpu_model()
        value["worker_cpu_statistics"]["workers"][0]["nanoseconds"] = 0
        diagnostic.validate_cpu(value, True)

    def test_enabled_and_disabled_are_explicit(self):
        value = model()
        self.assertEqual(diagnostic.validate_statistics(value, True), value["worker_lock_statistics"])
        self.assertIsNone(diagnostic.validate_statistics({"actors": 1, "committed": [1]}, False))
        with self.assertRaises(ValueError):
            diagnostic.validate_statistics(value, False)
        with self.assertRaises(ValueError):
            diagnostic.validate_statistics({}, True)

    def test_scope_joined_workers_and_version_cannot_be_forged(self):
        for key, value in (("version", True), ("version", 2), ("scope", "sample_only"),
                           ("metric", "cpu_time"), ("after_worker_join", False), ("after_worker_join", 1)):
            with self.subTest(key=key, value=value):
                forged = model()
                forged["worker_lock_statistics"][key] = value
                with self.assertRaises(ValueError):
                    diagnostic.validate_statistics(forged, True)

    def test_duplicate_missing_or_foreign_worker_is_rejected(self):
        for identity in (0, 2, True, -1):
            with self.subTest(identity=identity):
                forged = model()
                forged["worker_lock_statistics"]["workers"][1]["worker"] = identity
                with self.assertRaises(ValueError):
                    diagnostic.validate_statistics(forged, True)
        for count in (0, 1, 3):
            forged = model()
            rows = forged["worker_lock_statistics"]["workers"]
            forged["worker_lock_statistics"]["workers"] = (rows * 2)[:count]
            with self.assertRaises(ValueError):
                diagnostic.validate_statistics(forged, True)

    def test_phase_bounds_saturation_and_zero_count(self):
        for phase in diagnostic.PHASES:
            for key, value in (("acquisitions", False), ("wait_ns", -1), ("held_ns", 1 << 64),
                               ("wait_ns", 101), ("held_ns", 201),
                               ("max_wait_ns", 101), ("max_held_ns", 201), ("saturated", True),
                               ("saturated", 0), ("acquisitions", 0)):
                with self.subTest(phase=phase, key=key):
                    forged = model()
                    forged["worker_lock_statistics"]["workers"][0][phase][key] = value
                    with self.assertRaises(ValueError):
                        diagnostic.validate_statistics(forged, True)
        value = model()
        for phase in diagnostic.PHASES:
            value["worker_lock_statistics"]["workers"][0][phase] = {**dict.fromkeys(diagnostic.NUMBERS, 0), "saturated": False}
            value["worker_lock_statistics"]["workers"][1][phase]["acquisitions"] = 2
        diagnostic.validate_statistics(value, True)

    def test_phase_counts_allow_fast_paths_but_reject_impossible_counts(self):
        value = model()
        for phase in ("start", "finish"):
            for row in value["worker_lock_statistics"]["workers"]:
                row[phase] = {**dict.fromkeys(diagnostic.NUMBERS, 0), "saturated": False}
        diagnostic.validate_statistics(value, True)
        for phase in ("start", "finish"):
            forged = model()
            forged["worker_lock_statistics"]["workers"][0][phase]["acquisitions"] = 3
            with self.assertRaises(ValueError):
                diagnostic.validate_statistics(forged, True)
        diagnostic.validate_statistics({**model(), "committed": [2]}, True)
        for committed in ([], [True], [-1]):
            forged = {**model(), "committed": committed}
            with self.assertRaises(ValueError):
                diagnostic.validate_statistics(forged, True)

    def test_contention_switch_schema_and_bounds_are_explicit(self):
        value = contention_model()
        self.assertEqual(diagnostic.validate_contention(value, True), value["scheduler_contention_statistics"])
        self.assertIsNone(diagnostic.validate_contention({}, False))
        with self.assertRaises(ValueError):
            diagnostic.validate_contention(value, False)
        with self.assertRaises(ValueError):
            diagnostic.validate_contention({}, True)
        for key, invalid in (("version", True), ("scope", "worker_only"), ("after_worker_join", False)):
            forged = contention_model()
            forged["scheduler_contention_statistics"][key] = invalid
            with self.assertRaises(ValueError):
                diagnostic.validate_contention(forged, True)
        for lock in ("fast_slot", "mailbox"):
            for key, invalid in (("failed_attempts", -1), ("yield_calls", 1 << 64),
                                 ("max_wait_ns", 11), ("saturated", True),
                                 ("contended_acquisitions", 0)):
                forged = contention_model()
                forged["scheduler_contention_statistics"][lock][key] = invalid
                with self.assertRaises(ValueError):
                    diagnostic.validate_contention(forged, True)

    def test_frozen_inputs_detect_mutation_and_missing_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "source"
            path.write_bytes(b"original")
            inputs = {"source": diagnostic.checksum(path)}
            diagnostic.verify_inputs(root, inputs)
            path.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                diagnostic.verify_inputs(root, inputs)
            path.unlink()
            with self.assertRaises(OSError):
                diagnostic.verify_inputs(root, inputs)


def verify_runtime():
    runner = aot.TARGET / aot.PROFILE / "examples/beam_open_loop"
    with tempfile.TemporaryDirectory(prefix="wings-lock-cli-") as directory:
        root = Path(directory)
        trace = root / "trace"
        for suffix in (("--observe-locks", "--observe-locks"), ("--unknown",)):
            code, result = invoke([runner, "--trusted", "missing", "--measure", "hot-queue", 80000, trace, *suffix], b"", label="lock-invalid-suffix")
            if code != 2 or result != {"version": 6, "status": "invalid"} or trace.exists():
                raise ValueError("lock invalid CLI created trace")
        code, result = invoke([runner, "--trusted", "missing", "hot-queue", 10, 80000, "--observe-locks"], b"", label="lock-invalid-smoke")
        if code != 2 or result != {"version": 5, "status": "invalid"}:
            raise ValueError("smoke lock option accepted")
        trace.write_bytes(b"existing trace")
        command = [runner, "--trusted", "missing", "--measure", "hot-queue", 80000, trace, "--observe-locks"]
        result = aot.command(command, expected=3)
        if json.loads(result.stdout) != {"version": 6, "status": "oracle_error"} or trace.read_bytes() != b"existing trace":
            raise ValueError("observed measurement overwrote trace")
    print("Lock diagnostics: explicit long mode, invalid suffix/smoke and exclusive trace checked; no long run or CPU acceptance")


if __name__ == "__main__":
    if sys.argv[1:] == ["--runtime"]:
        verify_runtime()
    else:
        unittest.main()
