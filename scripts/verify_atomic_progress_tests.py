import copy
import unittest

import verify_atomic_progress as probe


def model(*, dropped=0):
    cases = []
    for workers in (1, 2):
        count = 256 if dropped else 2
        observations = []
        for worker in range(workers):
            observations.append({
                "worker": worker, "samples": [[index + 1, worker, 102 + index * 2, 105 + index * 2]
                                             for index in range(count)],
                "published": count + dropped, "taken": count, "dropped": dropped,
                "requests": count + dropped + 3, "coalesced": 2, "consumed": count + dropped,
                "cleared": 1, "confirmed": count + dropped,
            })
        cases.append({
            "workers": workers, "hot_actors": workers, "healthy_actors": 8, "rounds": 16,
            "slice_steps": probe.MAXIMUM, "quantum_ns": 1_000_000, "sample_capacity": 256,
            "started_ns": 100, "heartbeat_end_ns": 1400, "stopped_ns": 1500, "finished_ns": 1550,
            "heartbeats": [[index % 8, index // 8 + 1, 110 + index * 10, 115 + index * 10]
                           for index in range(128)],
            "observations": observations, "hot_committed": [0] * workers, "healthy_committed": [16] * 8,
            "replacement_committed": 0,
            "stale_identities_rejected": True, "resources_released": True,
            "bootstrap_bytes": 232, "remaining_bytes": 232,
        })
    return {"version": 1, "status": "ok", "metric": probe.METRIC, "cases": cases}


class AtomicProgress(unittest.TestCase):
    def reject(self, path, replacement):
        result = model()
        parent = result
        for component in path[:-1]:
            parent = parent[component]
        parent[path[-1]] = replacement
        with self.subTest(path=path, replacement=replacement):
            with self.assertRaises(ValueError):
                probe.validate(result)

    def test_valid_overlap_and_explicit_tail_drops(self):
        for dropped in (0, 1, 20):
            result = model(dropped=dropped)
            self.assertIs(probe.validate(result), result)
        result = model()
        result["cases"][0]["observations"][0]["samples"][1][1] = 1
        probe.validate(result)
        result["cases"][0]["observations"][0]["samples"][1][1] = 9
        probe.validate(result)

    def test_protocol_and_field_sets_are_exact(self):
        for key, replacement in (("version", True), ("version", 2), ("status", "failed"),
                                 ("metric", "fairness_acceptance"), ("cases", [])):
            self.reject((key,), replacement)
        for path in ((), ("cases", 0), ("cases", 0, "observations", 0)):
            result = model()
            parent = result
            for component in path:
                parent = parent[component]
            for field in list(parent):
                removed = parent.pop(field)
                with self.subTest(path=path, missing=field), self.assertRaises(ValueError):
                    probe.validate(result)
                parent[field] = removed
            parent["extra"] = 0
            with self.assertRaises(ValueError):
                probe.validate(result)

    def test_fixed_configuration_cannot_be_weakened(self):
        for key in ("workers", "hot_actors", "healthy_actors", "rounds", "slice_steps", "quantum_ns", "sample_capacity"):
            for replacement in (True, -1, 0, "1", None, probe.MAXIMUM + 1):
                self.reject(("cases", 0, key), replacement)
        self.reject(("cases", 0, "slice_steps"), 1)
        self.reject(("cases", 0, "observations"), [])
        for replacement in (0, 2, True, -1):
            self.reject(("cases", 1, "observations", 1, "worker"), replacement)
        self.reject(("cases",), list(reversed(model()["cases"])))

    def test_heartbeats_are_complete_serial_and_ordered(self):
        for replacement in ([], model()["cases"][0]["heartbeats"][:-1], None):
            self.reject(("cases", 0, "heartbeats"), replacement)
        for column, replacement in ((0, True), (0, 8), (1, 0), (1, 2), (2, 99), (2, 116), (3, 109), (3, 1401)):
            self.reject(("cases", 0, "heartbeats", 0, column), replacement)
        self.reject(("cases", 0, "heartbeats", 1), model()["cases"][0]["heartbeats"][0])
        self.reject(("cases", 0, "heartbeats", 1, 2), 114)

    def test_stages_and_deadline_are_bounded(self):
        for key in ("started_ns", "heartbeat_end_ns", "stopped_ns", "finished_ns"):
            for replacement in (True, -1, None, probe.DEADLINE_NS, probe.MAXIMUM):
                self.reject(("cases", 0, key), replacement)
        self.reject(("cases", 0, "started_ns"), 1500)
        self.reject(("cases", 0, "heartbeat_end_ns"), 1501)
        self.reject(("cases", 0, "stopped_ns"), 1399)
        self.reject(("cases", 0, "finished_ns"), 1499)

    def test_yield_clocks_and_actor_identity_are_checked(self):
        for column, replacement in ((0, 0), (0, 2), (1, True), (1, 10), (2, 99), (2, 106), (3, 101), (3, 1501)):
            self.reject(("cases", 0, "observations", 0, "samples", 0, column), replacement)
        self.reject(("cases", 1, "observations", 0, "samples", 1, 1), 1)
        self.reject(("cases", 0, "observations", 0, "samples", 0, 1), 1)
        self.reject(("cases", 0, "observations", 0, "samples", 0, 3), 111)
        self.reject(("cases", 0, "observations", 0, "samples", 1, 2), 101)
        self.reject(("cases", 0, "observations", 0, "samples", 1, 3), 104)

    def test_fifo_and_counters_conserve_without_overflow(self):
        for key in probe.COUNTERS:
            for replacement in (True, -1, 0.0, "2", None, probe.MAXIMUM + 1):
                self.reject(("cases", 0, "observations", 0, key), replacement)
            self.reject(("cases", 0, "observations", 0, key), model()["cases"][0]["observations"][0][key] + 1)
        self.reject(("cases", 0, "observations", 0, "samples"), [])
        result = model()
        result["cases"][0]["observations"][0].update(published=3, dropped=1, confirmed=3, consumed=3, requests=6)
        with self.assertRaises(ValueError):
            probe.validate(result)
        result = model(dropped=1)
        sample = copy.deepcopy(result["cases"][0]["observations"][0]["samples"][-1])
        sample[0] += 1
        result["cases"][0]["observations"][0]["samples"].append(sample)
        with self.assertRaises(ValueError):
            probe.validate(result)

    def test_committed_state_and_cleanup_are_exact(self):
        for key, replacement in (("hot_committed", [99]), ("hot_committed", [False]),
                                 ("healthy_committed", [True] * 8), ("healthy_committed", [15] * 8),
                                 ("healthy_committed", [16] * 7), ("stale_identities_rejected", 1),
                                 ("replacement_committed", False), ("replacement_committed", 16),
                                 ("resources_released", False), ("bootstrap_bytes", 0),
                                 ("remaining_bytes", 231), ("remaining_bytes", True)):
            self.reject(("cases", 0, key), replacement)


if __name__ == "__main__":
    unittest.main()
