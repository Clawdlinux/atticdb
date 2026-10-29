import http.client
import io
import json
import os
import re
import urllib.error

import pytest

from research.ctxcache import guard, hf


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
        if isinstance(r, BaseException):
            raise r
        if isinstance(r, (int, tuple)):
            code, headers = r if isinstance(r, tuple) else (r, {})
            raise urllib.error.HTTPError(request.full_url, code, "error", headers, None)
        return io.BytesIO(r if isinstance(r, bytes) else json.dumps(r).encode())


# name, replies, outcome, requests made, fetcher options, gap after, pauses logged
CASES = [
    ("a row is cached with only the kept columns", [reply(7)], "ok", 1, {}, 1.6, []),
    ("a run of 3 rows comes in 1 request", [reply(7, length=3)], "ok", 1, {"length": 3}, 1.6, []),
    ("a truncated trajectory cell is skipped and cached as such", [reply(7, True)], "truncated", 1, {}, 1.6, []),
    ("429 twice, then the row: the gap grows once", [429, 429, reply(7)], "ok", 3, {}, 2.0, [1, 2]),
    ("a reset, a cut body and a short read are retried",
     [ConnectionResetError(), b'{"rows": [', http.client.IncompleteRead(b""), reply(7)], "ok", 4, {}, 1.6, [1, 2, 4]),
    ("a Retry-After past the cap logs the 60 s it sleeps", [(429, {"Retry-After": "300"}), reply(7)], "ok", 2, {},
     2.0, [60]),
    ("a client error fails at once", [404], hf.FetchError, 1, {}, 1.6, []),
    ("5xx until the retries run out", [503] * 7, hf.FetchError, 7, {}, 1.6, [1, 2, 4, 8, 16, 32]),
    ("the request budget bounds the retries", [503, 503, reply(7)], hf.FetchError, 2, {"max_requests": 2}, 1.6, [1, 2]),
    ("offline with no cached row", [], hf.FetchError, 0, {"offline": True}, 1.6, []),
]


@pytest.mark.parametrize("name, replies, outcome, requests, options, gap, logged", CASES, ids=[c[0] for c in CASES])
def test_fetch_outcomes(tmp_path, name, replies, outcome, requests, options, gap, logged):
    http, pauses, log, length = FakeHTTP(replies), [], io.StringIO(), options.pop("length", 1)
    f = hf.Fetcher("swe-agent", str(tmp_path), urlopen=http, pause=pauses.append, log=log, **options)
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
    said = [float(x) for x in re.findall(r"retry \d+ in (\d+) s", log.getvalue())]
    assert said == logged and all(p in pauses for p in said), f"{name}: the log names the real pause"


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


# name, cache dir under the stand-in repo's parent, refused. In every case cache/swe-agent
# is a symlink to repo/rows.
GUARD = [
    ("a dir below the repo that does not exist yet", "repo/new", True),
    ("a sibling whose name starts with the repo's", "repo2", False),
    ("the repo under another case, on a case-insensitive disk", "REPO", True),
    ("a dataset folder that is a symlink into the repo", "cache", True),
]


@pytest.mark.parametrize("name, rel, refused", GUARD, ids=[c[0] for c in GUARD])
def test_cached_rows_never_land_in_the_repo(tmp_path, name, rel, refused):
    repo = tmp_path / "repo"
    (repo / "rows").mkdir(parents=True)
    (tmp_path / "repo2").mkdir()
    (tmp_path / "cache").mkdir()
    os.symlink(repo / "rows", tmp_path / "cache" / "swe-agent")
    if rel == "REPO" and not os.path.exists(tmp_path / "REPO"):
        pytest.skip("this filesystem is case-sensitive, so the alias does not exist")
    http = FakeHTTP([reply(7)])
    f = hf.Fetcher("swe-agent", str(tmp_path / rel), urlopen=http, pause=lambda s: None, log=io.StringIO(),
                   repo=str(repo))
    if refused:
        with pytest.raises(guard.InsideRepo):
            f.row(7)
        assert http.calls == 0, f"{name}: refused before any request"
    else:
        assert f.row(7)[1] == "ok", name
    assert sorted(os.listdir(repo)) == ["rows"] and os.listdir(repo / "rows") == [], f"{name}: the repo is untouched"


# name, destination under the stand-in repo's parent, its target if it is a symlink, refused.
# In every case repo/out-link is a symlink to the outside folder, which holds old.csv.
WRITES = [
    ("a new file outside the repo", "out/new.csv", None, False),
    ("a file below the repo", "repo/new.csv", None, True),
    ("a symlink in the repo that points out", "repo/sheet.csv", "out/old.csv", True),
    ("a symlink in the repo that points out to no file yet", "repo/sheet.csv", "out/missing.csv", True),
    ("a file in a folder of the repo that links out", "repo/out-link/new.csv", None, True),
    ("a symlink outside the repo", "out/link.csv", "out/old.csv", True),
]


def entries(folder) -> list:
    """Each entry with its link target or its text. Links are not followed."""
    return sorted((e.name, os.readlink(e) if e.is_symlink() else e.read_text()) for e in folder.iterdir())


@pytest.mark.parametrize("name, rel, target, refused", WRITES, ids=[w[0] for w in WRITES])
def test_a_write_never_lands_in_the_repo(tmp_path, name, rel, target, refused):
    # The new file is made and renamed in the destination's own folder, not its target's.
    repo, out = tmp_path / "repo", tmp_path / "out"
    repo.mkdir()
    out.mkdir()
    (out / "old.csv").write_text("old")
    os.symlink(out, repo / "out-link")
    if target:
        os.symlink(tmp_path / target, tmp_path / rel)
    before = entries(repo)

    def write():
        with guard.replacing(str(tmp_path / rel), "the sheet", root=str(repo)) as f:
            f.write("new")

    if refused:
        with pytest.raises(guard.InsideRepo):
            write()
    else:
        write()
        assert (tmp_path / rel).read_text() == "new", name
    assert entries(repo) == before, f"{name}: nothing in the repo changed"
    assert (out / "old.csv").read_text() == "old", f"{name}: a link's target is never written"
