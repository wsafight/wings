import unittest

import verify_failure_soak as failure


class FailureDiagnostics(unittest.TestCase):
    def capture(self, error):
        try:
            raise error
        except (OSError, RuntimeError, ValueError, AssertionError) as caught:
            return failure.failure_diagnostic(caught)

    def test_permission_error_retains_type_message_and_origin(self):
        try:
            raise PermissionError(1, "Operation not permitted")
        except PermissionError as error:
            diagnostic = failure.failure_diagnostic(error)
        self.assertTrue(diagnostic.startswith("Failure soak verification failed:\nTraceback"))
        self.assertIn("PermissionError: [Errno 1] Operation not permitted", diagnostic)
        self.assertIn("test_permission_error_retains_type_message_and_origin", diagnostic)
        self.assertIn("raise PermissionError", diagnostic)

    def test_causal_chain_is_preserved(self):
        try:
            try:
                raise PermissionError(1, "Operation not permitted")
            except PermissionError as error:
                raise RuntimeError("memory census") from error
        except RuntimeError as error:
            diagnostic = failure.failure_diagnostic(error)
        self.assertIn("PermissionError: [Errno 1] Operation not permitted", diagnostic)
        self.assertIn("RuntimeError: memory census", diagnostic)
        self.assertIn("The above exception was the direct cause", diagnostic)

    def test_diagnostic_is_byte_bounded_without_split_encoding_errors(self):
        diagnostic = self.capture(ValueError("界" * 10000 + " final-marker"))
        prefix = "Failure soak verification failed:\n"
        self.assertTrue(diagnostic.startswith(prefix))
        self.assertLessEqual(len(diagnostic[len(prefix):].encode("utf-8")), failure.DIAGNOSTIC_BYTES)
        self.assertIn("failure-soak diagnostic truncated", diagnostic)
        self.assertTrue(diagnostic.endswith("final-marker\n"))


if __name__ == "__main__":
    unittest.main()
