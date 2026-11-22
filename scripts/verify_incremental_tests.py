import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import verify_incremental as incremental
import verify_workspace as workspace


GRAPH = {
    "wings-source": set(),
    "wings-syntax": {"wings-source"},
    "wings-sema": {"wings-syntax", "wings-source"},
    "wings-abi": set(),
    "wings-artifact": {"wings-abi"},
    "wings-ir": {"wings-abi"},
    "wings-codegen": {"wings-ir", "wings-abi"},
    "wings-compile": {"wings-sema", "wings-codegen", "wings-artifact"},
    "wings-vm": {"wings-abi", "wings-artifact"},
    "wings-lsp": {"wings-sema"},
}
DIRECTORIES = {name: f"crates/{name}/" for name in GRAPH}


class IncrementalPlanning(unittest.TestCase):
    def plan(self, changes, **options):
        return incremental.make_plan(changes, GRAPH, DIRECTORIES, **options)

    def tasks(self, changes):
        return {task["name"] for task in self.plan(changes)["tasks"]}

    def test_frontend_changes_select_transitive_dependents_without_runtime_crate(self):
        plan = self.plan(["crates/wings-source/src/lib.rs"])
        self.assertEqual(set(plan["packages"]), {"wings-source", "wings-syntax", "wings-sema", "wings-compile", "wings-lsp"})
        self.assertIn("aot-smoke", {task["name"] for task in plan["tasks"]})

    def test_runtime_and_lsp_changes_stay_within_their_dependents(self):
        self.assertEqual(self.plan(["crates/wings-vm/src/scheduler/run.rs"])["packages"], ["wings-vm"])
        self.assertEqual(self.plan(["crates/wings-lsp/src/lib.rs"])["packages"], ["wings-lsp"])
        self.assertNotIn("aot-smoke", self.tasks(["crates/wings-lsp/src/lib.rs"]))

    def test_abi_changes_cover_compiler_runtime_and_artifacts(self):
        self.assertEqual(set(self.plan(["crates/wings-abi/src/lib.rs"])["packages"]),
                         {"wings-abi", "wings-ir", "wings-codegen", "wings-artifact", "wings-compile", "wings-vm"})

    def test_docs_python_and_no_changes_do_not_trigger_release_or_performance(self):
        self.assertEqual(self.tasks(["docs/evaluation.md"]), {"python", "site"})
        self.assertEqual(self.tasks(["scripts/verify_workspace.py"]), {"python"})
        self.assertEqual(self.tasks([]), set())
        plan = self.plan([], all_tasks=True)
        self.assertFalse(plan["release_gate_run"])
        self.assertFalse(plan["performance_acceptance_run"])
        commands = json.dumps(plan["tasks"])
        for forbidden in ("measure_open_loop", "measure_fairness", "+1.85.0", "verify_scalar_aot.py\""):
            self.assertNotIn(forbidden, commands)

    def test_unknown_paths_expand_and_lockfile_selects_all_packages(self):
        self.assertEqual(set(self.plan(["Cargo.lock"])["packages"]), set(GRAPH))
        plan = self.plan(["new-runtime-metadata.json"])
        self.assertEqual(set(plan["packages"]), set(GRAPH))
        self.assertTrue(plan["reasons"])
        self.assertIn("python", {task["name"] for task in plan["tasks"]})

    def test_smoke_script_changes_select_built_smoke_and_dependent_inputs(self):
        plan = self.plan(["scripts/verify_smoke.py"])
        smoke = next(task for task in plan["tasks"] if task["name"] == "aot-smoke")
        self.assertIn("--target-dir", smoke["commands"][0])
        lint = next(task for task in self.plan(["crates/wings-vm/src/lib.rs"])["tasks"] if task["name"] == "lint")
        self.assertIn("crates/wings-abi/", lint["inputs"])
        self.assertNotIn("crates/wings-source/", lint["inputs"])

    def test_workflow_policy_separates_daily_and_manual_release_checks(self):
        workflows = incremental.ROOT / ".github/workflows"
        daily = (workflows / "ci.yml").read_text()
        self.assertIn("verify_incremental.py --base", daily)
        self.assertIn("--no-cache", daily)
        self.assertNotIn("verify_scalar_aot.py", daily)
        for filename in ("release.yml", "benchmark.yml"):
            header = (workflows / filename).read_text().split("permissions:")[0]
            self.assertIn("workflow_dispatch:", header)
            self.assertNotIn("pull_request:", header)
            self.assertNotIn("push:", header)

    def test_runtime_tests_use_core_harnesses_without_starting_every_example(self):
        tests = next(task for task in self.plan(["crates/wings-vm/src/lib.rs"])["tasks"] if task["name"] == "rust-tests")
        self.assertEqual(tests["targets"], ["--lib", "--bins", "--tests"])
        tests = next(task for task in self.plan(["crates/wings-vm/examples/beam_open_loop.rs"])["tasks"] if task["name"] == "rust-tests")
        self.assertEqual(tests["targets"][-2:], ["--example", "beam_open_loop"])

    def test_environment_excludes_call_ids_but_keeps_build_and_runtime_settings(self):
        plan = {"packages": [], "tasks": []}
        with patch.dict(os.environ, {"CODEX_TOOL_CALL_ID": "first", "RUSTFLAGS": "one"}, clear=True):
            first = incremental.execution_context(plan)
        with patch.dict(os.environ, {"CODEX_TOOL_CALL_ID": "second", "RUSTFLAGS": "one"}, clear=True):
            self.assertEqual(first, incremental.execution_context(plan))
        with patch.dict(os.environ, {"CODEX_TOOL_CALL_ID": "second", "RUSTFLAGS": "two"}, clear=True):
            self.assertNotEqual(first, incremental.execution_context(plan))


class IncrementalExecution(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="wings-incremental-tests-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.git("init", "-q")
        self.git("config", "user.email", "verify@example.invalid")
        self.git("config", "user.name", "Verification")
        self.write(".gitignore", "target/\n")
        self.write("source.txt", "initial")
        self.git("add", ".")
        self.git("commit", "-qm", "initial")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=self.root, text=True)

    def write(self, filename, contents):
        source = self.root / filename
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(contents)
        return source

    def plan(self):
        task = {"name": "unit", "kind": "command", "inputs": ["source.txt", "other.txt"],
                "commands": [], "dependencies": []}
        return {"tasks": [task], "packages": [], "level": "incremental"}

    def run_plan(self, plan=None, **options):
        with patch.object(incremental, "execution_context", return_value={"tools": "fixed"}):
            return incremental.run_plan(self.root, plan or self.plan(), self.root / "target/verify", **options)

    def reports(self):
        return [json.loads(source.read_text()) for source in sorted((self.root / "target/verify/runs").glob("*/report.json"))]

    def test_git_selection_includes_committed_staged_unstaged_untracked_deleted_and_renamed(self):
        self.write("committed.txt", "commit")
        self.git("add", ".")
        self.git("commit", "-qm", "second")
        self.git("mv", "source.txt", "renamed.txt")
        self.write("staged.txt", "stage")
        self.git("add", "staged.txt")
        self.write("committed.txt", "working")
        self.write("untracked.txt", "untracked")
        self.write("target/ignored", "ignored")
        self.assertEqual(incremental.changed_files(self.root, self.base),
                         ["committed.txt", "renamed.txt", "source.txt", "staged.txt", "untracked.txt"])
        self.assertEqual(incremental.changed_files(self.root, None),
                         ["committed.txt", "renamed.txt", "source.txt", "staged.txt", "untracked.txt"])

    def test_invalid_base_fails_instead_of_skipping_changes(self):
        with self.assertRaises(subprocess.CalledProcessError):
            incremental.changed_files(self.root, "no-such-base")

    def test_fingerprint_covers_content_deletion_new_files_environment_and_mode(self):
        task = self.plan()["tasks"][0]
        fingerprint = lambda context=None: incremental.fingerprint(self.root, task, context or {})
        original = fingerprint()
        self.write("unrelated.txt", "unrelated")
        self.assertEqual(original, fingerprint())
        self.write("source.txt", "edited")
        self.assertNotEqual(original, fingerprint())
        self.write("source.txt", "initial")
        self.assertEqual(original, fingerprint())
        self.assertNotEqual(original, fingerprint({"rustc": "changed"}))
        (self.root / "source.txt").chmod(0o755)
        self.assertNotEqual(original, fingerprint())
        (self.root / "source.txt").unlink()
        self.assertNotEqual(original, fingerprint())
        self.write("other.txt", "new")
        self.assertNotEqual(original, fingerprint())

    def test_success_is_cached_and_changed_inputs_rerun(self):
        with patch.object(incremental, "execute_task") as execute:
            self.assertTrue(self.run_plan())
            self.assertTrue(self.run_plan())
            self.assertEqual(execute.call_count, 1)
            self.assertEqual(self.reports()[-1]["results"][0]["status"], "cached")
            self.write("source.txt", "changed")
            self.assertTrue(self.run_plan())
            self.assertEqual(execute.call_count, 2)

    def test_failures_interruptions_and_changed_inputs_never_cache_success(self):
        for failure in (ValueError("injected"), KeyboardInterrupt()):
            with patch.object(incremental, "execute_task", side_effect=failure):
                self.assertFalse(self.run_plan())
        self.assertEqual(list((self.root / "target/verify/cache").glob("*.json")), [])
        with patch.object(incremental, "execute_task", side_effect=lambda *arguments: self.write("source.txt", "during test")):
            self.assertFalse(self.run_plan())
        self.assertEqual(list((self.root / "target/verify/cache").glob("*.json")), [])
        self.assertEqual(self.reports()[-1]["results"][0]["status"], "inconclusive")

    def test_no_cache_and_corrupt_evidence_execute_again(self):
        with patch.object(incremental, "execute_task") as execute:
            self.assertTrue(self.run_plan())
            evidence = next((self.root / "target/verify/cache").glob("*.json"))
            evidence.write_text("not json")
            self.assertTrue(self.run_plan())
            self.assertTrue(self.run_plan(use_cache=False))
            self.assertEqual(execute.call_count, 3)

    def test_dependencies_are_required_before_execution(self):
        plan = self.plan()
        plan["tasks"][0]["dependencies"] = ["missing"]
        with patch.object(incremental, "execute_task") as execute:
            self.assertFalse(self.run_plan(plan))
            execute.assert_not_called()

    def test_new_changes_outside_selected_scope_make_overall_run_inconclusive(self):
        with patch.object(incremental, "execute_task", side_effect=lambda *arguments: self.write("new-module.rs", "changed")):
            self.assertFalse(self.run_plan())
        self.assertEqual(self.reports()[-1]["status"], "inconclusive")

    def test_lock_prevents_overlapping_runs(self):
        with incremental.exclusive_run(self.root / "lock"):
            with self.assertRaisesRegex(ValueError, "another incremental"):
                with incremental.exclusive_run(self.root / "lock"):
                    self.fail("overlapping verification acquired the lock")

    def test_runner_applies_limits_and_cleans_up_after_failure(self):
        command = [sys.executable, "-c", "import resource; assert resource.getrlimit(resource.RLIMIT_CORE) == (0,0); print('ok')"]
        with (self.root / "command.log").open("w") as log:
            incremental.run_command(command, self.root, log, timeout=5)
        self.assertEqual((self.root / "command.log").read_text(), "ok\n")
        with patch.object(incremental.subprocess, "Popen") as start, patch.object(workspace, "stop_group") as stop:
            child = start.return_value
            child.wait.side_effect = subprocess.TimeoutExpired("command", 5)
            with self.assertRaises(subprocess.TimeoutExpired):
                incremental.run_command(["command"], self.root, None, timeout=5)
            stop.assert_called_once_with(child)


if __name__ == "__main__":
    unittest.main()
