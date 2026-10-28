import io
import json
import os
import urllib.error

import pytest

from research.ctxcache import hf


def reply(offset, truncated=False, length=1):
    row = {"instance_id": "owner__repo-1", "model_name": "m", "target": False, "exit_status": "submitted",
           "trajectory": [{"role": "system"}], "generated_patch": "diff", "eval_logs": "logs"}
    cells = ["trajectory"] if truncated else []
    rows = [{"row_idx": offset + k, "row": row, "truncated_cells": cells} for k in range(length)]
    return {"rows": rows, "num_rows_total": 80_036}


class FakeHTTP:
    def __init__(self, replies):
        self.replies, self.calls = list(replies), 0

    def __call__(self, request, timeout):
        self.calls += 1
        r = self.replies.pop(0)
        if isinstance(r, int):
            raise urllib.error.HTTPError(request.full_url, r, "error", {}, None)
        return io.BytesIO(json.dumps(r).encode())


# name, replies, outcome, requests made, fetcher options, gap after
CASES = [
    ("a row is cached with only the kept columns", [reply(7)], "ok", 1, {}, 1.6),
    ("a run of 3 rows comes in 1 request", [reply(7, length=3)], "ok", 1, {"length": 3}, 1.6),
    ("a truncated trajectory cell is skipped and cached as such", [reply(7, True)], "truncated", 1, {}, 1.6),
    ("429 twice, then the row: the gap grows once", [429, 429, reply(7)], "ok", 3, {}, 2.0),
    ("a client error fails at once", [404], hf.FetchError, 1, {}, 1.6),
    ("5xx until the retries run out", [503] * 7, hf.FetchError, 7, {}, 1.6),
    ("the request budget bounds the retries", [503, 503, reply(7)], hf.FetchError, 2, {"max_requests": 2}, 1.6),
    ("offline with no cached row", [], hf.FetchError, 0, {"offline": True}, 1.6),
]


@pytest.mark.parametrize("name, replies, outcome, requests, options, gap", CASES, ids=[c[0] for c in CASES])
def test_fetch_outcomes(tmp_path, name, replies, outcome, requests, options, gap):
    http, pauses, length = FakeHTTP(replies), [], options.pop("length", 1)
    f = hf.Fetcher("swe-agent", str(tmp_path), urlopen=http, pause=pauses.append, log=io.StringIO(), **options)
    if outcome is hf.FetchError:
        with pytest.raises(hf.FetchError):
            f.row(7, length)
        assert not os.path.exists(tmp_path / "swe-agent" / "7.json.gz"), name
    else:
        row, status = f.row(7, length)
        assert status == outcome, name
        assert (row is None) == (outcome == "truncated"), name
        assert row is None or sorted(row) == sorted(hf.DATASETS["swe-agent"].keep), name
        assert f.row(7) == (row, status), "the second read comes from the cache"
        assert f.row(7 + length - 1) == (row, status), "every row of the run is cached"
    assert http.calls == requests, name
    assert f.sleep == gap, name
    assert all(p <= 60 for p in pauses), name


@pytest.mark.parametrize("name, block, head", [("swe-agent", 1, [18, 104, 229]), ("openhands", 10, [149, 150, 151])])
def test_sample_is_reproducible_and_spread_over_strata(name, block, head):
    total, size = hf.DATASETS[name].rows, 1000
    offsets = hf.sample_offsets(total, size, 87, block)
    assert offsets == hf.sample_offsets(total, size, 87, block) and len(set(offsets)) == size, name
    strata = size // block
    for k in range(strata):
        run = offsets[k * block : (k + 1) * block]
        assert run == list(range(run[0], run[0] + block)), name
        assert k * total // strata <= run[0] and run[-1] < (k + 1) * total // strata, name
    assert offsets[:3] == head, "this pins the published sample"
