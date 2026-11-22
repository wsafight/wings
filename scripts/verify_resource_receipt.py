import copy


def validate(result, *, historical=False):
    fields = ("bootstrap_bytes", "remaining_bytes")
    if result.get("resources_released") is not True:
        raise ValueError("Wings resources not released")
    if historical and all(field not in result for field in fields):
        return None
    for field in fields:
        value = result.get(field)
        if type(value) is not int or not 0 < value < 2**64:
            raise ValueError(f"invalid Wings {field}")
    if result["remaining_bytes"] != result["bootstrap_bytes"]:
        raise ValueError("Wings remaining bytes differ from bootstrap")
    return result["bootstrap_bytes"]


def models():
    valid = {"resources_released": True, "bootstrap_bytes": 168, "remaining_bytes": 168}
    assert validate(valid) == 168
    assert validate(valid, historical=True) == 168
    assert validate({"resources_released": True}, historical=True) is None
    rejected = []
    for field in valid:
        missing = copy.deepcopy(valid)
        del missing[field]
        rejected.append(missing)
        for value in [None, False, True, -1, 0, 168.0, "168", [], {}, 2**64]:
            if field == "resources_released" and value is True:
                continue
            changed = copy.deepcopy(valid)
            changed[field] = value
            rejected.append(changed)
    rejected.extend([
        {}, {"resources_released": False},
        {"resources_released": True},
        {**valid, "remaining_bytes": 167},
        {**valid, "remaining_bytes": 169},
    ])
    for result in rejected:
        for historical in [False, True]:
            if historical and result.get("resources_released") is True and all(field not in result for field in ("bootstrap_bytes", "remaining_bytes")):
                continue
            try:
                validate(result, historical=historical)
            except ValueError:
                continue
            raise AssertionError(("accepted forged resource receipt", result, historical))
    return len(rejected)


if __name__ == "__main__":
    print(f"Resource receipts verified: positive accounted bootstrap, exact cleanup, {models()} forged receipts and historical replay")
