import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import verify_smoke as smoke


class IncrementalSmoke(unittest.TestCase):
    def test_smoke_uses_built_binaries_trusted_bundle_and_expected_counter_replies(self):
        with tempfile.TemporaryDirectory(prefix="wings-smoke-test-") as temporary:
            target = Path(temporary).resolve()
            for filename in ("wings-compile", "wings-vm", "examples/mvp_runtime"):
                executable = target / "debug" / filename
                executable.parent.mkdir(parents=True, exist_ok=True)
                executable.touch()
            responses = [{"reply": 0}, {"reply": 3}, {"reply": 7}, {"accepted": True},
                         {"accepted": True}, {"reply": 12}, {"reply": None}]
            counter_output = "\n".join(json.dumps(value) for value in responses)
            results = [SimpleNamespace(stdout=value) for value in ("", counter_output, "", "session passed")]
            with patch("sys.argv", ["verify_smoke.py", "--target-dir", str(target)]), patch.object(
                smoke.aot, "command", side_effect=results
            ) as command:
                smoke.main()
            self.assertEqual(command.call_count, 4)
            arguments = command.call_args_list[-1].args[0]
            self.assertEqual(arguments[1], "--trusted")
            self.assertEqual(arguments[-1], target / "debug/wings-vm")

    def test_smoke_requires_built_executables(self):
        with tempfile.TemporaryDirectory(prefix="wings-smoke-missing-") as temporary:
            with patch("sys.argv", ["verify_smoke.py", "--target-dir", temporary]):
                with self.assertRaisesRegex(ValueError, "build the smoke binaries"):
                    smoke.main()


if __name__ == "__main__":
    unittest.main()
