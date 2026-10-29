import itertools
import random

from research.ctxcache import policies as P
from research.ctxcache.stats import reuse_distances
from research.ctxcache.trace import Access, check

from .helpers import every_trace, random_trace

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
            for name in P.CAUSAL:
                assert opt[k] <= P.POLICIES[name](acc, budget)[k], f"{name} beat OPT on {metric}; {where}"
            # Oracle clearing holds the budget only on average, and that is all PFOO-L assumes.
            assert low[k] <= P.clear_oracle(acc, budget)[k], f"clear_oracle beat PFOO-L on {metric}; {where}"
        lower_gap += low != opt
        upper_gap += up != opt
        # LRU hits exactly when the reuse distance plus the size fits, if every object fits.
        if all(a.tokens <= budget for a in acc):
            sizes = [a.tokens for a in acc if not a.first]
            by_distance = sum(d + s > budget for d, s in zip(reuse_distances(acc), sizes))
            assert P.lru(acc, budget)[0] == by_distance, where
    # The bounds are not trivially tight on this sample.
    assert lower_gap > TRACES // 20 and upper_gap > 0, (lower_gap, upper_gap)


def test_bounds_hold_on_every_tiny_trace():
    """Every trace up to 6 accesses over 3 objects, every size in 1 to 3 per object,
    budgets 1 to 5, and 2 reference patterns: 38,250 cases. The random test above is
    regression evidence. This one covers its small space completely."""
    cases = lower_gap = upper_gap = 0
    for seq in every_trace(6, 3):
        for sizes in itertools.product((1, 2, 3), repeat=max(seq) + 1):
            for refs in (False, True):
                seen, acc = set(), []
                for i, o in enumerate(seq):
                    acc.append(Access(str(o), sizes[o], "file", o not in seen, o in seen and refs and i % 2 == 1))
                    seen.add(o)
                for budget in range(1, 6):
                    opt, low, up = P.opt(acc, budget), P.pfoo_l(acc, budget), P.belady_size(acc, budget)
                    where = f"{seq} sizes {sizes} refs {refs} budget {budget}"
                    for k in (0, 1):
                        assert low[k] <= opt[k] <= up[k], where
                        assert all(opt[k] <= P.POLICIES[n](acc, budget)[k] for n in P.CAUSAL), where
                        assert low[k] <= P.clear_oracle(acc, budget)[k], where
                    cases, lower_gap, upper_gap = cases + 1, lower_gap + (low != opt), upper_gap + (up != opt)
    assert cases == 38_250 and lower_gap > 0 and upper_gap > 0, (cases, lower_gap, upper_gap)
