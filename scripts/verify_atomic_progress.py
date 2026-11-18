import argparse
import json
from pathlib import Path

from verify_resource_receipt import validate as validate_resource_receipt


METRIC = "short_atomic_progress_not_fairness_or_performance_acceptance"
MAXIMUM = (1 << 64) - 1
DEADLINE_NS = 3_000_000_000
CASE_FIELDS = {
    "workers", "hot_actors", "healthy_actors", "rounds", "slice_steps", "quantum_ns",
    "sample_capacity", "started_ns", "heartbeat_end_ns", "stopped_ns", "finished_ns", "heartbeats",
    "observations", "hot_committed", "healthy_committed", "stale_identities_rejected",
    "replacement_committed",
    "resources_released", "bootstrap_bytes", "remaining_bytes",
}
COUNTERS = {"published", "taken", "dropped", "requests", "coalesced", "consumed", "cleared", "confirmed"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, lower=0, upper=MAXIMUM):
    require(type(value) is int and lower <= value <= upper, "invalid atomic progress integer")
    return value


def fields(value, expected):
    require(type(value) is dict and set(value) == expected, "invalid atomic progress fields")


def vector(value, length):
    require(type(value) is list and len(value) == length, "invalid atomic progress vector")
    return value


def validate_case(case, workers):
    fields(case, CASE_FIELDS)
    for key, expected in {
        "workers": workers, "hot_actors": workers, "healthy_actors": 8, "rounds": 16,
        "slice_steps": MAXIMUM, "quantum_ns": 1_000_000, "sample_capacity": 256,
    }.items():
        require(integer(case[key]) == expected, f"changed atomic progress {key}")
    started, heartbeat_end, stopped, finished = [integer(case[key], upper=DEADLINE_NS - 1)
        for key in ("started_ns", "heartbeat_end_ns", "stopped_ns", "finished_ns")]
    require(started <= heartbeat_end <= stopped <= finished, "invalid atomic progress stages")
    previous = started
    for index, heartbeat in enumerate(vector(case["heartbeats"], 128)):
        actor, round_number, sent, observed = [integer(value) for value in vector(heartbeat, 4)]
        require((actor, round_number) == (index % 8, index // 8 + 1), "invalid heartbeat order")
        require(previous <= sent <= observed <= heartbeat_end, "invalid heartbeat interval")
        previous = observed
    for key, length, expected in (("hot_committed", workers, 0), ("healthy_committed", 8, 16)):
        require(all(integer(value) == expected for value in vector(case[key], length)), "invalid committed state")
    require(integer(case["replacement_committed"]) == 0, "replacement inherited old state")
    first_heartbeat = case["heartbeats"][0][2]
    for worker, observation in enumerate(vector(case["observations"], workers)):
        fields(observation, COUNTERS | {"worker", "samples"})
        require(integer(observation["worker"]) == worker, "missing or duplicate atomic worker")
        counters = {key: integer(observation[key]) for key in COUNTERS}
        require(counters["published"] == counters["taken"] + counters["dropped"]
                == counters["confirmed"] == counters["consumed"], "atomic yield conservation")
        require(counters["requests"] == counters["coalesced"] + counters["consumed"] + counters["cleared"],
                "atomic request conservation")
        require(counters["taken"] == min(counters["published"], 256) > 0, "invalid fixed atomic FIFO retention")
        previous_request = previous_yield = started
        for index, sample in enumerate(vector(observation["samples"], counters["taken"])):
            sequence, actor, requested, yielded = [integer(value) for value in vector(sample, 4)]
            require(sequence == index + 1, "invalid retained atomic sequence")
            require(actor == worker or workers <= actor <= workers + 8, "foreign atomic actor")
            require(started <= requested <= yielded <= stopped, "invalid atomic yield interval")
            require(previous_request <= requested and previous_yield <= yielded, "nonmonotonic atomic clocks")
            if index == 0:
                require(actor == worker and yielded <= first_heartbeat, "missing initial hot actor yield")
            previous_request, previous_yield = requested, yielded
    require(case["stale_identities_rejected"] is True, "stopped actor identities remain valid")
    validate_resource_receipt(case)


def validate(result):
    fields(result, {"version", "status", "metric", "cases"})
    require(integer(result["version"]) == 1 and result["status"] == "ok" and result["metric"] == METRIC,
            "invalid atomic progress protocol")
    for case, workers in zip(vector(result["cases"], 2), (1, 2)):
        validate_case(case, workers)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    arguments = parser.parse_args()
    validate(json.loads(arguments.report.read_text()))
    print("Atomic progress verified: infinite handlers, one/two workers, 128 ordered heartbeats each, "
          "bounded yield evidence and cleanup; not fairness or performance acceptance")


if __name__ == "__main__":
    main()
