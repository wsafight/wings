import json
import os
import re
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

import verify_scalar_aot as aot


class TcpStartupVerification(unittest.TestCase):
    def child(self, source):
        child = subprocess.Popen(
            [sys.executable, "-u", "-c", source],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
            start_new_session=True, preexec_fn=aot.limit_child,
        )

        def close():
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)
            child.stdout.close()
            child.stderr.close()

        self.addCleanup(close)
        return child

    def failure(self, source, reason, timeout=1):
        child = self.child(source)
        started = time.monotonic()
        with self.assertRaises(AssertionError) as raised:
            aot.tcp_ready(child, timeout=timeout)
        message = str(raised.exception)
        self.assertIn(reason, message)
        self.assertIn("elapsed=", message)
        self.assertIn("exit_before_cleanup=", message)
        self.assertIn("stderr_prefix=", message)
        self.assertIsNotNone(child.poll())
        self.assertLess(time.monotonic() - started, timeout + 5)
        return message

    def enable_sampling(self):
        previous = aot.TIMEOUT_SAMPLE
        aot.TIMEOUT_SAMPLE = True
        self.addCleanup(setattr, aot, "TIMEOUT_SAMPLE", previous)

    def test_ready_line_does_not_consume_the_final_report(self):
        child = self.child('print(\'{"listening":"127.0.0.1:7"}\'); print(\'{"done":true}\')')
        self.assertEqual(aot.tcp_ready(child), {"listening": "127.0.0.1:7"})
        output, errors = child.communicate(timeout=5)
        self.assertEqual(json.loads(output), {"done": True})
        self.assertEqual((child.returncode, errors), (0, b""))

    def test_silence_and_partial_lines_keep_the_startup_deadline(self):
        for prefix in ("", 'print(\'{"listening":\', end=""); '):
            with self.subTest(prefix=prefix):
                message = self.failure(prefix + "import time; time.sleep(10)", "deadline", 0.2)
                if prefix:
                    self.assertIn('{"listening":', message)

    def test_early_exit_preserves_stdout_stderr_and_exit_status(self):
        message = self.failure(
            'import sys; print("partial", end=""); print("load failed", file=sys.stderr); sys.exit(7)',
            "EOF before readiness",
        )
        self.assertIn("partial", message)
        self.assertIn("load failed", message)
        self.assertIn("exit=7;", message)
        self.failure("pass", "EOF before readiness")

    def test_invalid_json_utf8_and_schema_fail_with_evidence(self):
        for source in (
            'print("not-json")',
            'import os; os.write(1, b"\\xff\\n")',
            'print("[]")',
            'print(\'{"listening":7}\')',
        ):
            with self.subTest(source=source):
                self.failure(source, "TCP startup failed")

    def test_oversized_readiness_and_stderr_have_bounded_evidence(self):
        message = self.failure('import os; os.write(1, b"x" * 5000)', "exceeds 4096 bytes")
        self.assertLess(len(message), 5000)
        message = self.failure(
            'import os; os.write(2, b"z" * 1000000)', "deadline", 0.2,
        )
        self.assertLess(len(message), 5000)

    def test_timeout_sampling_is_absent_by_default(self):
        message = self.failure("import time; time.sleep(10)", "deadline", 0.2)
        self.assertNotIn("sample_exit=", message)

    @unittest.skipUnless(sys.platform == "darwin", "requires macOS /usr/bin/sample")
    def test_timeout_sampling_handles_a_live_stuck_process(self):
        self.enable_sampling()
        message = self.failure("import time; time.sleep(10)", "deadline", 0.2)
        if "sample_exit=0" in message:
            match = re.search(r"sample_file=([^;]+); sample_bytes=(\d+)", message)
            self.assertIsNotNone(match, message)
            self.assertGreater(int(match.group(2)), 0)
            report = Path(match.group(1)).read_text(errors="replace")
            self.assertIn("Call graph", report)
            self.assertIn("sleep", report)
        else:
            # The native sampler has its own 3.5-second deadline, which a slow
            # host can reach. That bounded outcome must not mask the original
            # startup failure or leave the stuck process alive.
            self.assertIn("sample_exit=sampler wall deadline exceeded", message)
            self.assertIn("sample_stderr_prefix=", message)
        self.assertNotIn("readiness_completed_during_sampling", message)

    @unittest.skipUnless(sys.platform == "darwin", "requires macOS /usr/bin/sample")
    def test_timeout_sampling_notices_late_readiness(self):
        self.enable_sampling()
        message = self.failure(
            'import time; time.sleep(0.8); print(\'{"listening":"127.0.0.1:9"}\')',
            "deadline", 0.2,
        )
        self.assertIn("readiness_completed_during_sampling=true", message)

    def test_timeout_sampling_skips_already_exited_processes(self):
        self.enable_sampling()
        message = self.failure("pass", "EOF before readiness")
        self.assertIn("sample_skipped=already exited before sampling", message)

    def test_pipe_holding_descendant_is_terminated_on_failure(self):
        child = self.child(
            'import subprocess, sys; '
            'process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(10)"]); '
            'print(process.pid); sys.stdout.flush()'
        )
        descendant = int(child.stdout.readline())
        with self.assertRaisesRegex(AssertionError, "deadline"):
            aot.tcp_ready(child, timeout=0.2)
        ended = time.monotonic() + 2
        while time.monotonic() < ended:
            result = subprocess.run(
                ["ps", "-o", "stat=", "-p", str(descendant)], capture_output=True, text=True,
                timeout=1,
            )
            if not result.stdout.strip() or result.stdout.strip().startswith("Z"):
                break
            time.sleep(0.01)
        else:
            self.fail("TCP startup left a pipe-holding descendant running")


class CommandTimeoutSampling(unittest.TestCase):
    def enable_sampling(self):
        previous = aot.TIMEOUT_SAMPLE
        aot.TIMEOUT_SAMPLE = True
        self.addCleanup(setattr, aot, "TIMEOUT_SAMPLE", previous)

    def test_default_timeout_behavior_does_not_start_the_sampler(self):
        with mock.patch.object(aot, "sample_timeout_stacks") as sampler:
            with self.assertRaises(subprocess.TimeoutExpired):
                aot.command([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.2)
        sampler.assert_not_called()

    def test_opt_in_timeout_samples_before_bounded_group_cleanup(self):
        self.enable_sampling()
        started = time.monotonic()
        with mock.patch.object(aot, "sample_timeout_stacks", return_value="sample_exit=test") as sampler:
            with self.assertRaises(AssertionError) as raised:
                aot.command(
                    [sys.executable, "-u", "-c",
                     'import sys, time; print("partial"); print("stuck", file=sys.stderr); time.sleep(10)'],
                    timeout=0.2,
                )
        message = str(raised.exception)
        self.assertIn("timed out after 0.2 seconds", message)
        self.assertIn("exit_before_cleanup=None", message)
        self.assertIn(f"exit={-signal.SIGKILL}", message)
        self.assertIn("sample_exit=test", message)
        self.assertIn("partial", message)
        self.assertIn("stuck", message)
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(sampler.call_count, 1)
        self.assertIsNotNone(sampler.call_args.args[0].poll())

    def test_sampler_failure_does_not_replace_timeout_or_skip_cleanup(self):
        self.enable_sampling()
        with mock.patch.object(aot, "sample_timeout_stacks", side_effect=OSError("denied")):
            with self.assertRaises(AssertionError) as raised:
                aot.command([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.2)
        message = str(raised.exception)
        self.assertIn("timed out after 0.2 seconds", message)
        self.assertIn("sample_error=OSError('denied')", message)
        self.assertIn(f"exit={-signal.SIGKILL}", message)

    def test_opt_in_success_and_expected_exit_keep_the_command_contract(self):
        self.enable_sampling()
        with mock.patch.object(aot, "sample_timeout_stacks") as sampler:
            result = aot.command([sys.executable, "-c", 'print("ok")'])
            failure = aot.command([sys.executable, "-c", "raise SystemExit(7)"], expected=7)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "ok\n", ""))
        self.assertEqual(failure.returncode, 7)
        sampler.assert_not_called()


if __name__ == "__main__":
    unittest.main()
