import pytest

from research.ctxcache import policies as P
from research.ctxcache.stats import measure, reuse_distances

from .helpers import trace

R = "ref"
# name, trace, budget, expected (misses, missed tokens) per policy, reuse distances, peak live, dead tokens
CASES = [
    (
        "a cycle longer than the budget: LRU misses every re-access, Belady-Size does not",
        trace(*[(n, 1) for n in "abcabcabc"]),
        2,
        {"lru": (6, 6), "fifo": (6, 6), "gdsf": (6, 6), "clear_oracle": (6, 6), "clear_cap": (6, 6),
         "belady_size": (2, 2), "pfoo_l": (1, 1), "opt": (2, 2)},
        [2] * 6, 3, 0,
    ),
    (
        "everything fits: no policy misses",
        trace(("a", 3), ("b", 2), ("a", 3), ("b", 2)),
        5,
        {p: (0, 0) for p in (*P.POLICIES, "opt")},
        [2, 3], 5, 0,
    ),
    (
        "an object over the budget misses under every policy",
        trace(("a", 5), ("b", 1), ("a", 5), ("b", 1)),
        4,
        {p: (1, 5) for p in (*P.POLICIES, "opt")},
        [1, 5], 6, 0,
    ),
    (
        "LRU and GDSF keep a hot object that FIFO drops",
        trace(*[(n, 1) for n in "abacada"]),
        2,
        {"lru": (0, 0), "fifo": (1, 1), "gdsf": (0, 0), "clear_oracle": (0, 0), "clear_cap": (0, 0),
         "belady_size": (0, 0), "pfoo_l": (0, 0), "opt": (0, 0)},
        [1, 1, 1], 1, 3,
    ),
    (
        "clearing drops a file the agent keeps naming, because references do not refresh it",
        trace(("a", 1), ("b", 1), ("a", 1, R), ("c", 1), ("a", 1, R), ("d", 1), ("a", 1, R)),
        2,
        {"lru": (0, 0), "fifo": (1, 1), "gdsf": (0, 0), "clear_oracle": (2, 2), "clear_cap": (2, 2),
         "belady_size": (0, 0), "pfoo_l": (0, 0), "opt": (0, 0)},
        [1, 1, 1], 1, 3,
    ),
]


@pytest.mark.parametrize("name, acc, budget, expected, distances, peak, dead", CASES, ids=[c[0] for c in CASES])
def test_policy_and_trace_measures(name, acc, budget, expected, distances, peak, dead):
    rec = measure(acc, {}, [budget])
    got = {p: rec["results"][(p, budget)] for p in P.POLICIES}
    got["opt"] = P.opt(acc, budget)
    assert got == expected, name
    assert reuse_distances(acc) == distances, name
    assert rec["counts"]["peak_live"] == peak, name
    assert rec["counts"]["dead_tokens"] == dead, name
    assert rec["counts"]["first"] + rec["counts"]["re"] == len(acc), name
