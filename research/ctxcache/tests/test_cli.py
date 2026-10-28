import csv
import os

import pytest

from research.ctxcache import run, trace

FIXTURE = os.path.join(os.path.dirname(os.path.dirname(__file__)), "fixtures", "tiny-trace.jsonl")


def test_cli_on_the_fixture_trace(tmp_path):
    out, exported = tmp_path / "results", tmp_path / "trace.jsonl"
    argv = ["--dataset", "trace", "--input", FIXTURE, "--budgets", "0.2k,4k", "--out", str(out),
            "--bootstrap", "20", "--export-trace", str(exported)]
    assert run.main(argv) == 0

    rows = list(csv.DictReader(open(out / "trace.csv")))
    assert [(r["budget"], r["policy"]) for r in rows][:2] == [("200", "lru"), ("200", "fifo")]
    assert len(rows) == 2 * 6
    by = {(int(r["budget"]), r["policy"]): r for r in rows}
    for budget in (200, 4000):
        missed = {p: int(by[(budget, p)]["missed_tokens"]) for p in ("lru", "fifo", "clearing", "gdsf")}
        low, up = int(by[(budget, "pfoo_l")]["missed_tokens"]), int(by[(budget, "belady_size")]["missed_tokens"])
        assert low <= up and all(low <= m for m in missed.values()), budget
    assert all(int(by[(4000, p)]["misses"]) == 0 for p in ("lru", "fifo", "clearing", "gdsf")), "all fits in 4K"

    measures = {r["measure"]: r["value"] for r in csv.DictReader(open(out / "trace-trace.csv"))}
    assert measures["trajectories"] == "2" and measures["accesses"] == "20"
    assert float(measures["compulsory_share"]) == 0.5
    assert float(measures["dead_share"]) == pytest.approx(1600 / 3410)
    assert float(measures["file_dead_share"]) == pytest.approx(100 / 1010), "keys missing from the first record"
    assert "## Missed tokens, % of re-accessed tokens" in open(out / "trace.md").read()

    # Dropping references leaves the 7 re-reads: 1 in the first trajectory, 6 in the cycle.
    assert run.main(argv[:-2] + ["--drop-refs"]) == 0
    rereads = list(csv.DictReader(open(out / "trace-rereads.csv")))
    assert {r["reaccesses"] for r in rereads} == {"7"} and len(rereads) == 12

    original = trace.load(open(FIXTURE))
    assert trace.load(open(exported)) == original, "an exported trace reads back the same"

    with pytest.raises(SystemExit):
        run.main(argv[:-1] + [os.path.join(os.path.dirname(FIXTURE), "exported.jsonl")])
    assert not os.path.exists(os.path.join(os.path.dirname(FIXTURE), "exported.jsonl")), "never into the repo"
