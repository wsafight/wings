import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import unittest

import verify_multi_counter as counter
import verify_scalar_aot as aot


class MultiCounterCapture(unittest.TestCase):
    def invoke(self, source, payload=b"", timeout=1):
        return counter.invoke([sys.executable, "-u", "-c", source], payload, label="capture-test", timeout=timeout)

    def failure(self, source, payload=b"", timeout=1):
        started = time.monotonic()
        with self.assertRaises(RuntimeError) as raised:
            self.invoke(source, payload, timeout)
        self.assertLess(time.monotonic() - started, timeout + 2)
        message = str(raised.exception)
        self.assertLess(len(message), 65536)
        result = json.loads(message.removeprefix("multi Counter capture failed: "))
        self.assertEqual(result["label"], "capture-test")
        self.assertEqual(result["input_sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(result["input_bytes"], len(payload))
        self.assertIsNotNone(result["exit_after_cleanup"])
        return result

    def test_valid_and_nonzero_protocol_exits_preserve_results(self):
        for code in (0, 1, 2):
            with self.subTest(code=code):
                result = self.invoke(f'import json, sys; data = sys.stdin.buffer.read(); print(json.dumps({{"size": len(data)}})); sys.exit({code})', b"x" * 65537)
                self.assertEqual(result, (code, {"size": 65537}))

    def test_timeout_retains_both_prefixes_and_input_identity(self):
        result = self.failure('import sys, time; sys.stdin.buffer.read(); print("partial"); print("load failed", file=sys.stderr); time.sleep(10)', b"sample", 0.2)
        self.assertIn("deadline", result["error"])
        self.assertEqual(result["input_written_bytes"], 6)
        self.assertEqual(result["input_header_hex"], b"sample".hex())
        self.assertIsNone(result["exit_before_cleanup"])
        self.assertEqual(result["exit_after_cleanup"], -9)
        self.assertEqual(result["stdout_prefix"], "partial\n")
        self.assertEqual(result["stderr_prefix"], "load failed\n")
        self.assertFalse(result["stdout_eof"])

    def test_closed_output_does_not_remove_the_process_deadline(self):
        result = self.failure('import os, time; os.close(1); os.close(2); time.sleep(10)', timeout=0.2)
        self.assertTrue(result["stdout_eof"] and result["stderr_eof"])
        self.assertEqual(result["error_type"], "TimeoutExpired")
        self.assertEqual(result["exit_after_cleanup"], -9)

    def test_oversized_output_is_rejected_before_full_capture(self):
        code, value = self.invoke('import os; os.write(1, b\'"\' + b"x" * ((1 << 20) - 3) + b\'"\\n\')')
        self.assertEqual(code, 0)
        self.assertEqual(len(value), (1 << 20) - 3)
        for descriptor, maximum, name in ((1, 1 << 20, "stdout"), (2, 65536, "stderr")):
            with self.subTest(name=name):
                result = self.failure(f'import os, time; os.write({descriptor}, b"z" * {maximum + 65536}); time.sleep(10)')
                self.assertIn(f"{name} exceeds", result["error"])
                self.assertGreater(result[f"{name}_observed_bytes"], maximum)
                self.assertLessEqual(result[f"{name}_observed_bytes"], maximum + 65536)
                self.assertEqual(result[f"{name}_prefix"], "z" * 1024)
                self.assertTrue(result[f"{name}_prefix_truncated"])

    def test_invalid_json_utf8_and_stderr_retain_exit_status(self):
        for source, expected in [('print("not-json"); raise SystemExit(7)', "not-json"),
                                 ('print("not-json")' + " " * 5000, "not-json"),
                                 ('import os; os.write(1, b"\\xff")', "\\xff"),
                                 ('import sys; print("{}"); print("problem", file=sys.stderr)', "{}")]:
            with self.subTest(source=source):
                result = self.failure(source)
                self.assertEqual(result["stdout_prefix"].strip(), expected)
                self.assertIsNotNone(result["exit_before_cleanup"])
                self.assertTrue(result["stdout_eof"] and result["stderr_eof"])
                self.assertEqual(result["command_truncated"], len(source) > 512)
                if "problem" in source:
                    self.assertEqual(result["stderr_prefix"], "problem\n")

    @unittest.skipUnless(hasattr(os, "fork"), "requires fork")
    def test_terminated_parent_with_pipe_holding_descendant_is_cleaned_up(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "survived"
            source = ('import os, time; child = os.fork(); '
                      f'os._exit(0) if child else None; time.sleep(0.6); open({str(marker)!r}, "w").write("alive")')
            result = self.failure(source, timeout=0.2)
            self.assertEqual(result["exit_before_cleanup"], 0)
            time.sleep(0.7)
            self.assertFalse(marker.exists())

    def test_original_limits_cannot_be_raised(self):
        for timeout, payload in ((11, b""), (0, b""), (1, b"x" * 65538)):
            with self.subTest(timeout=timeout):
                with self.assertRaises(ValueError):
                    self.invoke("raise SystemExit(99)", payload, timeout)

    def sampled_failure(self, source, timeout=0.2):
        previous = aot.TIMEOUT_SAMPLE
        aot.TIMEOUT_SAMPLE = True
        self.addCleanup(setattr, aot, "TIMEOUT_SAMPLE", previous)
        started = time.monotonic()
        with self.assertRaises(RuntimeError) as raised:
            self.invoke(source, timeout=timeout)
        self.assertLess(time.monotonic() - started, timeout + 6)
        result = json.loads(str(raised.exception).removeprefix("multi Counter capture failed: "))
        self.assertEqual(result["label"], "capture-test")
        self.assertIsNotNone(result["exit_after_cleanup"])
        return result

    def test_timeout_sampling_is_absent_by_default(self):
        result = self.failure("import time; time.sleep(10)", timeout=0.2)
        self.assertNotIn("timeout_sampling", result)

    @unittest.skipUnless(sys.platform == "darwin", "requires macOS /usr/bin/sample")
    def test_timeout_sampling_handles_a_live_stuck_process(self):
        result = self.sampled_failure("import time; time.sleep(10)")
        sampling = result["timeout_sampling"]
        if "sample_exit=0" in sampling:
            match = re.search(r"sample_file=([^;]+); sample_bytes=(\d+)", sampling)
            self.assertIsNotNone(match, sampling)
            self.assertGreater(int(match.group(2)), 0)
            self.assertIn("multi-counter-", match.group(1))
            report = Path(match.group(1)).read_text(errors="replace")
            self.assertIn("Call graph", report)
            self.assertIn("sleep", report)
        else:
            # A bounded sampler timeout is a valid diagnostic outcome on a slow
            # host; the original timeout evidence and cleanup must still hold.
            self.assertIn("sample_exit=sampler wall deadline exceeded", sampling)
            self.assertIn("sample_stderr_prefix=", sampling)
        self.assertEqual(result["exit_after_cleanup"], -9)
        self.assertEqual(result["output_bytes_during_sampling"], {"stdout": 0, "stderr": 0})

    @unittest.skipUnless(sys.platform == "darwin", "requires macOS /usr/bin/sample")
    def test_timeout_sampling_retains_late_output(self):
        result = self.sampled_failure(
            'import sys, time; print("early"); sys.stdout.flush(); time.sleep(0.8); '
            'print("late"); sys.stdout.flush(); time.sleep(10)'
        )
        self.assertRegex(result["timeout_sampling"], r"sample_exit=(0|sampler wall deadline exceeded)")
        self.assertEqual(result["stdout_prefix"], "early\nlate\n")
        self.assertEqual(result["output_bytes_during_sampling"], {"stdout": 5, "stderr": 0})

    def test_timeout_sampling_skips_already_exited_processes(self):
        result = self.sampled_failure("raise SystemExit(7)")
        self.assertIn("sample_skipped=already exited before sampling", result["timeout_sampling"])
        self.assertEqual(result["exit_before_cleanup"], 7)


if __name__ == "__main__":
    unittest.main()
