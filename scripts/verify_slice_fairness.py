import argparse
from array import array
from collections import deque
import copy
import json
from pathlib import Path
import sys

import measure_fairness as fairness
from verify_fairness import quantiles
from verify_open_loop import integer, require


MAX_SLICES = 8000000
MAX_TRACE = fairness.MAX_TRACE + MAX_SLICES * fairness.measure.EVENT.size
FIELDS = {"capacity", "retained", "taken", "dropped", "published", "suspended", "completed", "failed", "overflowed"}
TRACE_FIELDS = {"trace_limit", "trace_emitted", "trace_dropped"}


class Slices:
    def __init__(self, offset):
        self.offset = offset
        self.sequence = 0
        self.returned = 0
        self.records = 0
        self.gaps = 0
        self.generation = 0
        self.durations = array("Q")
        self.waits = array("Q")
        self.actors = [{"actor": actor, "completed": 0, "failed": 0, "observed": 0,
                        "previous": None, "durations": array("Q"), "waits": array("Q"),
                        "omitted_waits": 0, "boundary_wait_lower_bound_ns": None}
                       for actor in range(1, 102)]

    def consume(self, record):
        sequence, began, returned, actor, kind = record
        require(all(type(value) is int for value in record), "slice integer fields")
        require(kind in (8, 9, 10) and 1 <= actor <= 101, "slice kind/identity")
        require(self.sequence < sequence and (self.records != 0 or sequence == 1), "slice sequence")
        require(self.returned <= began <= returned < fairness.STAGE_NS, "slice interval")
        require(self.records < MAX_SLICES, "slice history bound")
        missing = sequence - self.sequence - 1
        self.gaps += missing
        self.generation += bool(missing)
        group = self.actors[actor - 1]
        previous = group["previous"]
        if previous is not None and previous[1] == 8:
            if previous[2] != self.generation:
                group["omitted_waits"] += 1
            elif self.offset + fairness.WARMUP_NS <= began < self.offset + 90_000_000_000:
                delay = began - previous[0]
                self.waits.append(delay)
                group["waits"].append(delay)
            elif previous[0] < self.offset + 90_000_000_000 <= began:
                group["boundary_wait_lower_bound_ns"] = self.offset + 90_000_000_000 - previous[0]
        if self.offset + fairness.WARMUP_NS <= began < self.offset + 90_000_000_000:
            self.durations.append(returned - began)
            group["durations"].append(returned - began)
        group["previous"] = (returned, kind, self.generation)
        group["observed"] += 1
        group["completed"] += kind == 9
        group["failed"] += kind == 10
        self.records += 1
        self.sequence = sequence
        self.returned = returned

    def finish(self, statistics, committed):
        require(type(statistics) is dict and set(statistics) == FIELDS, "slice statistics fields")
        require(type(statistics["capacity"]) is int and statistics["capacity"] == 1024
                and statistics["overflowed"] is False, "slice control")
        counters = {key: integer(statistics[key]) for key in FIELDS - {"capacity", "overflowed"}}
        require(all(value <= 2**64 - 1 for value in counters.values()), "slice counter range")
        require(type(committed) is list and len(committed) == 101, "slice committed actor count")
        values = [integer(value) for value in committed]
        require(counters["retained"] == 0 and counters["taken"] == self.records, "slice stopped FIFO")
        require(counters["published"] == self.records + counters["dropped"]
                == counters["suspended"] + counters["completed"] + counters["failed"], "slice conservation")
        require(counters["completed"] == 101 + sum(values) and counters["failed"] == 0, "slice invocation totals")
        require(self.records > 0 and self.sequence <= counters["published"]
                and self.gaps + counters["published"] - self.sequence == counters["dropped"], "slice missing records")
        complete = counters["dropped"] == 0
        require(sum(actor["completed"] for actor in self.actors) <= counters["completed"]
                and sum(actor["failed"] for actor in self.actors) <= counters["failed"]
                and sum(actor["observed"] - actor["completed"] - actor["failed"] for actor in self.actors)
                <= counters["suspended"], "slice observed outcome totals")
        groups = []
        for index, actor in enumerate(self.actors):
            if complete:
                require(actor["completed"] == values[index] + 1 and actor["failed"] == 0
                        and actor["previous"][1] == 9, "slice per-actor completion history")
            groups.append({"actor": actor["actor"], "observed": actor["observed"],
                           "observed_completed": actor["completed"], "observed_failed": actor["failed"],
                           "resume_wall_time": quantiles(actor["durations"]),
                           "resumable_wait": quantiles(actor["waits"]),
                           "waits_omitted_across_loss": actor["omitted_waits"],
                           "boundary_wait_lower_bound_ns": actor["boundary_wait_lower_bound_ns"]})
        return {**counters, "complete": complete, "has_data": bool(self.durations),
                "last_retained_ns": self.returned,
                "distribution_scope": "complete" if complete else "retained subset only; not full-window quantiles",
                "resume_wall_time": quantiles(self.durations), "resumable_wait": quantiles(self.waits),
                "metric": "resume_call_wall_time_and_wait_after_suspension_not_exclusive_cpu",
                "sample_classification": "next_resume_start_in_30s_to_90s; boundary waits separate",
                "per_actor": groups}


def bounded_statistics(statistics, limit=MAX_SLICES):
    require(type(statistics) is dict and set(statistics) == FIELDS | TRACE_FIELDS, "bounded slice fields")
    trace = {key: integer(statistics[key]) for key in TRACE_FIELDS}
    require(all(value <= 2**64 - 1 for value in trace.values())
            and trace["trace_limit"] == limit and trace["trace_emitted"] <= limit, "bounded trace range")
    taken = integer(statistics["taken"])
    dropped = integer(statistics["dropped"])
    require(taken == trace["trace_emitted"] + trace["trace_dropped"]
            and (trace["trace_dropped"] == 0 or trace["trace_emitted"] == limit), "bounded trace conservation")
    normalized = {key: value for key, value in statistics.items() if key in FIELDS}
    normalized["taken"] = trace["trace_emitted"]
    normalized["dropped"] = dropped + trace["trace_dropped"]
    return normalized, {**trace, "fifo_taken": taken, "fifo_dropped": dropped}


class BoundedSlices(Slices):
    def finish(self, statistics, committed):
        normalized, trace = bounded_statistics(statistics)
        return {**super().finish(normalized, committed), **trace}


class Timeline:
    def __init__(self, offset):
        self.offset = offset
        self.enabled = True
        self.configured = [False] * 101
        self.entries = {}
        self.queues = [deque() for _ in range(101)]
        self.linked = 0

    def lose(self):
        self.enabled = False
        self.entries.clear()
        for queue in self.queues:
            queue.clear()

    def business(self, record):
        if not self.enabled:
            return
        sequence, sent, observed, _, kind = record
        if kind not in (0, 1, 2, 5, 6):
            return
        key = (kind in (5, 6), sequence)
        actor = 0 if key[0] else 1 + (sequence - 1) % 100
        if kind in (0, 5):
            require(key not in self.entries, "slice duplicate linked admission")
            entry = {"key": key, "sent": sent + self.offset, "observed": None, "returned": None}
            self.entries[key] = entry
            self.queues[actor].append(entry)
            require(len(self.entries) <= 1280, "slice bounded business links")
        else:
            require(key in self.entries, "slice unknown linked result")
            entry = self.entries[key]
            entry["observed"] = observed + self.offset
            self.release(entry)

    def release(self, entry):
        if entry["observed"] is not None and entry["returned"] is not None:
            require(entry["returned"] <= entry["observed"], "slice returned after host result")
            self.entries.pop(entry["key"])
            self.linked += 1

    def slice(self, record):
        if not self.enabled:
            return
        _, began, returned, identity, kind = record
        actor = identity - 1
        require(kind in (8, 9), "slice unexpected business failure")
        if not self.configured[actor]:
            require(returned <= self.offset, "slice Configure after business origin")
            self.configured[actor] = kind == 9
            return
        require(bool(self.queues[actor]), "slice executed without admitted business")
        entry = self.queues[actor][0]
        require(entry["sent"] <= began, "slice executed before admission")
        if kind == 9:
            self.queues[actor].popleft()
            entry["returned"] = returned
            self.release(entry)

    def finish(self, complete):
        if complete:
            require(self.enabled and all(self.configured) and not self.entries
                    and not any(self.queues), "slice incomplete business clock links")
        return {"business_clock_links_complete": complete and self.enabled,
                "linked_business_completions_in_verified_prefix": self.linked}


class Replay(fairness.Replay):
    def __init__(self, offset=0):
        super().__init__(offset)
        self.slices = Slices(offset)
        self.timeline = Timeline(offset)

    def consume(self, record):
        if record[4] in (8, 9, 10):
            self.slices.consume(record)
            if self.slices.gaps and self.timeline.enabled:
                self.timeline.lose()
            self.timeline.slice(record)
        else:
            super().consume(record)
            self.timeline.business(record)

    def finish(self, result):
        summary = super().finish(result)
        summary["slice_observation"] = self.slices.finish(result.get("slice_observation"), result["committed"])
        summary["slice_observation"].update(self.timeline.finish(summary["slice_observation"]["complete"]))
        return summary


class BoundedReplay(Replay):
    def __init__(self, offset, statistics):
        super().__init__(offset)
        self.slices = BoundedSlices(offset)
        _, self.trace = bounded_statistics(statistics)

    def consume(self, record):
        super().consume(record)
        if self.timeline.enabled and self.trace["trace_dropped"] and self.slices.records == self.trace["trace_emitted"]:
            self.timeline.lose()


def validate_trace(path, result):
    require(result.get("environment", {}).get("runtime") == "wings", "slice runtime")
    version = result.get("version")
    require(type(version) is int and version in (12, 13), "slice protocol version")
    factory = (lambda offset: BoundedReplay(offset, result.get("slice_observation"))) if version == 13 else Replay
    return fairness.validate_trace(path, result, version=version, replay_factory=factory, max_trace=MAX_TRACE)


def fixture():
    records = [[actor, actor * 2, actor * 2 + 1, actor, 9] for actor in range(1, 102)]
    records += [[102, 32_000_000_000, 32_001_000_000, 1, 8],
                [103, 32_002_000_000, 32_003_000_000, 2, 9],
                [104, 32_004_000_000, 32_005_000_000, 1, 9]]
    statistics = {"capacity": 1024, "retained": 0, "taken": 104, "dropped": 0, "published": 104,
                  "suspended": 1, "completed": 103, "failed": 0, "overflowed": False}
    committed = [1, 1] + [0] * 99
    return records, statistics, committed


def replay_model(records, statistics, committed):
    replay = Slices(1_000_000_000)
    for record in records:
        replay.consume(record)
    return replay.finish(statistics, committed)


def self_test():
    records, statistics, committed = fixture()
    summary = replay_model(records, statistics, committed)
    require(summary["complete"] is True and summary["resume_wall_time"]["samples"] == 3
            and summary["resume_wall_time"]["p99_ns"] == 1_000_000
            and summary["resumable_wait"]["samples"] == 1
            and summary["resumable_wait"]["max_ns"] == 3_000_000, "slice model intervals")
    require(summary["per_actor"][2]["resume_wall_time"]["p99_ns"] is None, "slice zero samples")
    for removed in (101, 103):
        subset = records[:removed] + records[removed + 1:]
        lossy = {**statistics, "taken": 103, "dropped": 1}
        summary = replay_model(subset, lossy, committed)
        require(summary["complete"] is False and summary["resumable_wait"]["samples"] == 0, "slice honest loss")
    boundary = copy.deepcopy(records)
    boundary[-1][1:3] = [92_000_000_000, 92_001_000_000]
    summary = replay_model(boundary, statistics, committed)
    require(summary["per_actor"][0]["boundary_wait_lower_bound_ns"] == 58_999_000_000, "slice boundary censoring")
    invalid = []
    for index, field, value in [(0, 0, 2), (2, 0, 2), (0, 0, True), (0, 1, -1),
                                (2, 1, 1), (0, 2, 0), (0, 2, fairness.STAGE_NS),
                                (0, 3, 0), (0, 3, 102), (0, 3, True), (0, 4, 7), (103, 4, 8)]:
        changed = copy.deepcopy(records)
        changed[index][field] = value
        invalid.append((changed, statistics, committed))
    invalid += [(records[:-1], statistics, committed), (records[1:], statistics, committed)]
    for field, value in [("capacity", True), ("capacity", 64), ("retained", 1), ("taken", 103),
                         ("dropped", 1), ("published", 105), ("suspended", 2), ("completed", 104),
                         ("failed", 1), ("overflowed", True)]:
        invalid.append((records, {**statistics, field: value}, committed))
    invalid.append((records, {**statistics, "extra": 1}, committed))
    invalid.append((records, statistics, [2, 0] + [0] * 99))
    changed = copy.deepcopy(records)
    changed[102][4] = 10
    invalid.append((changed[:101] + changed[102:], {**statistics, "taken": 103, "dropped": 1}, committed))
    for case in invalid:
        try:
            replay_model(*case)
        except (AssertionError, ValueError):
            continue
        raise AssertionError("forged slice history accepted")
    for invalid_clock in (None, "admission", "result", "missing"):
        timeline = Timeline(1_000_000_000)
        for record in records[:101]:
            timeline.slice(record)
        try:
            if invalid_clock != "missing":
                timeline.business([1, 31_000_000_001 if invalid_clock == "admission" else 31_000_000_000, 0, 0, 5])
            timeline.slice(records[101])
            timeline.business([1, 31_001_500_000, 0, 0, 0])
            timeline.business([1, 31_001_500_000, 31_003_500_000, 1, 1])
            timeline.slice(records[102])
            timeline.business([1, 31_000_000_000, 31_004_500_000 if invalid_clock == "result" else 31_006_000_000, 1000000, 6])
            timeline.slice(records[103])
            require(timeline.finish(True)["linked_business_completions_in_verified_prefix"] == 2, "slice business links")
        except (AssertionError, ValueError):
            require(invalid_clock is not None, "valid business clocks rejected")
        else:
            require(invalid_clock is None, "forged business clocks accepted")
    bounded = {**statistics, "trace_limit": 104, "trace_emitted": 104, "trace_dropped": 2,
               "taken": 106, "published": 106, "completed": 104, "suspended": 2}
    normalized, trace = bounded_statistics(bounded, 104)
    summary = replay_model(records, normalized, [2, 1] + [0] * 99)
    require(summary["complete"] is False and summary["dropped"] == 2 and trace["fifo_dropped"] == 0,
            "bounded retained-prefix model")
    both = {**bounded, "dropped": 1, "published": 107, "suspended": 3}
    normalized, trace = bounded_statistics(both, 104)
    require(replay_model(records, normalized, [2, 1] + [0] * 99)["dropped"] == 3
            and trace["fifo_dropped"] == 1, "bounded distinct loss causes")
    no_loss = {**statistics, "trace_limit": MAX_SLICES, "trace_emitted": 104, "trace_dropped": 0}
    require(BoundedReplay(1_000_000_000, no_loss).trace["trace_dropped"] == 0, "bounded zero loss")
    require(bounded_statistics(no_loss)[0] == statistics, "bounded unchanged full history")
    bounded_invalid = [{**bounded, field: value} for field, value in
                       (("trace_limit", 105), ("trace_emitted", 103), ("trace_emitted", 105),
                        ("trace_dropped", 1), ("trace_dropped", True), ("taken", 105),
                        ("trace_limit", 2**64), ("trace_emitted", -1))]
    for case in bounded_invalid:
        try:
            bounded_statistics(case, 104)
        except (AssertionError, ValueError):
            continue
        raise AssertionError("forged bounded trace counters accepted")
    prefix = BoundedReplay(1_000_000_000, {**no_loss, "trace_emitted": MAX_SLICES,
                                          "trace_dropped": 1, "taken": MAX_SLICES + 1})
    prefix.slices.records = MAX_SLICES
    prefix.consume([1, 0, 0, 0, 5])
    require(prefix.timeline.enabled is False and not prefix.timeline.entries, "bounded prefix closes links")
    print(f"Resume fairness model: clock links, interval/loss boundaries, bounded retention and {len(invalid) + 3 + len(bounded_invalid)} forged histories rejected; not runtime acceptance")


def main():
    parser = argparse.ArgumentParser(description="Independently replay v12 resume fairness diagnostics")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--trace", type=Path)
    parser.add_argument("--result", type=Path)
    arguments = parser.parse_args()
    if arguments.self_test:
        require(arguments.trace is None and arguments.result is None, "self-test arguments")
        self_test()
    else:
        require(arguments.trace is not None and arguments.result is not None, "trace/result required")
        require(arguments.result.stat().st_size <= 65536, "result size")
        print(json.dumps(validate_trace(arguments.trace, json.loads(arguments.result.read_text())), indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, AssertionError) as error:
        print(f"Resume fairness replay failed: {error}", file=sys.stderr)
        sys.exit(3)
