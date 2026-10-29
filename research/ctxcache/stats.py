"""Per-trajectory measures, pooled and macro ratios, influence, and a cluster bootstrap."""

from __future__ import annotations

import math
import operator
import random
from collections import Counter
from typing import Callable, Iterable, Sequence

from . import policies
from .trace import EDITS, REF_KINDS, Access

# Which references each proxy variant keeps. Every variant keeps every re-read.
VARIANTS = {
    "all": REF_KINDS,
    "reread": (),
    "edits": EDITS,
    "exact": ("edit_span", "line_span"),
    "no_run_git": tuple(k for k in REF_KINDS if k not in ("run", "git")),
}


def proxy_only(acc: Sequence[Access], kinds) -> list[Access]:
    """The trace with every first access and re-read, and only the references of the
    given kinds. A reference with no subtype counts as "path"."""
    keep = set(kinds)
    return [a for a in acc if a.first or not a.ref or (a.sub or "path") in keep]


def subtype(a: Access) -> str:
    return a.sub or ("first" if a.first else ("path" if a.ref else "reread"))


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


def measure(acc: Sequence[Access], info: dict, budgets: Iterable[int], sens_budgets: Iterable[int] = ()) -> dict:
    """Counts for 1 trajectory, every policy's result at every budget, and every proxy
    variant's result at the sensitivity budgets."""
    budgets, sens_budgets = list(budgets), list(sens_budgets)
    rec: Counter = Counter()
    seen: dict[str, int] = {}
    kind: dict[str, str] = {}
    for a in acc:
        cls = "first" if a.first else ("ref" if a.ref else "reread")
        rec[cls] += 1
        rec[f"{cls}_tokens"] += a.tokens
        rec[f"{a.kind}_{cls}"] += 1
        rec[f"sub_{subtype(a)}"] += 1
        rec[f"sub_{subtype(a)}_tokens"] += a.tokens
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
    variants = {}
    for v, kinds in VARIANTS.items():
        part = list(acc) if v == "all" else proxy_only(acc, kinds)
        rec[f"re@{v}"] = sum(1 for a in part if not a.first)
        rec[f"re_tokens@{v}"] = sum(a.tokens for a in part if not a.first)
        for b in sens_budgets:
            for name, policy in policies.POLICIES.items():
                variants[(v, name, b)] = results[(name, b)] if v == "all" and b in budgets else policy(part, b)
    return {"counts": rec, "results": results, "variants": variants, "cluster": info.get("cluster")}


def pooled(recs: Sequence[dict], key: str) -> int:
    return sum(r["counts"][key] for r in recs)


def percentile(sorted_values: Sequence[float], q: float) -> float:
    """Nearest-rank percentile of sorted values."""
    if not sorted_values:
        return math.nan
    return sorted_values[min(len(sorted_values) - 1, max(0, math.ceil(q * len(sorted_values)) - 1))]


def ratio(num: float, den: float) -> float:
    return num / den if den else math.nan


def macro(recs: Sequence[dict], num: Callable[[dict], float], den: Callable[[dict], float]) -> float:
    """The mean of per-trajectory ratios, over trajectories whose denominator is above 0."""
    vals = [num(r) / den(r) for r in recs if den(r) > 0]
    return sum(vals) / len(vals) if vals else math.nan


def leave_one_out(recs: Sequence[dict], num, den) -> tuple[float, float]:
    """The lowest and highest pooled ratio with 1 trajectory left out."""
    n, d = sum(num(r) for r in recs), sum(den(r) for r in recs)
    vals = [(n - num(r)) / (d - den(r)) for r in recs if d - den(r) > 0]
    return (min(vals), max(vals)) if vals else (math.nan, math.nan)


def top_share(recs: Sequence[dict], key: str, k: int = 10) -> float:
    """The share of a pooled count held by its k largest trajectories."""
    vals = sorted((r["counts"][key] for r in recs), reverse=True)
    return ratio(sum(vals[:k]), sum(vals))


def bootstrap(recs: Sequence[dict], columns: dict, stats: dict, resamples: int, seed: int) -> dict:
    """95% percentile intervals over clusters, resampled with replacement. `columns`
    maps a name to f(record), summed per cluster. `stats` maps a name to f(sums)."""
    clusters: dict = {}
    for r in recs:
        clusters.setdefault(r["cluster"], []).append(r)
    groups = list(clusters.values())
    table = {c: [sum(f(r) for r in g) for g in groups] for c, f in columns.items()}
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
