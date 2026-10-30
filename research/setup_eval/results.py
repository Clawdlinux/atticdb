"""The results CSV and the printed summary. Neither holds a full reply, a token or an env value."""

from __future__ import annotations

import csv
import json
import os
from collections import defaultdict

from .harness import RunResult

COLUMNS = ["date", "harness", "scenario", "run", "pass", "atticdb_calls", "input_tokens", "output_tokens",
           "cache_read_tokens", "turns", "wall_s", "setup_exit", "parse_ok", "notes"]


def _cell(value) -> str:
    return "" if value is None else str(value)


def row(date: str, res: RunResult) -> dict:
    o = res.outcome
    return {
        "date": date,
        "harness": res.harness,
        "scenario": res.scenario,
        "run": res.run,
        "pass": int(res.passed),
        "atticdb_calls": json.dumps(o.counts(), separators=(",", ":")),
        "input_tokens": _cell(o.input_tokens),
        "output_tokens": _cell(o.output_tokens),
        "cache_read_tokens": _cell(o.cache_read_tokens),
        "turns": _cell(o.turns),
        "wall_s": "" if res.wall_s is None else f"{res.wall_s:.1f}",
        "setup_exit": _cell(res.setup_exit),
        "parse_ok": int(o.parse_ok),
        "notes": "; ".join(res.notes),
    }


def append(path: str, rows: list[dict]) -> None:
    """Appends `rows`, with the header first when the file is new or empty. Each run's row is
    written as soon as it ends, so a stopped batch keeps the runs that finished."""
    folder = os.path.dirname(os.path.abspath(path))
    os.makedirs(folder, exist_ok=True)
    new = not os.path.exists(path) or os.path.getsize(path) == 0
    with open(path, "a", newline="") as f:
        w = csv.DictWriter(f, fieldnames=COLUMNS)
        if new:
            w.writeheader()
        w.writerows(rows)


def summary(results: list[RunResult]) -> list[str]:
    """A table per harness: passes over runs per scenario, and the atticdb calls in all."""
    lines = []
    by = defaultdict(lambda: defaultdict(list))
    for r in results:
        by[r.harness][r.scenario].append(r)
    for harness in sorted(by):
        lines += ["", f"{harness}", "", "| Scenario | Pass | atticdb calls | Errors |", "|---|---|---|---|"]
        for scenario in sorted(by[harness]):
            rs = by[harness][scenario]
            calls = sum(sum(r.outcome.counts().values()) for r in rs)
            lines.append(f"| {scenario} | {sum(r.passed for r in rs)}/{len(rs)} | {calls} | "
                         f"{sum(r.errored for r in rs)} |")
    return lines
