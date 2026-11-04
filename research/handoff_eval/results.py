"""The results CSV and the markdown summary. Neither holds a full reply, a token or an env value."""

from __future__ import annotations

import csv
import os
from collections import defaultdict

from .arms import ARMS
from .run import HandoffResult

FIRST_LINE = "Integration pilot. Not a statistical result."

COLUMNS = ["date", "case", "arm", "writer", "reader", "run", "captured", "captured_where", "delivered",
           "delivered_where", "passed", "check_timed_out", "errored", "error_stage", "sessions", "input_tokens",
           "output_tokens", "wall_s", "writer_model", "reader_model", "reasons"]


def flag(value: bool | None) -> str:
    """1, 0, or empty for not applicable or not measured."""
    return "" if value is None else str(int(value))


def _cell(value) -> str:
    return "" if value is None else str(value)


def row(date: str, res: HandoffResult) -> dict:
    return {
        "date": date,
        "case": res.case,
        "arm": res.arm,
        "writer": res.writer,
        "reader": res.reader,
        "run": res.run,
        "captured": flag(res.captured),
        "captured_where": res.captured_where,
        "delivered": flag(res.delivered),
        "delivered_where": res.delivered_where,
        "passed": flag(res.passed),
        "check_timed_out": int(res.check_timed_out),
        "errored": int(res.errored),
        "error_stage": res.error_stage,
        "sessions": res.sessions,
        "input_tokens": _cell(res.input_tokens),
        "output_tokens": _cell(res.output_tokens),
        "wall_s": "" if res.wall_s is None else f"{res.wall_s:.1f}",
        "writer_model": res.writer_model,
        "reader_model": res.reader_model,
        "reasons": "; ".join(res.reasons),
    }


def append(path: str, rows: list[dict]) -> None:
    """Appends `rows`, with the header first when the file is new or empty. Each run's row is
    written as soon as it ends, so a stopped batch keeps the runs that finished."""
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    new = not os.path.exists(path) or os.path.getsize(path) == 0
    with open(path, "a", newline="") as f:
        w = csv.DictWriter(f, fieldnames=COLUMNS)
        if new:
            w.writeheader()
        w.writerows(rows)


def _rate(rs: list[HandoffResult], stage: str) -> str:
    """`k/n` over the runs where `stage` was measured, or `n/a`."""
    known = [getattr(r, stage) for r in rs if not r.errored and getattr(r, stage) is not None]
    return f"{sum(known)}/{len(known)}" if known else "n/a"


def _arms(results: list[HandoffResult]) -> list[str]:
    seen = {r.arm for r in results}
    return [a for a in ARMS if a in seen]


def summary(results: list[HandoffResult]) -> list[str]:
    """Markdown lines: the pilot caveat, pass rates by arm and case, then a stage funnel per arm.
    A rate is passes over runs that did not error. Errored runs are counted apart."""
    lines = [FIRST_LINE, ""]
    errored = sum(r.errored for r in results)
    lines.append(f"Runs: {len(results)}. Errored: {errored}. Agent sessions: {sum(r.sessions for r in results)}.")
    models = sorted({f"{r.writer}={r.writer_model}" for r in results if r.writer_model}
                    | {f"{r.reader}={r.reader_model}" for r in results if r.reader_model})
    lines.append(f"Models: {', '.join(models) if models else 'none'}.")
    if not results:
        return lines
    arm_list = _arms(results)
    cases = sorted({r.case for r in results})
    by = defaultdict(list)
    for r in results:
        by[r.arm, r.case].append(r)

    lines += ["", "## Pass rate by arm and case", "",
              "| Arm | " + " | ".join(cases) + " | Errored |", "|---" * (len(cases) + 2) + "|"]
    for arm in arm_list:
        cells = [_rate(by[arm, c], "passed") if by[arm, c] else "" for c in cases]
        n_err = sum(r.errored for c in cases for r in by[arm, c])
        lines.append(f"| {arm} | " + " | ".join(cells) + f" | {n_err} |")

    lines += ["", "## Stage funnel per arm", "",
              "Each stage counts runs that did not error and where the stage applies. Oracle has no capture. "
              "H5 has no delivery. Oracle ceiling is the oracle arm's pass rate on the same cases.", "",
              "| Arm | Captured | Delivered | Passed | Oracle ceiling | Errored |", "|---|---|---|---|---|---|"]
    for arm in arm_list:
        rs = [r for r in results if r.arm == arm]
        arm_cases = {r.case for r in rs}
        oracle = [r for r in results if r.arm == "oracle" and r.case in arm_cases]
        ceiling = _rate(oracle, "passed") if oracle else "n/a"
        lines.append(f"| {arm} | {_rate(rs, 'captured')} | {_rate(rs, 'delivered')} | {_rate(rs, 'passed')} | "
                     f"{ceiling} | {sum(r.errored for r in rs)} |")

    directions = sorted({(r.writer, r.reader) for r in results})
    if len(directions) > 1:
        lines += ["", "## Pass rate per arm and direction", "",
                  "| Arm | " + " | ".join(f"{w} to {rd}" for w, rd in directions) + " |",
                  "|---" * (len(directions) + 1) + "|"]
        for arm in arm_list:
            cells = [_rate([r for r in results if r.arm == arm and (r.writer, r.reader) == d], "passed")
                     for d in directions]
            lines.append(f"| {arm} | " + " | ".join(cells) + " |")
    stages = defaultdict(int)
    for r in results:
        if r.errored:
            stages[r.error_stage] += 1
    if stages:
        lines += ["", "Errored runs by stage: " + ", ".join(f"{s} {n}" for s, n in sorted(stages.items())) + "."]
    return lines
