"""Scoring: recall@k, hit@k, the token estimate and the per-arm summary.

Evidence ids are deduped before scoring, and so are retrieved ids.
A question with empty evidence can't be scored, so it raises.
"""

from __future__ import annotations

import math
import statistics
from collections.abc import Iterable, Mapping, Sequence

from .data import CATEGORY_NAMES

LATENCY = "latency_ms"


def _found(retrieved: Sequence[str], evidence: Iterable[str], k: int) -> tuple[int, int]:
    """(evidence ids in the top k, distinct evidence ids)."""
    if k < 1:
        raise ValueError(f"k must be at least 1, got {k}")
    gold = set(evidence)
    if not gold:
        raise ValueError("empty evidence can't be scored")
    return len(gold & set(retrieved[:k])), len(gold)


def recall(retrieved: Sequence[str], evidence: Iterable[str], k: int) -> float:
    """The share of distinct evidence ids found in the top k of `retrieved`."""
    found, total = _found(retrieved, evidence, k)
    return found / total


def hit(retrieved: Sequence[str], evidence: Iterable[str], k: int) -> bool:
    """True when the top k of `retrieved` holds at least 1 evidence id."""
    return _found(retrieved, evidence, k)[0] > 0


def approx_tokens(texts: Iterable[str]) -> float:
    """UTF-8 bytes of `texts` divided by 4. Reported as "≈ tokens (bytes/4)"."""
    return sum(len(t.encode("utf-8")) for t in texts) / 4


def percentile(values: Sequence[float], pct: float) -> float:
    """The nearest-rank percentile: the smallest value with pct% of values at or below it."""
    if not values:
        raise ValueError("percentile of no values")
    ordered = sorted(values)
    rank = max(1, math.ceil(pct / 100 * len(ordered)))
    return ordered[rank - 1]


def _aggregate(rows: list[Mapping], metrics: list[str]) -> dict:
    out: dict = {"n": len(rows)}
    for m in metrics:
        out[m] = statistics.fmean(float(r[m]) for r in rows)
    latencies = [r[LATENCY] for r in rows if r.get(LATENCY) is not None]
    if latencies:
        out[f"{LATENCY}_median"] = statistics.median(latencies)
        out[f"{LATENCY}_p95"] = percentile(latencies, 95)
    return out


def summarize(rows: Iterable[Mapping]) -> dict:
    """Average 1 arm's per-question rows, overall and per category.

    Each row has `category` (1 to 4), the same metric keys, and optional `latency_ms`.
    Metric values are numbers or bools, and bools average to a share.
    `latency_ms` gives a median and a p95, and None means not measured.
    The result has `n`, each metric's mean, and `by_category` keyed by category name.
    Rows with different keys, or with latency in some rows only, raise ValueError.
    """
    rows = list(rows)
    if not rows:
        return {"n": 0, "by_category": {}}
    keys = set(rows[0]) - {"category", LATENCY}
    has_latency = rows[0].get(LATENCY) is not None
    for r in rows:
        if (other := set(r) - {"category", LATENCY}) != keys:
            raise ValueError(f"rows have different metric keys: {sorted(keys)} vs {sorted(other)}")
        if (r.get(LATENCY) is not None) != has_latency:
            raise ValueError("latency_ms is set in some rows only")
        if r.get("category") not in CATEGORY_NAMES:
            raise ValueError(f"row category must be 1 to 4, got {r.get('category')!r}")
    metrics = sorted(keys)
    out = _aggregate(rows, metrics)
    out["by_category"] = {
        CATEGORY_NAMES[c]: _aggregate([r for r in rows if r["category"] == c], metrics)
        for c in sorted({r["category"] for r in rows})
    }
    return out
