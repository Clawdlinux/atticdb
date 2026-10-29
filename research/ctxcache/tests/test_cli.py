import csv
import os

import pytest

from research.ctxcache import hf, run, trace
from research.ctxcache.policies import ONLINE, POLICIES

from .helpers import cache_row, copilot_call, copilot_session, swe_obs, swe_row, swe_window

FIXTURE = os.path.join(os.path.dirname(os.path.dirname(__file__)), "fixtures", "tiny-trace.jsonl")


def test_cli_on_the_fixture_trace(tmp_path):
    out, exported = tmp_path / "results", tmp_path / "trace.jsonl"
    argv = ["--dataset", "trace", "--input", FIXTURE, "--budgets", "0.2k,1k,4k", "--out", str(out),
            "--bootstrap", "20", "--export-trace", str(exported)]
    assert run.main(argv) == 0

    rows = list(csv.DictReader(open(out / "trace.csv")))
    assert [(r["budget"], r["policy"]) for r in rows][:2] == [("200", "lru"), ("200", "fifo")]
    assert len(rows) == 3 * len(POLICIES)
    by = {(int(r["budget"]), r["policy"]): r for r in rows}
    for budget in (200, 1000, 4000):
        missed = {p: int(by[(budget, p)]["missed_tokens"]) for p in ONLINE}
        low, up = int(by[(budget, "pfoo_l")]["missed_tokens"]), int(by[(budget, "belady_size")]["missed_tokens"])
        assert low <= up and all(low <= m for m in missed.values()), budget
    assert all(int(by[(4000, p)]["misses"]) == 0 for p in ONLINE), "all fits in 4K"
    # At 1K, LRU misses only the 310-token run reference of the first trajectory, of its 1,820.
    lru = by[(1000, "lru")]
    six = {"rel": 1e-5, "abs": 1e-9}  # the CSV keeps 6 significant digits
    assert float(lru["token_miss_ratio"]) == pytest.approx(310 / 2420, **six)
    assert float(lru["macro_token_miss_ratio"]) == pytest.approx((310 / 1820 + 0) / 2, **six)
    assert (float(lru["loo_min"]), float(lru["loo_max"])) == pytest.approx((0, 310 / 1820), **six)

    measures = {r["measure"]: r["value"] for r in csv.DictReader(open(out / "trace-trace.csv"))}
    assert measures["trajectories"] == "2" and measures["accesses"] == "20"
    assert float(measures["compulsory_share"]) == 0.5
    assert float(measures["dead_share"]) == pytest.approx(1600 / 3410)
    assert float(measures["file_dead_share"]) == pytest.approx(100 / 1010), "keys missing from the first record"
    # Each trajectory's own output tokens over its own peak live: 1,000 / 300 and 3,320 / 900.
    assert float(measures["carry_over_peak_p50"]) == pytest.approx(1000 / 300, **six)
    assert float(measures["carry_over_peak_p90"]) == pytest.approx(3320 / 900, **six)
    md = open(out / "trace.md").read()
    assert md.startswith("# Trace file: offline cache headroom on observed traces")
    assert "## Missed tokens, % of proxy-access tokens, pooled" in md and "## Reference proxy check" in md

    # Every variant keeps the 7 re-reads. The fixture has 1 edit_span, 1 run and 1 path reference.
    sens = list(csv.DictReader(open(out / "trace-sensitivity.csv")))
    proxy = {r["variant"]: r["reaccesses"] for r in sens}
    assert proxy == {"all": "10", "reread": "7", "edits": "8", "exact": "8", "no_run_git": "9"}
    assert len(sens) == 5 * 2 * len(POLICIES), "5 variants at 4K and 8K"

    original = trace.load(open(FIXTURE))
    assert trace.load(open(exported)) == original, "an exported trace reads back the same"

    with pytest.raises(SystemExit):
        run.main(argv[:-1] + [os.path.join(os.path.dirname(FIXTURE), "exported.jsonl")])
    assert not os.path.exists(os.path.join(os.path.dirname(FIXTURE), "exported.jsonl")), "never into the repo"


def session(prefix: str, ids: str, text: str) -> list[dict]:
    """3 main-agent calls: a view, a shell read of the same text, and a listing."""
    path = f"/r/{prefix}.py"
    return (copilot_call(f"{ids}1", "view", {"path": path}, text)
            + copilot_call(f"{ids}2", "bash", {"command": f"cat {path}"}, text)
            + copilot_call(f"{ids}3", "bash", {"command": "ls /r"}, "a.py b.py"))


def test_copilot_results_do_not_depend_on_the_salt(tmp_path):
    # 7 sessions tie on 3 calls, with different sizes, so any change of order moves the
    # intervals. The fork reuses the call ids of "s-a", so only 1 of the 2 is kept.
    root = tmp_path / "logs"
    copilot_session(str(root), "s-0fork", session("a", "a", "x = 2\n" * 3))
    for k, name in enumerate("abcdef"):
        copilot_session(str(root), f"s-{name}", session(name, name, f"{name} = {k}\n" * (5 * 3**k)))
    outputs = []
    for k in range(2):  # each run draws a fresh random salt
        out = tmp_path / f"run{k}"
        argv = ["--dataset", "copilot", "--copilot-root", str(root), "--out", str(out), "--budgets", "2,1k"]
        assert run.main(argv + ["--bootstrap", "200"]) == 0
        outputs.append([open(out / f).read() for f in ("copilot.csv", "copilot-trace.csv", "copilot.md")])
    assert outputs[0] == outputs[1]
    assert "skipped: 1 fork" in outputs[0][2], "the fork that sorts later is the one skipped"


def test_cli_on_a_cached_public_sample(tmp_path):
    # --sample 3 --block 2 draws 1 stratum of 2 rows. The report says 2, not 3.
    cache = tmp_path / "cache"
    offsets = hf.sample_offsets(hf.DATASETS["swe-agent"].rows, 3, 87, 2)
    assert len(offsets) == 2
    for off in offsets:
        cache_row(str(cache), "swe-agent", off, swe_row(
            ("open a.py", swe_obs(swe_window("/repo/a.py", 3, 1, 3), "/repo/a.py")),
            ("python a.py", swe_obs("ok", "/repo/a.py"))))
    out = tmp_path / "out"
    argv = ["--dataset", "swe-agent", "--sample", "3", "--block", "2", "--offline", "--cache-dir", str(cache),
            "--out", str(out), "--bootstrap", "0"]
    assert run.main(argv) == 0
    md = open(out / "swe-agent.md").read()
    assert "Sample: 2 rows." in md and "| Trajectories used | 2 " in md


REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
BAD = [
    ("a budget that is not a number", ["--dataset", "trace", "--input", FIXTURE, "--budgets", "1k,lots"]),
    ("a budget of 0", ["--dataset", "trace", "--input", FIXTURE, "--budgets", "0"]),
    ("a block larger than the sample", ["--dataset", "swe-agent", "--sample", "5", "--block", "10"]),
    ("a sample of 0", ["--dataset", "swe-agent", "--sample", "0"]),
    ("a cache dir inside the repo", ["--dataset", "swe-agent", "--cache-dir", os.path.join(REPO, "rows")]),
    ("a row that is not cached, offline", ["--dataset", "swe-agent", "--sample", "2", "--offline",
                                            "--cache-dir", "{tmp}", "--out", "{tmp}/out"]),
]


@pytest.mark.parametrize("name, argv", BAD, ids=[c[0] for c in BAD])
def test_cli_refuses_bad_input_with_exit_2(tmp_path, name, argv):
    argv = [a.replace("{tmp}", str(tmp_path)) for a in argv]
    try:
        code = run.main(argv)
    except SystemExit as e:
        code = e.code
    assert code == 2, name
    assert not os.path.exists(os.path.join(REPO, "rows")), "nothing is written into the repo"
