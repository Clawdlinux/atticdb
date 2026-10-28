"""Per-trajectory measures, pooled ratios, and a cluster bootstrap."""

from __future__ import annotations

import math
import operator
import random
from collections import Counter
from typing import Iterable, Sequence

from . import policies
from .trace import Access


def reuse_distances(acc: Sequence[Access]) -> list[int]:
    """Per non-first access: the tokens of distinct other objects accessed since the
    previous access to the same object (the token-weighted LRU stack distance). LRU
    with budget B hits exactly when distance + size <= B, if no object exceeds B."""
    tree = [0] * (len(acc) + 1)

    def add(i: int, v: int) -> None:
        i += 1
        while i < len(tree):
            tree[i] += v
            i += i & -i

    def upto(i: int) -> int:  # sum over positions 0..i
        s, i = 0, i + 1
        while i > 0:
            s += tree[i]
            i -= i & -i
        return s

    last: dict[str, int] = {}
    out = []
    for t, a in enumerate(acc):
        p = last.get(a.obj)
        if p is not None:
            out.append(upto(t - 1) - upto(p))
            add(p, -a.tokens)
        add(t, a.tokens)
        last[a.obj] = t
    return out


def peak_live(acc: Sequence[Access]) -> int:
    """The most tokens that are live at once: held from an access to its next access.
    The optimum never misses at this budget, and misses at any budget below it."""
    nxt = policies.next_use(acc)
    delta = [0] * (len(acc) + 1)
    for i, a in enumerate(acc):
        if nxt[i] != math.inf:
            delta[i] += a.tokens
            delta[int(nxt[i])] -= a.tokens
    peak = cur = 0
    for d in delta:
        cur += d
        peak = max(peak, cur)
    return peak


def measure(acc: Sequence[Access], info: dict, budgets: Iterable[int]) -> dict:
    """Counts for 1 trajectory, and every policy's result at every budget."""
    rec: Counter = Counter()
    seen: dict[str, int] = {}
    kind: dict[str, str] = {}
    for a in acc:
        cls = "first" if a.first else ("ref" if a.ref else "reread")
        rec[cls] += 1
        rec[f"{cls}_tokens"] += a.tokens
        rec[f"{a.kind}_{cls}"] += 1
        seen[a.obj] = seen.get(a.obj, 0) + 1
        kind[a.obj] = a.kind
        if not a.ref:
            rec["output_tokens_seen"] += a.tokens
    size = {a.obj: a.tokens for a in acc}
    for obj, n in seen.items():
        rec["object_tokens"] += size[obj]
        rec[f"{kind[obj]}_object_tokens"] += size[obj]
        if n == 1:
            rec["dead_tokens"] += size[obj]
            rec[f"{kind[obj]}_dead_tokens"] += size[obj]
    rec["accesses"] = len(acc)
    rec["objects"] = len(seen)
    rec["re"] = rec["ref"] + rec["reread"]
    rec["re_tokens"] = rec["ref_tokens"] + rec["reread_tokens"]
    rec["gaps"] = max(0, len(acc) - 1)
    rec["peak_live"] = peak_live(acc)
    for k in ("message_tokens", "refs_without_view", "writes", "condensed", "compactions",
              "reread_same_window", "reread_same_window_tokens",
              "reread_after_compaction", "reread_after_compaction_tokens"):
        rec[k] += int(info.get(k, 0) or 0)
    m, t, area, peak = policies.keep_last(acc, 5)
    rec.update({"last5_misses": m, "last5_missed": t, "last5_area": area, "last5_peak": peak})
    _, _, area, peak = policies.keep_last(acc, math.inf)
    rec.update({"all_area": area, "all_peak": peak})
    results = {}
    for b in budgets:
        for name, policy in policies.POLICIES.items():
            results[(name, b)] = policy(acc, b)
        rec[f"clearing_n@{b}"] = policies.clearing_keep(acc, b)
    return {"counts": rec, "results": results, "cluster": info.get("cluster")}


def pooled(recs: Sequence[dict], key: str) -> int:
    return sum(r["counts"][key] for r in recs)


def percentile(sorted_values: Sequence[float], q: float) -> float:
    """Nearest-rank percentile of sorted values."""
    if not sorted_values:
        return math.nan
    return sorted_values[min(len(sorted_values) - 1, max(0, math.ceil(q * len(sorted_values)) - 1))]


def ratio(num: float, den: float) -> float:
    return num / den if den else math.nan


def bootstrap(recs: Sequence[dict], stats, resamples: int, seed: int, columns: Iterable[str] = ()) -> dict:
    """95% percentile intervals over clusters (instances or sessions), resampled with
    replacement. `stats` maps a name to f(sums), where sums maps a column to its total.
    Columns are the given counts keys and (policy, budget, "misses" or "missed")."""
    clusters: dict = {}
    for r in recs:
        clusters.setdefault(r["cluster"], []).append(r)
    groups = list(clusters.values())
    table: dict = {c: [sum(r["counts"][c] for r in g) for g in groups] for c in columns}
    for k in sorted({k for r in recs for k in r["results"]}, key=str):
        table[(*k, "misses")] = [sum(r["results"][k][0] for r in g) for g in groups]
        table[(*k, "missed")] = [sum(r["results"][k][1] for r in g) for g in groups]
    rng, n = random.Random(seed), len(groups)
    draws: dict[str, list[float]] = {name: [] for name in stats}
    for _ in range(resamples):
        w = [0] * n
        for _ in range(n):
            w[rng.randrange(n)] += 1
        sums = {col: sum(map(operator.mul, vals, w)) for col, vals in table.items()}
        for name, f in stats.items():
            draws[name].append(f(sums))
    out = {}
    for name, values in draws.items():
        values = sorted(v for v in values if not math.isnan(v))
        out[name] = (percentile(values, 0.025), percentile(values, 0.975)) if values else (math.nan, math.nan)
    return out
