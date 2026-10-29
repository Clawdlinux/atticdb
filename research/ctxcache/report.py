"""CSV and markdown results: offline cache headroom on observed traces.

Aggregate numbers only, never paths or content.
"""

from __future__ import annotations

import csv
import math
import os
from collections import Counter
from typing import Sequence

from .policies import ONLINE
from .stats import VARIANTS, bootstrap, leave_one_out, macro, percentile, pooled, ratio, top_share
from .trace import REF_KINDS

HEADROOM = "offline cache headroom on observed traces"
ORDER = ("lru", "fifo", "clear_oracle", "clear_cap", "gdsf", "pfoo_l", "belady_size")
LABEL = {
    "lru": "LRU",
    "fifo": "FIFO",
    "clear_oracle": "Clearing (oracle N)",
    "clear_cap": "Clearing (cap)",
    "gdsf": "GDSF",
    "pfoo_l": "PFOO-L (lower)",
    "belady_size": "Belady-Size (upper)",
}
SUBTYPES = ("reread", *REF_KINDS)
VARIANT_LABEL = {
    "all": "all proxy accesses",
    "reread": "re-reads only",
    "edits": "re-reads and edits",
    "exact": "re-reads and exact-span edits",
    "no_run_git": "all but run and git references",
}


def kb(budget: int) -> str:
    return f"{budget // 1000}K" if budget % 1000 == 0 else str(budget)


def pct(x: float) -> str:
    return "n/a" if math.isnan(x) else f"{100 * x:.1f}"


def num(x: float) -> str:
    return "n/a" if isinstance(x, float) and math.isnan(x) else f"{x:,.0f}"


def mult(x: float) -> str:
    return "n/a" if math.isnan(x) else ("inf" if math.isinf(x) else f"{x:.2f}x")


def summarize(recs: Sequence[dict], distances: Sequence[int], skipped: dict) -> dict:
    s: Counter = Counter()
    for r in recs:
        s.update(r["counts"])
    get = s.__getitem__
    d = sorted(distances)
    per = lambda k, q: percentile(sorted(r["counts"][k] for r in recs), q)  # noqa: E731
    # Each trajectory's own carry over its own peak live, not a ratio of 2 marginals.
    paired = sorted(r["counts"]["output_tokens_seen"] / r["counts"]["peak_live"]
                    for r in recs if r["counts"]["peak_live"] > 0)
    out = {
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
        "reread_token_share": ratio(get("reread_tokens"), get("re_tokens")),
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
        "carry_over_peak_p50": percentile(paired, 0.5),
        "carry_over_peak_p90": percentile(paired, 0.9),
        "carry_over_peak_n": len(paired),
        "no_reuse": len(recs) - len(paired),
        "top10_share": top_share(recs, "re_tokens", 10),
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
    for sub in SUBTYPES:
        out[f"sub_{sub}"] = get(f"sub_{sub}")
        out[f"sub_{sub}_tokens"] = get(f"sub_{sub}_tokens")
    return out


def _share(part: float, whole: float) -> float:
    return part / whole if whole > 0 else 0.0


def _cols(prefix: tuple, missed, misses, den):
    """Bootstrap columns for 1 cell: pooled tokens and counts, and the macro sum."""
    return {
        (*prefix, "missed"): missed,
        (*prefix, "misses"): misses,
        (*prefix, "macro"): lambda r: _share(missed(r), den(r)),
    }


def table(recs: Sequence[dict], budgets: Sequence[int], resamples: int, seed: int) -> list[dict]:
    """1 row per (budget, policy): pooled and macro ratios, intervals, leave-one-out, and
    the gap to the bounds."""
    den = lambda r: r["counts"]["re_tokens"]  # noqa: E731
    columns = {"re": lambda r: r["counts"]["re"], "re_tokens": den, "reused": lambda r: int(den(r) > 0)}
    stats = {}
    for b in budgets:
        for p in ORDER:
            missed = lambda r, p=p, b=b: r["results"][(p, b)][1]  # noqa: E731
            misses = lambda r, p=p, b=b: r["results"][(p, b)][0]  # noqa: E731
            columns.update(_cols((p, b), missed, misses, den))
            stats[f"tok:{p}:{b}"] = lambda s, p=p, b=b: ratio(s[(p, b, "missed")], s["re_tokens"])
            stats[f"cnt:{p}:{b}"] = lambda s, p=p, b=b: ratio(s[(p, b, "misses")], s["re"])
            stats[f"mac:{p}:{b}"] = lambda s, p=p, b=b: ratio(s[(p, b, "macro")], s["reused"])
            if p in ONLINE:
                stats[f"up:{p}:{b}"] = lambda s, p=p, b=b: _div(s[(p, b, "missed")], s[("belady_size", b, "missed")])
                stats[f"lo:{p}:{b}"] = lambda s, p=p, b=b: _div(s[(p, b, "missed")], s[("pfoo_l", b, "missed")])
    ci = bootstrap(recs, columns, stats, resamples, seed) if resamples and recs else {}
    get = lambda name: ci.get(name, (math.nan, math.nan))  # noqa: E731
    re, re_tokens = pooled(recs, "re"), pooled(recs, "re_tokens")
    rows = []
    for b in budgets:
        upper = sum(r["results"][("belady_size", b)][1] for r in recs)
        lower = sum(r["results"][("pfoo_l", b)][1] for r in recs)
        for p in ORDER:
            missed_of = lambda r, p=p, b=b: r["results"][(p, b)][1]  # noqa: E731
            misses = sum(r["results"][(p, b)][0] for r in recs)
            missed = sum(missed_of(r) for r in recs)
            online = p in ONLINE
            loo = leave_one_out(recs, missed_of, den)
            rows.append({
                "budget": b,
                "policy": p,
                "trajectories": len(recs),
                "reaccesses": re,
                "reaccess_tokens": re_tokens,
                "misses": misses,
                "missed_tokens": missed,
                "miss_ratio": ratio(misses, re),
                "miss_ratio_lo": get(f"cnt:{p}:{b}")[0],
                "miss_ratio_hi": get(f"cnt:{p}:{b}")[1],
                "token_miss_ratio": ratio(missed, re_tokens),
                "token_miss_ratio_lo": get(f"tok:{p}:{b}")[0],
                "token_miss_ratio_hi": get(f"tok:{p}:{b}")[1],
                "macro_token_miss_ratio": macro(recs, missed_of, den),
                "macro_lo": get(f"mac:{p}:{b}")[0],
                "macro_hi": get(f"mac:{p}:{b}")[1],
                "loo_min": loo[0],
                "loo_max": loo[1],
                # The policy's missed tokens over the optimum's lie between these 2.
                "x_opt_min": _div(missed, upper) if online else math.nan,
                "x_opt_min_lo": get(f"up:{p}:{b}")[0],
                "x_opt_min_hi": get(f"up:{p}:{b}")[1],
                "x_opt_max": _div(missed, lower) if online else math.nan,
                "x_opt_max_lo": get(f"lo:{p}:{b}")[0],
                "x_opt_max_hi": get(f"lo:{p}:{b}")[1],
                "clearing_n_p50": percentile(sorted(r["counts"][f"clearing_n@{b}"] for r in recs), 0.5)
                if p == "clear_oracle" else math.nan,
            })
    return rows


def sensitivity(recs: Sequence[dict], budgets: Sequence[int], resamples: int, seed: int) -> list[dict]:
    """1 row per (variant, budget, policy): the same policies on a narrower proxy."""
    columns, stats = {}, {}
    for v in VARIANTS:
        den = lambda r, v=v: r["counts"][f"re_tokens@{v}"]  # noqa: E731
        columns[f"re_tokens@{v}"] = den
        columns[f"reused@{v}"] = lambda r, den=den: int(den(r) > 0)
        for b in budgets:
            for p in ORDER:
                missed = lambda r, v=v, p=p, b=b: r["variants"][(v, p, b)][1]  # noqa: E731
                misses = lambda r, v=v, p=p, b=b: r["variants"][(v, p, b)][0]  # noqa: E731
                columns.update(_cols((v, p, b), missed, misses, den))
                stats[f"tok:{v}:{p}:{b}"] = lambda s, v=v, p=p, b=b: ratio(s[(v, p, b, "missed")], s[f"re_tokens@{v}"])
                stats[f"mac:{v}:{p}:{b}"] = lambda s, v=v, p=p, b=b: ratio(s[(v, p, b, "macro")], s[f"reused@{v}"])
    ci = bootstrap(recs, columns, stats, resamples, seed) if resamples and recs else {}
    get = lambda name: ci.get(name, (math.nan, math.nan))  # noqa: E731
    rows = []
    for v in VARIANTS:
        den = lambda r, v=v: r["counts"][f"re_tokens@{v}"]  # noqa: E731
        re, re_tokens = pooled(recs, f"re@{v}"), pooled(recs, f"re_tokens@{v}")
        for b in budgets:
            for p in ORDER:
                missed_of = lambda r, v=v, p=p, b=b: r["variants"][(v, p, b)][1]  # noqa: E731
                missed = sum(missed_of(r) for r in recs)
                rows.append({
                    "variant": v,
                    "budget": b,
                    "policy": p,
                    "reaccesses": re,
                    "reaccess_tokens": re_tokens,
                    "misses": sum(r["variants"][(v, p, b)][0] for r in recs),
                    "missed_tokens": missed,
                    "token_miss_ratio": ratio(missed, re_tokens),
                    "token_miss_ratio_lo": get(f"tok:{v}:{p}:{b}")[0],
                    "token_miss_ratio_hi": get(f"tok:{v}:{p}:{b}")[1],
                    "macro_token_miss_ratio": macro(recs, missed_of, den),
                    "macro_lo": get(f"mac:{v}:{p}:{b}")[0],
                    "macro_hi": get(f"mac:{v}:{p}:{b}")[1],
                })
    return rows


def _div(a: float, b: float) -> float:
    if b == 0:
        return math.nan if a == 0 else math.inf
    return a / b


def write(out_dir: str, name: str, rows: list[dict], sens: list[dict], summary: dict, meta: dict) -> list[str]:
    os.makedirs(out_dir, exist_ok=True)
    paths = [os.path.join(out_dir, f"{name}{suffix}") for suffix in (".csv", "-sensitivity.csv", "-trace.csv", ".md")]
    for path, table_rows in ((paths[0], rows), (paths[1], sens)):
        with open(path, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=["dataset", *table_rows[0].keys()])
            w.writeheader()
            for row in table_rows:
                w.writerow({"dataset": name, **{k: _cell(v) for k, v in row.items()}})
    with open(paths[2], "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["dataset", "measure", "value"])
        for k, v in summary.items():
            if k == "skipped":
                for reason, n in sorted(v.items()):
                    w.writerow([name, f"skipped_{reason}", n])
            else:
                w.writerow([name, k, _cell(v)])
        for k, v in meta.get("measures", {}).items():
            w.writerow([name, k, _cell(v)])
    with open(paths[3], "w") as f:
        f.write(markdown(rows, sens, summary, meta))
    return paths


def _cell(v):
    if isinstance(v, float):
        return "" if math.isnan(v) else ("inf" if math.isinf(v) else f"{v:.6g}")
    return v


def markdown(rows: list[dict], sens: list[dict], s: dict, meta: dict) -> str:
    budgets = sorted({r["budget"] for r in rows})
    at = {(r["budget"], r["policy"]): r for r in rows}
    skipped = ", ".join(f"{n} {reason}" for reason, n in sorted(s["skipped"].items())) or "none"
    out = [
        f"# {meta['title']}: {HEADROOM}",
        "",
        *meta["source"],
        f"Run: `{meta['command']}`",
        "",
        "These numbers replay fixed traces offline. An agent's next step depends on what its own context held,",
        "so another policy would change the trace itself. They measure offline cache headroom on observed traces,",
        "not what an agent would do under each policy.",
        f"Every ratio is over proxy accesses: re-reads plus references inferred from later actions. "
        f"Re-reads hold {pct(s['reread_token_share'])}% of the proxy-access tokens here.",
        *meta.get("notes", []),
        "",
        "## Trace",
        "",
        "| Measure | Value |",
        "|---|---|",
        f"| Trajectories used | {num(s['trajectories'])} (skipped: {skipped}) |",
        f"| Accesses | {num(s['accesses'])}: {num(s['first'])} first, {num(s['rereads'])} re-reads, "
        f"{num(s['refs'])} references |",
        f"| Proxy accesses | {num(s['re'])}, {num(s['re_tokens'])} tokens. Re-reads hold "
        f"{pct(s['reread_token_share'])}% of the tokens |",
        f"| Compulsory share (first accesses) | {pct(s['compulsory_share'])}% of accesses, "
        f"{pct(s['compulsory_token_share'])}% of access tokens |",
        f"| Object tokens by kind | file {pct(s['file_token_share'])}%, output {pct(s['output_token_share'])}%, "
        f"url {pct(s['url_token_share'])}% |",
        f"| Dead tokens (objects never accessed again) | {pct(s['dead_share'])}% of object tokens. "
        f"Files {pct(s['file_dead_share'])}%, outputs {pct(s['output_dead_share'])}% |",
        f"| Reuse distance p50 / p90 / p99 | {num(s['distance_p50'])} / {num(s['distance_p90'])} / "
        f"{num(s['distance_p99'])} tokens |",
        f"| Peak live tokens per trajectory, p50 / p90, separate percentiles | {num(s['peak_live_p50'])} / "
        f"{num(s['peak_live_p90'])}. The optimum never misses at this budget |",
        f"| Tool output tokens per trajectory, p50 / p90, separate percentiles | {num(s['context_p50'])} / "
        f"{num(s['context_p90'])}. The context with no eviction |",
        f"| Tool output over peak live, per trajectory, p50 / p90 | {_x(s['carry_over_peak_p50'])} / "
        f"{_x(s['carry_over_peak_p90'])}, over the {num(s['carry_over_peak_n'])} trajectories that reuse "
        f"anything. {num(s['no_reuse'])} reuse nothing |",
        f"| The 10 trajectories with the most proxy-access tokens | hold {pct(s['top10_share'])}% of them |",
        f"| Message tokens, all trajectories | {num(s['message_tokens'])}, outside the model "
        f"(tool output: {num(s['output_tokens'])}) |",
        f"| Uses of a file with no view of its version | {num(s['refs_without_view'])}, not accesses |",
    ]
    out += [f"| {row} |" for row in meta.get("extra_rows", [])]
    out += ["", "## Proxy accesses by subtype", "", "| Subtype | Accesses | Tokens | Share of proxy-access tokens |",
            "|---|---|---|---|"]
    for sub in SUBTYPES:
        out.append(f"| `{sub}` | {num(s[f'sub_{sub}'])} | {num(s[f'sub_{sub}_tokens'])} | "
                   f"{pct(ratio(s[f'sub_{sub}_tokens'], s['re_tokens']))}% |")
    out += ["", "## Missed tokens, % of proxy-access tokens, pooled", "",
            "All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.", "",
            _grid(budgets, at, "token_miss_ratio", pct)]
    out += ["", "## Missed tokens, mean of per-trajectory %, macro", "",
            "Each trajectory with proxy accesses weighs the same.", "", _grid(budgets, at, "macro_token_miss_ratio", pct)]
    out += ["", "## Misses, % of proxy accesses, pooled", "", _grid(budgets, at, "miss_ratio", pct)]
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
    if sens:
        out += _sensitivity_md(sens)
    out += ["", "## Influence", ""]
    for b in sorted({r["budget"] for r in sens}) or budgets[:2]:
        r = at.get((b, "lru"))
        if r:
            out.append(f"- LRU at {kb(b)}: {pct(r['token_miss_ratio'])}% pooled. Leaving out any 1 trajectory "
                       f"gives {pct(r['loo_min'])}% to {pct(r['loo_max'])}%.")
    out.append(f"- The 10 trajectories with the most proxy-access tokens hold {pct(s['top10_share'])}% of them.")
    clear = [f"{kb(b)}: {num(at[(b, 'clear_oracle')]['clearing_n_p50'])}" for b in budgets]
    out += ["", "## Clearing", "",
            "Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, "
            "and only the average fits the budget.",
            f"Median N per budget: {', '.join(clear)}. N stops at the trajectory's count of tool outputs "
            "once everything fits.",
            "Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal."]
    out += ["", "## Scaffold baselines", ""] + meta.get("scaffold", [])
    return "\n".join(out) + "\n"


def _sensitivity_md(sens: list[dict]) -> list[str]:
    at = {(r["variant"], r["budget"], r["policy"]): r for r in sens}
    out = ["", "## Reference proxy check", "",
           "Each variant keeps every re-read and only the named references. "
           "Cells are pooled missed tokens, % of that variant's proxy-access tokens.", "",
           "| Variant | Budget | Proxy tokens | " + " | ".join(LABEL[p] for p in ORDER) + " |",
           "|---|---|---|" + "---|" * len(ORDER)]
    for v in VARIANTS:
        for b in sorted({r["budget"] for r in sens}):
            first = at[(v, b, ORDER[0])]
            cells = " | ".join(pct(at[(v, b, p)]["token_miss_ratio"]) for p in ORDER)
            out.append(f"| {VARIANT_LABEL[v]} | {kb(b)} | {num(first['reaccess_tokens'])} | {cells} |")
    return out


def _x(v: float) -> str:
    return "n/a" if math.isnan(v) else f"{v:.1f}x"


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
