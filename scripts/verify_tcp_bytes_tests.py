import copy
import os
import unittest

from verify_tcp_bytes import frame, model, validate_report, workloads


class TcpBytesModelTests(unittest.TestCase):
    def test_fragmented_and_connection_business_values(self):
        cases = workloads()
        replies, report = model(cases[0][1])
        self.assertEqual(replies, [[(1, 3, 3, 0), (2, 3, 4, 0), (3, 3, 9, 0), (4, 3, 8, 0)]])
        self.assertEqual(report["committed"], 8)
        replies, report = model(cases[1][1])
        self.assertEqual(replies[-1], [(1, 3, 13, 0), (2, 3, 6, 0)])
        self.assertEqual(report["connections"], 2)

    def test_failure_keeps_old_state_and_rejects_next_frame(self):
        for name, expected in (("candidate-failure", 7), ("overflow", 2**63 - 1), ("underflow", -(2**63))):
            groups = next(groups for actual, groups, _ in workloads() if actual == name)
            replies, report = model(groups)
            self.assertEqual(replies[0][1:], [(2, 4, 3, 0), (3, 4, 4, 0)])
            self.assertEqual(report["committed"], expected)
            self.assertEqual((report["published"], report["failed"], report["rejected"]), (2, 1, 1))
            self.assertFalse(report["full_state_verified"])

    def test_invalid_frame_closes_only_its_connection(self):
        groups = next(groups for name, groups, _ in workloads() if name == "invalid")
        replies, report = model(groups)
        self.assertEqual(replies, [[], [], [], [(1, 3, 4, 0)]])
        self.assertEqual((report["invalid"], report["connections"], report["committed"]), (3, 4, 4))

    def test_maximum_and_i64_edges_without_failure(self):
        groups = next(groups for name, groups, _ in workloads() if name == "maximum")
        replies, report = model(groups)
        self.assertEqual(replies[-1][-1], (64, 3, 512, 0))
        self.assertEqual((report["published"], report["replies"], report["committed"]), (512, 512, 512))
        groups = [[frame(1, 2**63 - 1), frame(2, -(2**63)), frame(3, 1)]]
        self.assertEqual(model(groups)[0], [[(1, 3, 2**63 - 1, 0), (2, 3, -1, 0), (3, 3, 0, 0)]])

    def reports(self):
        groups = workloads()[0][1]
        base = model(groups)[1] | {"resources_released": True, "max_socket_buffer": 16384}
        wings = base | {"bootstrap_bytes": 232, "remaining_bytes": 232, "wake_errors": 0,
                        "socket_setup_errors": 0, "socket_buffer_rejections": 0,
                        "max_tcp_segment": 1024 if os.uname().sysname == "Darwin" else None}
        beam = base | {"gate_closed": True, "actor_stopped": True, "otp_release": "29"}
        return groups, wings, beam

    def test_valid_reports_and_nonzero_resource_baseline(self):
        groups, wings, beam = self.reports()
        validate_report(wings, groups)
        validate_report(beam, groups)

    def test_report_mutations_are_rejected(self):
        groups, wings, beam = self.reports()
        for original in (wings, beam):
            for key in original:
                missing = copy.deepcopy(original)
                del missing[key]
                with self.assertRaises((AssertionError, KeyError), msg=f"missing {key}"):
                    validate_report(missing, groups)
            for key, value in {"resources_released": False, "published": True, "replies": 3,
                               "committed": 9, "full_state_verified": 1, "max_socket_buffer": 65537,
                               "timed_out": 1, "invalid": 1}.items():
                changed = original | {key: value}
                with self.assertRaises(AssertionError, msg=f"changed {key}"):
                    validate_report(changed, groups)
        for key in ("wake_errors", "socket_setup_errors", "socket_buffer_rejections", "remaining_bytes"):
            with self.assertRaises(AssertionError):
                validate_report(wings | {key: 1}, groups)
        for key in ("gate_closed", "actor_stopped"):
            with self.assertRaises(AssertionError):
                validate_report(beam | {key: False}, groups)


if __name__ == "__main__":
    unittest.main()
