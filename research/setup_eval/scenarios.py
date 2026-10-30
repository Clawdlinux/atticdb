"""The scenarios, their seeds and their pass rules.

Every text match is a case-insensitive substring match. Each rule returns whether the run
passed and short reasons for the notes column, never the reply itself.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .parse import Outcome

REPO = "evalrepo"
FACT_PREFIX = f"repo/{REPO}/"

TEST_RUNNER_KEY = f"{FACT_PREFIX}test-runner"
TEST_RUNNER_FACT = "This repo runs its tests with cargo nextest run."
REDIS_NOTE = "We dropped Redis because its 2 extra round trips per append broke the 1 ms p99 budget."
# Any of these in the reply counts as giving the seeded reason.
REDIS_MARKERS = ("round trip", "round-trip", "roundtrip", "p99")
FILLER_NOTES = [
    "Renamed the helper in calc.py so the name says what it returns.",
    "Reviewed the README wording; no change needed.",
    "Checked that calc.py has no unused imports.",
    "Talked about adding type hints later; not started.",
    "Looked at the line length in calc.py; it fits in 100 columns.",
    "Agreed to keep calc.py free of global state.",
    "Noted that the first commit is the only one so far.",
    "Discussed a docstring style; picked short plain sentences.",
    "Confirmed calc.py runs under Python 3.",
    "Left the folder layout as it is for now.",
]
ADD_LINE = "def add(a, b)"
NOTE_PREFIX_CHARS = 60  # how much of a saved note the block must show


@dataclass(frozen=True)
class Scenario:
    id: str
    prompt: str
    seed: str  # "recall", "search" or "none"
    rule: str  # "recall", "search", "save" or "summary"


SCENARIOS = {
    "S1": Scenario("S1", "How do I run this repo's tests? 1 line.", "recall", "recall"),
    "S2": Scenario("S2", "Why did we drop Redis here? 1 line.", "search", "search"),
    "S3": Scenario("S3", "Remember for this repo: always pin Node 22.", "none", "save"),
    "S4": Scenario("S4", "Add `add(a, b)` to `calc.py`.", "none", "summary"),
    # Baselines: the S1 and S2 prompts with the same setup and an empty store.
    "B1": Scenario("B1", "How do I run this repo's tests? 1 line.", "none", "recall"),
    "B2": Scenario("B2", "Why did we drop Redis here? 1 line.", "none", "search"),
}

# The block checks re-run the hook after the agent, for these rules only.
NEEDS_BLOCK = {"save", "summary"}

CALC_PY = 'def subtract(a, b):\n    """a minus b."""\n    return a - b\n'
README_MD = "# evalrepo\n\nA small calculator module.\n"


def seed_commands(scenario: Scenario) -> list[list[str]]:
    """The atticdb args that seed the store, run with cwd = the fixture repo."""
    if scenario.seed == "recall":
        return [["fact", "set", "--key", TEST_RUNNER_KEY, "--content", TEST_RUNNER_FACT]]
    if scenario.seed == "search":
        notes = [REDIS_NOTE] + FILLER_NOTES
        return [["append", "--content", note] for note in notes]
    return []


@dataclass
class Verdict:
    passed: bool
    reasons: list[str]


def _has(text: str, needle: str) -> bool:
    return needle.lower() in (text or "").lower()


def check_recall(out: Outcome) -> Verdict:
    ok = _has(out.reply, "nextest")
    return Verdict(ok, [] if ok else ["reply lacks nextest"])


def check_search(out: Outcome) -> Verdict:
    searched = any(c.name == "memory_search" for c in out.calls)
    reason = any(_has(out.reply, m) for m in REDIS_MARKERS)
    reasons = ([] if searched else ["no memory_search call"]) + ([] if reason else ["reply lacks the reason"])
    return Verdict(searched and reason, reasons)


def _block_lines(block: str) -> list[str]:
    return [line.strip() for line in (block or "").splitlines()]


def check_save(out: Outcome, block: str) -> Verdict:
    """A `fact_set` under `repo/evalrepo/` with content, and the next block shows that key with
    `22` in its line."""
    keys = [c.args.get("key") for c in out.calls
            if c.name == "fact_set" and c.args and isinstance(c.args.get("key"), str)
            and c.args["key"].startswith(FACT_PREFIX) and isinstance(c.args.get("content"), str)
            and not c.args.get("delete")]
    if not keys:
        return Verdict(False, [f"no fact_set under {FACT_PREFIX}"])
    lines = _block_lines(block)
    shown = any(line.startswith(f"- {key}:") and "22" in line for key in keys for line in lines)
    return Verdict(shown, [] if shown else ["the next block lacks the fact"])


def added_lines(diff: str) -> list[str]:
    return [line[1:] for line in (diff or "").splitlines() if line.startswith("+") and not line.startswith("+++")]


def check_summary(out: Outcome, block: str, diff: str) -> Verdict:
    """`calc.py` gained `def add(a, b)`, a `memory_append` had no session, and the next block
    shows the start of that note. atticdb reads a `session` of "" or null as none, and models
    fill optional fields with "", so those count as no session."""
    changed = any(_has(line, ADD_LINE) for line in added_lines(diff))
    notes = [c.args["content"] for c in out.calls
             if c.name == "memory_append" and c.args is not None and not c.args.get("session")
             and isinstance(c.args.get("content"), str)]
    flat = " ".join(re.sub(r"[\r\n]", " ", block or "").split()).lower()
    shown = any(" ".join(n.replace("\r", " ").replace("\n", " ").split())[:NOTE_PREFIX_CHARS].lower() in flat
                for n in notes if n.strip())
    reasons = (([] if changed else [f"calc.py lacks {ADD_LINE}"])
               + ([] if notes else ["no memory_append without session"])
               + ([] if shown or not notes else ["the next block lacks the note"]))
    return Verdict(changed and bool(notes) and shown, reasons)


def judge(scenario: Scenario, out: Outcome, block: str = "", diff: str = "") -> Verdict:
    if scenario.rule == "recall":
        return check_recall(out)
    if scenario.rule == "search":
        return check_search(out)
    if scenario.rule == "save":
        return check_save(out, block)
    return check_summary(out, block, diff)
