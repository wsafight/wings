import unittest

from verify_allocation_failures import validate


class AllocationFailureReportTests(unittest.TestCase):
    def report(self):
        return {
            "status": "ok", "fixed_heap_failures": 4, "root_failures": 1,
            "module_path_failures": 1,
            "aot_heap_failures": 7, "invocation_failures": 1, "managed_recovered_failures": 1,
            "managed_terminal_failures": 2, "host_payload_failures": 1, "reply_payload_failures": 1,
            "total_injected_failures": 19, "healthy_replies": 2, "invocation_retry_value": 8,
            "resources_released": True, "bootstrap_bytes": 232, "remaining_bytes": 232,
            "metric": "rust_requested_storage_not_rss_or_complete_resource_acceptance",
        }

    def test_valid_nonzero_baseline(self):
        validate(self.report())

    def test_each_field_is_required_and_unknown_fields_are_rejected(self):
        for key in self.report():
            value = self.report()
            del value[key]
            with self.assertRaises(ValueError, msg=key):
                validate(value)
        with self.assertRaises(ValueError):
            validate(self.report() | {"extra": 1})
        for value in (None, [], "ok", 1):
            with self.assertRaises(ValueError):
                validate(value)

    def test_wrong_counts_failed_state_and_boolean_numeric_aliases(self):
        for key, actual in self.report().items():
            wrong = not actual if type(actual) is bool else actual + 1 if type(actual) is int else "wrong"
            with self.assertRaises(ValueError, msg=key):
                validate(self.report() | {key: wrong})
            if type(actual) is int:
                for wrong in (True, False, str(actual), float(actual)):
                    with self.assertRaises(ValueError, msg=key):
                        validate(self.report() | {key: wrong})

    def test_zero_or_negative_or_mismatched_receipts(self):
        for value in (0, -1, True, 232.0):
            with self.assertRaises(ValueError):
                validate(self.report() | {"bootstrap_bytes": value, "remaining_bytes": value})
        with self.assertRaises(ValueError):
            validate(self.report() | {"remaining_bytes": 233})


if __name__ == "__main__":
    unittest.main()
