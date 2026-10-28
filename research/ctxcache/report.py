"""CSV and markdown results. Aggregate numbers only, never paths or content."""

from __future__ import annotations

import csv
import math
import os
from collections import Counter
from typing import Sequence

from .policies import ONLINE
from .stats import bootstrap, percentile, pooled, ratio

ORDER = ("lru", "fifo", "clearing", "gdsf", "pfoo_l", "belady_size")
LABEL = {
    "lru": "LRU",
    "fifo": "FIFO",
    "clearing": "Clearing",
    "gdsf": "GDSF",
    "pfoo_l": "PFOO-L (lower)",
    "belady_size": "Belady-Size (upper)",
}


def kb(budget: int) -> str:
    return f"{budget // 1000}K" if budget % 1000 == 0 else str(budget)


def pct(x: float) -> str:
    return "n/a" if math.isnan(x) else f"{100 * x:.1f}"


def num(x: float) -> str:
    return f"{x:,.0f}"


def mult(x: float) -> str:
    return "n/a" if math.isnan(x) else ("inf" if math.isinf(x) else f"{x:.2f}x")


def summarize(recs: Sequence[dict], distances: Sequence[int], skipped: dict) -> dict:
    s: Counter = Counter()
    for r in recs:
        s.update(r["counts"])
    get = s.__getitem__
    d = sorted(distances)
    per = lambda k, q: percentile(sorted(r["counts"][k] for r in recs), q)  # noqa: E731
    return {
        "trajectories": len(recs),
        "skipped": dict(skipped),
        "accesses": get("accesses"),
        "first": get("first"),
        "re": get("re"),
        "rereads": get("reread"),
        "refs": get("ref"),
        "first_tokens": get("first_tokens"),
        "re_tokens": get("re_tokens"),
        "reread_tokens": get("reread_tokens"),
        "ref_tokens": get("ref_tokens"),
        "compulsory_share": ratio(get("first"), get("accesses")),
        "compulsory_token_share": ratio(get("first_tokens"), get("first_tokens") + get("re_tokens")),
        "objects": get("objects"),
        "object_tokens": get("object_tokens"),
        "file_token_share": ratio(get("file_object_tokens"), get("object_tokens")),
        "url_token_share": ratio(get("url_object_tokens"), get("object_tokens")),
        "output_token_share": ratio(get("output_object_tokens"), get("object_tokens")),
        "dead_share": ratio(get("dead_tokens"), get("object_tokens")),
        "file_dead_share": ratio(get("file_dead_tokens"), get("file_object_tokens")),
        "output_dead_share": ratio(get("output_dead_tokens"), get("output_object_tokens")),
        "distance_p50": percentile(d, 0.5),
        "distance_p90": percentile(d, 0.9),
        "distance_p99": percentile(d, 0.99),
        "peak_live_p50": per("peak_live", 0.5),
        "peak_live_p90": per("peak_live", 0.9),
        "context_p50": per("output_tokens_seen", 0.5),
        "context_p90": per("output_tokens_seen", 0.9),
        "message_tokens": get("message_tokens"),
        "output_tokens": get("output_tokens_seen"),
        "refs_without_view": get("refs_without_view"),
        "writes": get("writes"),
        "condensed": get("condensed"),
        "compactions": get("compactions"),
        "reread_same_window": get("reread_same_window"),
        "reread_same_window_tokens": get("reread_same_window_tokens"),
        "reread_after_compaction": get("reread_after_compaction"),
        "reread_after_compaction_tokens": get("reread_after_compaction_tokens"),
        "last5_token_miss": ratio(get("last5_missed"), get("re_tokens")),
        "last5_miss": ratio(get("last5_misses"), get("re")),
        "last5_mean_held": ratio(get("last5_area"), get("gaps")),
        "all_mean_held": ratio(get("all_area"), get("gaps")),
    }


def table(recs: Sequence[dict], budgets: Sequence[int], resamples: int, seed: int) -> list[dict]:
    """1 row per (budget, policy): pooled counts, ratios, intervals, and the gap to the bounds."""
    stats = {}
    for b in budgets:
        for p in ORDER:
            stats[f"tok:{p}:{b}"] = lambda s, p=p, b=b: ratio(s[(p, b, "missed")], s["re_tokens"])
            stats[f"cnt:{p}:{b}"] = lambda s, p=p, b=b: ratio(s[(p, b, "misses")], s["re"])
            if p in ONLINE:
                stats[f"up:{p}:{b}"] = lambda s, p=p, b=b: _div(s[(p, b, "missed")], s[("belady_size", b, "missed")])
                stats[f"lo:{p}:{b}"] = lambda s, p=p, b=b: _div(s[(p, b, "missed")], s[("pfoo_l", b, "missed")])
    ci = bootstrap(recs, stats, resamples, seed, columns=("re", "re_tokens")) if resamples and recs else {}
    nan2 = (math.nan, math.nan)
    re, re_tokens = pooled(recs, "re"), pooled(recs, "re_tokens")
    rows = []
    for b in budgets:
        upper = sum(r["results"][("belady_size", b)][1] for r in recs)
        lower = sum(r["results"][("pfoo_l", b)][1] for r in recs)
        for p in ORDER:
            misses = sum(r["results"][(p, b)][0] for r in recs)
            missed = sum(r["results"][(p, b)][1] for r in recs)
            online = p in ONLINE
            rows.append({
                "budget": b,
                "policy": p,
                "trajectories": len(recs),
                "reaccesses": re,
                "reaccess_tokens": re_tokens,
                "misses": misses,
                "missed_tokens": missed,
                "miss_ratio": ratio(misses, re),
                "miss_ratio_lo": ci.get(f"cnt:{p}:{b}", nan2)[0],
                "miss_ratio_hi": ci.get(f"cnt:{p}:{b}", nan2)[1],
                "token_miss_ratio": ratio(missed, re_tokens),
                "token_miss_ratio_lo": ci.get(f"tok:{p}:{b}", nan2)[0],
                "token_miss_ratio_hi": ci.get(f"tok:{p}:{b}", nan2)[1],
                # The policy's missed tokens over the optimum's lie between these 2.
                "x_opt_min": _div(missed, upper) if online else math.nan,
                "x_opt_min_lo": ci.get(f"up:{p}:{b}", nan2)[0],
                "x_opt_min_hi": ci.get(f"up:{p}:{b}", nan2)[1],
                "x_opt_max": _div(missed, lower) if online else math.nan,
                "x_opt_max_lo": ci.get(f"lo:{p}:{b}", nan2)[0],
                "x_opt_max_hi": ci.get(f"lo:{p}:{b}", nan2)[1],
                "clearing_n_p50": percentile(sorted(r["counts"][f"clearing_n@{b}"] for r in recs), 0.5)
                if p == "clearing" else math.nan,
            })
    return rows


def _div(a: float, b: float) -> float:
    if b == 0:
        return math.nan if a == 0 else math.inf
    return a / b


def write(out_dir: str, dataset: str, rows: list[dict], summary: dict, meta: dict) -> list[str]:
    os.makedirs(out_dir, exist_ok=True)
    paths = [os.path.join(out_dir, f"{dataset}{suffix}") for suffix in (".csv", "-trace.csv", ".md")]
    with open(paths[0], "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["dataset", *rows[0].keys()])
        w.writeheader()
        for row in rows:
            w.writerow({"dataset": dataset, **{k: _cell(v) for k, v in row.items()}})
    with open(paths[1], "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["dataset", "measure", "value"])
        for k, v in summary.items():
            if k == "skipped":
                for reason, n in sorted(v.items()):
                    w.writerow([dataset, f"skipped_{reason}", n])
            else:
                w.writerow([dataset, k, _cell(v)])
    with open(paths[2], "w") as f:
        f.write(markdown(dataset, rows, summary, meta))
    return paths


def _cell(v):
    if isinstance(v, float):
        return "" if math.isnan(v) else ("inf" if math.isinf(v) else f"{v:.6g}")
    return v


def markdown(dataset: str, rows: list[dict], s: dict, meta: dict) -> str:
    budgets = sorted({r["budget"] for r in rows})
    at = {(r["budget"], r["policy"]): r for r in rows}
    skipped = ", ".join(f"{n} {reason}" for reason, n in sorted(s["skipped"].items())) or "none"
    out = [
        f"# {meta['title']}",
        "",
        *meta["source"],
        f"Run: `{meta['command']}`",
        "",
        "## Trace",
        "",
        "| Measure | Value |",
        "|---|---|",
        f"| Trajectories used | {num(s['trajectories'])} (skipped: {skipped}) |",
        f"| Accesses | {num(s['accesses'])}: {num(s['first'])} first, {num(s['rereads'])} re-reads, "
        f"{num(s['refs'])} references |",
        f"| Compulsory share (first accesses) | {pct(s['compulsory_share'])}% of accesses, "
        f"{pct(s['compulsory_token_share'])}% of access tokens |",
        f"| Object tokens by kind | file {pct(s['file_token_share'])}%, output {pct(s['output_token_share'])}%, "
        f"url {pct(s['url_token_share'])}% |",
        f"| Dead tokens (objects never accessed again) | {pct(s['dead_share'])}% of object tokens. "
        f"Files {pct(s['file_dead_share'])}%, outputs {pct(s['output_dead_share'])}% |",
        f"| Reuse distance p50 / p90 / p99 | {num(s['distance_p50'])} / {num(s['distance_p90'])} / "
        f"{num(s['distance_p99'])} tokens |",
        f"| Peak live tokens per trajectory, p50 / p90 | {num(s['peak_live_p50'])} / {num(s['peak_live_p90'])}. "
        "The optimum never misses at this budget |",
        f"| Tool output tokens per trajectory, p50 / p90 | {num(s['context_p50'])} / {num(s['context_p90'])}. "
        "The context with no eviction |",
        f"| Message tokens, all trajectories | {num(s['message_tokens'])}, outside the model "
        f"(tool output: {num(s['output_tokens'])}) |",
        f"| Uses of a file with no view of its version | {num(s['refs_without_view'])}, not accesses |",
    ]
    out += [f"| {row} |" for row in meta.get("extra_rows", [])]
    out += ["", "## Missed tokens, % of re-accessed tokens", "", _grid(budgets, at, "token_miss_ratio", pct)]
    out += ["", "## Misses, % of re-accesses", "", _grid(budgets, at, "miss_ratio", pct)]
    out += [
        "",
        "## Gap to the optimum, in missed tokens",
        "",
        "Each cell is the policy's missed tokens over the upper bound, then over the lower bound.",
        "The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.",
        "\"OPT misses 0\": the upper bound is 0, so the optimum misses nothing. \"at least\": the lower bound is 0.",
        "",
        "| Budget | " + " | ".join(LABEL[p] for p in ONLINE) + " |",
        "|---|" + "---|" * len(ONLINE),
    ]
    for b in budgets:
        upper, lower = at[(b, "belady_size")]["missed_tokens"], at[(b, "pfoo_l")]["missed_tokens"]
        cells = [_gap(at[(b, p)]["missed_tokens"], upper, lower) for p in ONLINE]
        out.append(f"| {kb(b)} | " + " | ".join(cells) + " |")
    clear = [f"{kb(b)}: {num(at[(b, 'clearing')]['clearing_n_p50'])}" for b in budgets]
    out += ["", f"Clearing keeps the newest N tool outputs. Median N per budget: {', '.join(clear)}.",
            "N stops at the trajectory's count of tool outputs once everything fits."]
    out += ["", "## Scaffold baselines", ""] + meta.get("scaffold", [])
    return "\n".join(out) + "\n"


def _gap(missed: int, upper: int, lower: int) -> str:
    """The policy's multiple of the optimum, from the 2 bounds on the optimum."""
    if missed == 0:
        return "none missed"
    if upper == 0:
        return "OPT misses 0"
    if lower == 0:
        return f"at least {mult(missed / upper)}"
    return f"{mult(missed / upper)} to {mult(missed / lower)}"


def _grid(budgets, at, key, fmt) -> str:
    lines = ["| Budget | " + " | ".join(LABEL[p] for p in ORDER) + " |", "|---|" + "---|" * len(ORDER)]
    for b in budgets:
        lines.append(f"| {kb(b)} | " + " | ".join(fmt(at[(b, p)][key]) for p in ORDER) + " |")
    return "\n".join(lines)
