import random

from research.ctxcache import policies as P
from research.ctxcache.stats import reuse_distances
from research.ctxcache.trace import check

from .helpers import random_trace

TRACES = 500


def test_bounds_bracket_the_exact_optimum_on_random_tiny_traces():
    rng = random.Random(87)
    lower_gap = upper_gap = 0
    for case in range(TRACES):
        acc, budget = random_trace(rng)
        check(acc)
        where = f"trace {case}, budget {budget}: {[(a.obj, a.tokens, a.first, a.ref) for a in acc]}"
        opt = P.opt(acc, budget)
        low, up = P.pfoo_l(acc, budget), P.belady_size(acc, budget)
        for k, metric in enumerate(("misses", "missed tokens")):
            assert low[k] <= opt[k] <= up[k], f"{metric}: PFOO-L {low}, OPT {opt}, Belady-Size {up}; {where}"
            # Every policy that holds the budget at every step is feasible, so OPT is below it.
            for name in ("lru", "fifo", "gdsf"):
                assert opt[k] <= P.POLICIES[name](acc, budget)[k], f"{name} beat OPT on {metric}; {where}"
            # Clearing holds the budget only on average, and that is all PFOO-L assumes.
            assert low[k] <= P.clearing(acc, budget)[k], f"clearing beat PFOO-L on {metric}; {where}"
        lower_gap += low != opt
        upper_gap += up != opt
        # LRU hits exactly when the reuse distance plus the size fits, if every object fits.
        if all(a.tokens <= budget for a in acc):
            sizes = [a.tokens for a in acc if not a.first]
            by_distance = sum(d + s > budget for d, s in zip(reuse_distances(acc), sizes))
            assert P.lru(acc, budget)[0] == by_distance, where
    # The bounds are not trivially tight on this sample.
    assert lower_gap > TRACES // 20 and upper_gap > 0, (lower_gap, upper_gap)
