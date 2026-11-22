import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import verify_workspace as workspace


SUMMARY = "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"


class WorkspaceVerification(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="wings-workspace-model-")
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()

    def executable(self, body):
        path = self.parent / "harness"
        path.write_text(f"#!{sys.executable}\n{body}\n")
        path.chmod(0o700)
        return path

    def artifact(self, path, **changes):
        return {"reason": "compiler-artifact", "profile": {"test": True}, "executable": str(path), **changes}

    def inventory(self, messages):
        return workspace.harnesses("\n".join(map(json.dumps, messages)))

    def test_inventory_requires_successful_nonempty_build_and_deduplicates(self):
        path = self.executable("print('ready')")
        artifact = self.artifact(path)
        finished = {"reason": "build-finished", "success": True}
        self.assertEqual(self.inventory([artifact, artifact, finished]), [(path, workspace.digest(path))])
        for messages in ([], [artifact], [finished], [artifact, {**finished, "success": False}],
                         [self.artifact(self.parent / "absent"), finished],
                         [self.artifact(path, profile={"test": False}), finished]):
            with self.subTest(messages=messages), self.assertRaises((OSError, ValueError)):
                self.inventory(messages)
        with self.assertRaises(ValueError):
            workspace.harnesses("not JSON")

    def test_summary_rejects_missing_duplicate_failed_and_skipped_tests(self):
        self.assertEqual(workspace.test_count(SUMMARY), 3)
        self.assertEqual(workspace.test_count(SUMMARY.replace("3 passed", "0 passed")), 0)
        for output in ("", SUMMARY + "\n" + SUMMARY, SUMMARY.replace("0 failed", "1 failed"),
                       SUMMARY.replace("0 ignored", "1 ignored"), SUMMARY.replace("0 measured", "1 measured"),
                       SUMMARY.replace("0 filtered", "1 filtered")):
            with self.subTest(output=output), self.assertRaises(ValueError):
                workspace.test_count(output)

    def test_real_harness_checks_os_limits_stdin_and_records_output(self):
        path = self.executable(
            "import resource,sys\n"
            "assert resource.getrlimit(resource.RLIMIT_CPU) == (20,20)\n"
            "assert resource.getrlimit(resource.RLIMIT_CORE) == (0,0)\n"
            "assert sys.stdin.read() == ''\n"
            f"print({SUMMARY!r})"
        )
        record = workspace.execute(path, workspace.digest(path), self.parent / "success.log")
        self.assertEqual((record["status"], record["tests"], record["exit_code"]), ("passed", 3, 0))
        self.assertIn(SUMMARY, (self.parent / "success.log").read_text())

    def test_nonzero_exit_invalid_summary_and_changed_binary_are_failures(self):
        for index, body in enumerate(("raise SystemExit(7)", "print('no summary')", "print(" + repr(SUMMARY.replace("0 ignored", "1 ignored")) + ")")):
            path = self.executable(body)
            record = workspace.execute(path, workspace.digest(path), self.parent / f"failure-{index}.log")
            self.assertEqual(record["status"], "failed")
            self.assertIsNone(record["tests"])
        path = self.executable("raise SystemExit(0)")
        checksum = workspace.digest(path)
        path.write_text(path.read_text() + "print('changed')\n")
        record = workspace.execute(path, checksum, self.parent / "changed.log")
        self.assertEqual(record["status"], "failed")
        self.assertIn("changed after build", record["error"])
        self.assertFalse((self.parent / "changed.log").exists())

    def test_timeout_keeps_original_limit_and_kills_process_group(self):
        path = self.executable("print('unused')")
        with patch.object(workspace.subprocess, "Popen") as start:
            child = start.return_value
            child.pid = 123
            child.returncode = -signal.SIGKILL
            child.wait.side_effect = [subprocess.TimeoutExpired(str(path), 30), -signal.SIGKILL]
            with patch.object(workspace.os, "killpg") as kill:
                record = workspace.execute(path, workspace.digest(path), self.parent / "timeout.log")
            child.wait.assert_any_call(timeout=30)
            kill.assert_called_once_with(123, signal.SIGKILL)
            self.assertTrue(start.call_args.kwargs["start_new_session"])
        self.assertEqual(record["status"], "timed_out")
        self.assertEqual(record["exit_code"], -signal.SIGKILL)
        self.assertIsNone(record["tests"])

    def test_reports_keep_inventory_failures_and_refuse_overwrite(self):
        path = self.executable("print('unused')")
        inventory = [self.artifact(path), {"reason": "build-finished", "success": True}]
        output = self.parent / "reports"

        def build(command, **options):
            self.assertIn("--all-targets", command)
            self.assertIn("--release", command)
            options["stdout"].write(("\n".join(map(json.dumps, inventory)) + "\n").encode())

        failure = {"status": "failed", "tests": None, "error": "injected failure"}
        with patch.object(workspace.subprocess, "run", side_effect=build), patch.object(workspace, "execute", return_value=failure):
            self.assertFalse(workspace.run("release", output))
        report = json.loads((output / "report.json").read_text())
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["error"], "injected failure")
        self.assertEqual(report["harnesses"], [failure])
        self.assertEqual(json.loads((output / "inventory.json").read_text())[0]["sha256"], workspace.digest(path))
        with self.assertRaises(FileExistsError):
            workspace.run("release", output)
        self.assertEqual(json.loads((output / "report.json").read_text()), report)

    def test_build_failure_preserves_cargo_logs_without_running_harnesses(self):
        output = self.parent / "bad-build"

        def build(command, **options):
            options["stderr"].write(b"injected build failure\n")
            raise subprocess.CalledProcessError(101, command)

        with patch.object(workspace.subprocess, "run", side_effect=build), patch.object(workspace, "execute") as execute:
            self.assertFalse(workspace.run("debug", output))
            execute.assert_not_called()
        report = json.loads((output / "report.json").read_text())
        self.assertEqual((report["status"], report["tests"], report["harnesses"]), ("failed", 0, []))
        self.assertEqual((output / "cargo.stderr").read_text(), "injected build failure\n")

    def test_success_counts_only_executed_tests_and_keeps_zero_test_harnesses(self):
        path = self.executable("print('unused')")
        inventory = [self.artifact(path), {"reason": "build-finished", "success": True}]
        output = self.parent / "success-report"

        def build(command, **options):
            self.assertNotIn("--release", command)
            options["stdout"].write(("\n".join(map(json.dumps, inventory)) + "\n").encode())

        for count in [0, 3]:
            destination = output / str(count)
            with patch.object(workspace.subprocess, "run", side_effect=build), patch.object(workspace, "execute", return_value={"status": "passed", "tests": count}):
                self.assertTrue(workspace.run("debug", destination))
            report = json.loads((destination / "report.json").read_text())
            self.assertEqual((report["status"], report["tests"], len(report["harnesses"])), ("passed", count, 1))

    def test_selected_packages_are_built_once_without_workspace_or_test_filters(self):
        path = self.executable("print('unused')")
        inventory = [self.artifact(path), {"reason": "build-finished", "success": True}]

        def build(command, **options):
            self.assertNotIn("--workspace", command)
            self.assertEqual(command.count("-p"), 2)
            self.assertEqual(command[-4:], ["-p", "wings-source", "-p", "wings-vm"])
            options["stdout"].write(("\n".join(map(json.dumps, inventory)) + "\n").encode())

        with patch.object(workspace.subprocess, "run", side_effect=build) as build_call, patch.object(
            workspace, "execute", return_value={"status": "passed", "tests": 3}
        ):
            self.assertTrue(workspace.run("debug", self.parent / "selected", ["wings-vm", "wings-source", "wings-vm"]))
            self.assertEqual(build_call.call_count, 1)

    def test_incremental_core_target_scope_does_not_change_full_runner_default(self):
        path = self.executable("print('unused')")
        inventory = [self.artifact(path), {"reason": "build-finished", "success": True}]

        def build(command, **options):
            self.assertNotIn("--all-targets", command)
            for target in ("--lib", "--bins", "--tests", "--example", "beam_open_loop"):
                self.assertIn(target, command)
            options["stdout"].write(("\n".join(map(json.dumps, inventory)) + "\n").encode())

        with patch.object(workspace.subprocess, "run", side_effect=build), patch.object(
            workspace, "execute", return_value={"status": "passed", "tests": 3}
        ):
            self.assertTrue(workspace.run("debug", self.parent / "core", ["wings-vm"],
                                          ["--lib", "--bins", "--tests", "--example", "beam_open_loop"]))


if __name__ == "__main__":
    unittest.main()
