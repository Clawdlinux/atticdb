"""The results CSV, the markdown summary, and a re-grade of a CSV's stored replies.

The CSV's `reply` column holds the reader's whole reply, redacted: token shapes and the run's paths
are replaced. No column holds a token or an env value."""

from __future__ import annotations

import csv
import os
import re
from collections import defaultdict
from dataclasses import dataclass, field

from .arms import ARMS
from .cases import CASES
from .grade import H4_REGRADE_NOTE, grade
from .run import HandoffResult, all_in_one_place, parse_where

FIRST_LINE = "Integration pilot. Not a statistical result."

COLUMNS = ["date", "case", "arm", "writer", "reader", "run", "captured", "captured_where", "captured_all",
           "delivered", "delivered_where", "leaked", "passed", "check_timed_out", "errored", "error_stage", "sessions",
           "input_tokens", "output_tokens", "wall_s", "writer_model", "reader_model", "reasons", "reply"]


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
        "captured_all": flag(res.captured_all),
        "delivered": flag(res.delivered),
        "delivered_where": res.delivered_where,
        "leaked": flag(res.leaked),
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
        "reply": res.reply,
    }


def check_header(path: str) -> None:
    """Raises ValueError when `path` already has rows under other columns, such as a CSV from before
    `captured_all` and `reply`. Appending there would put values under the wrong names."""
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return
    with open(path, newline="") as f:
        header = next(csv.reader(f), [])
    if header != COLUMNS:
        raise ValueError(f"{path} has other columns than this version writes, so rows can't be appended. "
                         "Use a new --out")


def append(path: str, rows: list[dict]) -> None:
    """Appends `rows`, with the header first when the file is new or empty. Each run's row is
    written as soon as it ends, so a stopped batch keeps the runs that finished."""
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    check_header(path)
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

    lines += ["", "## Captured with reason per arm", "",
              "Captured with reason counts runs where 1 capture place holds every evidence token of the case: "
              "for H1, LEDG-7731 and 412 in 1 note or file. Same runs as Captured.", "",
              "| Arm | Captured | Captured with reason |", "|---|---|---|"]
    for arm in arm_list:
        rs = [r for r in results if r.arm == arm]
        lines.append(f"| {arm} | {_rate(rs, 'captured')} | {_rate(rs, 'captured_all')} |")

    directions = sorted({(r.writer, r.reader) for r in results})
    if len(directions) > 1:
        lines += ["", "## Pass rate per arm and direction", "",
                  "| Arm | " + " | ".join(f"{w} to {rd}" for w, rd in directions) + " |",
                  "|---" * (len(directions) + 1) + "|"]
        for arm in arm_list:
            cells = [_rate([r for r in results if r.arm == arm and (r.writer, r.reader) == d], "passed")
                     for d in directions]
            lines.append(f"| {arm} | " + " | ".join(cells) + " |")
    blank = [r for r in results if r.arm == "blank"]
    if blank:
        known = [r for r in blank if not r.errored and r.leaked is not None]
        lines += ["", "## Control (blank arm)", "",
                  "The blank arm has no writer and no notes. Its pass rate is the chance floor. "
                  "A leak is an evidence token in the reader's reply or tool outputs.", "",
                  f"Pass rate: {_rate(blank, 'passed')}.", f"Leaked: {sum(r.leaked for r in known)} of {len(known)}."]
    stages = defaultdict(int)
    for r in results:
        if r.errored:
            stages[r.error_stage] += 1
    if stages:
        lines += ["", "Errored runs by stage: " + ", ".join(f"{s} {n}" for s, n in sorted(stages.items())) + "."]
    return lines


# ---- re-grade a CSV's stored replies ----

# A CSV from before the `reply` column has these. `captured_all` and `reply` are optional.
REGRADE_COLUMNS = ("case", "arm", "writer", "reader", "run", "captured", "captured_where", "delivered", "passed",
                   "errored", "reasons")
EXCERPT_LIMIT = 200  # guard.excerpt's default, which made the reasons' reply=
# What run_handoff can append after reply=, in its finally block.
AFTER_REPLY = re.compile(r"; (?:disconnect \S+ exited |the run's codex auth\.json |codex refreshed its login"
                         r"|error in teardown: )")
BARE_TURN = "atticdb:turn"  # before turns were named by lsn, every turn was this 1 place


def read(path: str) -> list[dict]:
    """The rows of a results CSV. Raises ValueError when a column a re-grade needs is missing."""
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        missing = [c for c in REGRADE_COLUMNS if c not in (reader.fieldnames or [])]
        if missing:
            raise ValueError(f"{path} has no {', '.join(missing)} column")
        return list(reader)


def reply_from_reasons(reasons: str) -> str | None:
    """The reply excerpt that the reasons end with, or None. It is flattened to 1 line and cut at
    EXCERPT_LIMIT chars, so it may not be the whole reply."""
    starts = [m.end() for m in re.finditer(r"(?:^|; )reply=", reasons or "")]
    if not starts:
        return None
    rest = reasons[starts[-1]:]
    after = AFTER_REPLY.search(rest)
    return (rest[:after.start()] if after else rest)[:EXCERPT_LIMIT]


def _bool(text: str) -> bool | None:
    return None if text in ("", None) else text.strip() == "1"


def _int(text: str) -> int | None:
    return int(text) if text not in ("", None) else None


def from_row(r: dict) -> HandoffResult:
    """A results row back as a HandoffResult, for the summary. Reasons stay 1 string."""
    return HandoffResult(
        r["case"], r["arm"], r["writer"], r["reader"], _int(r["run"]) or 0,
        captured=_bool(r["captured"]), captured_where=r.get("captured_where") or "",
        captured_all=_bool(r.get("captured_all", "")), delivered=_bool(r["delivered"]),
        delivered_where=r.get("delivered_where") or "", leaked=_bool(r.get("leaked", "")),
        passed=_bool(r["passed"]),
        check_timed_out=bool(_bool(r.get("check_timed_out", ""))), errored=bool(_bool(r["errored"])),
        error_stage=r.get("error_stage") or "", reasons=[r["reasons"]] if r["reasons"] else [],
        sessions=_int(r.get("sessions", "")) or 0, input_tokens=_int(r.get("input_tokens", "")),
        output_tokens=_int(r.get("output_tokens", "")),
        wall_s=float(r["wall_s"]) if r.get("wall_s") else None, writer_model=r.get("writer_model") or "",
        reader_model=r.get("reader_model") or "", reply=r.get("reply") or "")


def _verdict(res: HandoffResult) -> str:
    return "error" if res.errored else "n/a" if res.passed is None else "pass" if res.passed else "fail"


@dataclass
class Regrade:
    results: list[HandoffResult]
    changed: list[str] = field(default_factory=list)  # 1 line per verdict that flipped
    from_reply: int = 0  # answer rows graded on the whole reply column
    from_excerpt: int = 0  # answer rows graded on the reasons' reply= excerpt
    at_limit: int = 0  # excerpts EXCERPT_LIMIT chars long, so likely cut off
    changed_cut: int = 0  # changed verdicts graded on a cut excerpt
    kept: list[str] = field(default_factory=list)  # rows whose stored verdict stands, and why
    bare_turn: int = 0  # rows whose captured_all rests on the old, unnamed atticdb:turn place


def regrade(rows: list[dict]) -> Regrade:
    """Answer cases graded again with the current graders, on the stored reply. Code cases keep the
    stored verdict, as the repo they were graded on is gone. Errored rows stay errored.
    captured_all is worked out again from captured_where. Runs no agent and writes nothing."""
    out = Regrade([])
    has_reply = bool(rows) and "reply" in rows[0]
    for r in rows:
        res = from_row(r)
        out.results.append(res)
        label = f"{res.case} {res.arm} {res.writer} to {res.reader} rep {res.run}"
        case = CASES.get(res.case)
        if case is None:
            raise ValueError(f"unknown case {res.case!r} in the CSV")
        if res.captured is not None:
            try:
                found = parse_where(res.captured_where)
            except ValueError as e:
                res.captured_all = None
                out.kept.append(f"{label}: captured_where unreadable, so no captured_all ({e})")
            else:
                res.captured_all = all_in_one_place(found, case.evidence)
                named = [(p, t) for p, t in found if p != BARE_TURN]
                if res.captured_all and not all_in_one_place(named, case.evidence):
                    out.bare_turn += 1
        if res.errored or case.kind != "answer":
            continue
        if has_reply:
            reply, cut = r["reply"], False
            out.from_reply += 1
        else:
            reply = reply_from_reasons(r["reasons"])
            if reply is None:
                out.kept.append(f"{label}: no reply= in the reasons, stored verdict kept")
                continue
            cut = len(reply) >= EXCERPT_LIMIT
            out.from_excerpt += 1
            out.at_limit += cut
        before = _verdict(res)
        verdict = grade(case, reply, "", "")
        res.passed, res.reasons = verdict.passed, verdict.reasons
        if _verdict(res) != before:
            why = f" ({'; '.join(verdict.reasons)})" if verdict.reasons else ""
            note = f" [reply cut at {EXCERPT_LIMIT} chars: the flip may come from the missing text]" if cut else ""
            out.changed.append(f"{label}: {before} -> {_verdict(res)}{why}{note}")
            out.changed_cut += cut
    return out


def regrade_report(path: str, rg: Regrade) -> list[str]:
    """What a re-grade did, then the summary of the re-graded rows."""
    code = sum(1 for r in rg.results if not r.errored and CASES[r.case].kind != "answer")
    lines = [f"Re-grade of {path}: {len(rg.results)} rows. No agent ran and nothing was written.", ""]
    if rg.from_reply:
        lines.append(f"{rg.from_reply} answer rows were graded on the whole reply in the reply column.")
    if rg.from_excerpt:
        lines.append(f"This CSV has no reply column. {rg.from_excerpt} answer rows were graded on the reply= text in "
                     f"the reasons. That text is flattened to 1 line and cut at {EXCERPT_LIMIT} chars, so those "
                     f"replies may be cut off. {rg.at_limit} of them are at the limit. A cut reply can lose a fact "
                     "or a wrong claim, so its verdict can differ from a grade of the whole reply.")
    lines.append(f"Code cases keep their stored verdict, as the repo they were graded on is gone: {code} rows.")
    lines.append(f"Errored rows stay errored: {sum(r.errored for r in rg.results)}.")
    if any(r.case == "H4" for r in rg.results):
        lines.append(H4_REGRADE_NOTE)
    if rg.bare_turn:
        lines.append(f"{rg.bare_turn} rows count as captured with reason only through `{BARE_TURN}`. This CSV "
                     "predates turns named by lsn, so tokens in 2 notes there count as 1 place.")
    lines += [f"Kept: {k}" for k in rg.kept]
    lines += ["", "Changed verdicts:" if rg.changed else "Changed verdicts: none."]
    lines += [f"- {c}" for c in rg.changed]
    if rg.changed_cut:
        lines.append(f"{rg.changed_cut} of {len(rg.changed)} changed verdicts were graded on a cut reply. The summary "
                     "below counts them as re-graded. Check them by hand before trusting the totals.")
    return lines + [""] + summary(rg.results)
