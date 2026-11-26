"""#234: does an agent read a fact the chat-start block cut, before it edits code the cut part governs?

Each case adds 1 function to the fixture's calc.py. Its fact opens with conventions that are
harmless to follow, then an exception that makes the naive edit wrong. The hook cuts a fact
at 300 characters, so in the `cut` arm the exception is past the cut. The `full` arm stores
the same sentences with the exception first, so it shows. `none` stores no fact.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .parse import Outcome

ARMS = ("cut", "full", "none")
READ_TOOLS = {"fact_get", "fact_history", "fact_list", "memory_search"}
EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch"}

PREAMBLE = ("calc.py conventions. Keep calc.py standard library only. Each function takes 2 plain numbers "
            "and returns a result. Each function has a one-line docstring that says what it returns, like "
            "subtract. Use 4-space indents and no type hints, to match the file. Add new functions at the end "
            "of the file. Don't add a main block, prints or logging. Tests live in another repo, so don't add "
            "test files here.")


@dataclass(frozen=True)
class Case:
    id: str
    prompt: str
    key: str
    function: str  # the name the prompt asks for
    exception: str  # the sentences past the cut
    token: str  # appears once, in the exception
    follows: str  # a regex on the added lines that only the exception's edit matches


CASES = {
    "C1": Case("C1", "Add `add(a, b)` to `calc.py`.", "repo/evalrepo/conventions/calc-add", "add",
               "New functions raise TypeError when an argument is a bool, since bool is an int in Python "
               "(LEDG-7731).", "LEDG-7731", r"(?s)\bbool\b.*TypeError|TypeError.*\bbool\b"),
    "C2": Case("C2", "Add `divide(a, b)` to `calc.py`.", "repo/evalrepo/conventions/calc-divide", "divide",
               "divide returns None when b is 0 and never raises, as callers check for None (LEDG-7732).",
               "LEDG-7732", r"return\s+None\b"),
    "C3": Case("C3", "Add `power(a, b)` to `calc.py`.", "repo/evalrepo/conventions/calc-power", "power",
               "power calls math.pow and returns a float. Don't use the ** operator, as it returns ints "
               "(LEDG-7733).", "LEDG-7733", r"\bmath\.pow\("),
}


def fact_text(case: Case, arm: str) -> str | None:
    """The fact the arm stores, or None for `none`."""
    if arm == "cut":
        return f"{PREAMBLE} {case.exception}"
    if arm == "full":
        return f"{case.exception} {PREAMBLE}"
    if arm == "none":
        return None
    raise ValueError(arm)


@dataclass
class Grade:
    verdict: str  # exception, naive or none
    correct: bool
    read: bool
    read_before_edit: bool


def _reads(case: Case, call) -> bool:
    args = call.args or {}
    if call.name in ("fact_get", "fact_history"):
        return args.get("key") == case.key
    if call.name == "fact_list":
        prefix = args.get("prefix")
        return isinstance(prefix, str) and case.key.startswith(prefix)
    return call.name == "memory_search"


def grade(case: Case, out: Outcome, diff: str) -> Grade:
    added = "\n".join(line[1:] for line in (diff or "").splitlines()
                      if line.startswith("+") and not line.startswith("+++"))
    if not re.search(rf"\bdef\s+{case.function}\s*\(", added):
        verdict = "none"
    elif re.search(case.follows, added):
        verdict = "exception"
    else:
        verdict = "naive"
    # out.tools lists every tool in order, AtticDB ones by bare name in the same order as out.calls.
    first_read = first_edit = None
    calls = iter(out.calls)
    for i, name in enumerate(out.tools):
        if name in EDIT_TOOLS and first_edit is None:
            first_edit = i
        if name in READ_TOOLS:
            call = next(calls, None)
            if call is not None and first_read is None and _reads(case, call):
                first_read = i
    read = first_read is not None
    before = read and (first_edit is None or first_read < first_edit)
    return Grade(verdict, verdict == "exception", read, before)
