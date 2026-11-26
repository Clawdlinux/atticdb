import json
import os
import re

import pytest

from research.setup_eval import cutfact, harness
from research.setup_eval import scenarios as sc
from research.setup_eval.parse import Call, Outcome, parse_claude

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))


def max_content() -> int:
    """The hook's cut length, read from hook.rs so the fixture can't drift from it."""
    with open(os.path.join(ROOT, "crates", "atticdb", "src", "hook.rs")) as f:
        return int(re.search(r"const MAX_CONTENT: usize = (\d+);", f.read()).group(1))


def sentences(text: str) -> set[str]:
    return {s.strip() for s in re.split(r"(?<=\.)\s+", text) if s.strip()}


@pytest.mark.parametrize("case", sorted(cutfact.CASES))
def test_the_cut_hides_the_whole_exception_and_full_shows_it(case):
    c = cutfact.CASES[case]
    cut, full = cutfact.fact_text(c, "cut"), cutfact.fact_text(c, "full")
    assert cut.index(c.exception) >= max_content() + 20
    assert full.index(c.exception) + len(c.exception) < max_content() - 20
    assert sentences(cut) == sentences(full)
    assert c.token in c.exception and cut.count(c.token) == 1
    assert cutfact.fact_text(c, "none") is None


def test_cases_differ_in_prompt_key_and_token():
    cases = list(cutfact.CASES.values())
    for field in ("prompt", "key", "token"):
        assert len({getattr(c, field) for c in cases}) == len(cases), field


def test_cut_scenarios_are_opt_in():
    assert set(sc.SCENARIOS) == {"S1", "S2", "S3", "S4", "B1", "B2"}
    assert {"C1-CUT", "C1-FULL", "C1-NONE"} <= set(sc.ALL_SCENARIOS)
    assert set(sc.SCENARIOS) <= set(sc.ALL_SCENARIOS)


def test_seed_sets_the_fact_for_cut_and_full_only():
    c = cutfact.CASES["C2"]
    assert sc.seed_commands(sc.ALL_SCENARIOS["C2-CUT"]) == [
        ["fact", "set", "--key", c.key, "--content", cutfact.fact_text(c, "cut")]]
    assert sc.seed_commands(sc.ALL_SCENARIOS["C2-FULL"])[0][-1] == cutfact.fact_text(c, "full")
    assert sc.seed_commands(sc.ALL_SCENARIOS["C2-NONE"]) == []


def test_cut_scenarios_diff_calc_py():
    cfg = harness.Config(atticdb="atticdb", home="/h", claude_dir="/c", codex_dir="/x")
    steps = harness.plan(cfg, "claude-code", sc.ALL_SCENARIOS["C1-CUT"], "/r")
    assert any(s.argv[:2] == ["git", "diff"] and s.argv[-1] == "calc.py" for s in steps["check"])


def diff(*lines: str) -> str:
    return "--- a/calc.py\n+++ b/calc.py\n" + "".join(f"+{line}\n" for line in lines)


EXCEPTION_DIFFS = {
    "C1": diff("def add(a, b):", "    if isinstance(a, bool) or isinstance(b, bool):", "        raise TypeError('bool')", "    return a + b"),
    "C2": diff("def divide(a, b):", "    if b == 0:", "        return None", "    return a / b"),
    "C3": diff("import math", "def power(a, b):", "    return math.pow(a, b)"),
}
NAIVE_DIFFS = {
    "C1": diff("def add(a, b):", "    return a + b"),
    "C2": diff("def divide(a, b):", "    if b == 0:", "        raise ZeroDivisionError('b is 0')", "    return a / b"),
    "C3": diff("def power(a, b):", "    return a ** b"),
}


def outcome(*tools: str, key: str = "") -> Outcome:
    calls = [Call(t, {"key": key} if t in ("fact_get", "fact_history") else {"query": "calc"}) for t in tools
             if t in cutfact.READ_TOOLS]
    return Outcome(calls=calls, tools=list(tools), final=True)


@pytest.mark.parametrize("case", sorted(cutfact.CASES))
def test_judge_reads_the_edit(case):
    c = cutfact.CASES[case]
    good = cutfact.grade(c, outcome("fact_get", "Edit", key=c.key), EXCEPTION_DIFFS[case])
    assert (good.verdict, good.correct, good.read, good.read_before_edit) == ("exception", True, True, True)
    naive = cutfact.grade(c, outcome("Edit", "fact_get", key=c.key), NAIVE_DIFFS[case])
    assert (naive.verdict, naive.correct, naive.read, naive.read_before_edit) == ("naive", False, True, False)
    assert cutfact.grade(c, outcome(), "").verdict == "none"


def test_a_read_of_another_key_is_not_a_read():
    c = cutfact.CASES["C1"]
    g = cutfact.grade(c, outcome("fact_get", "Edit", key="repo/evalrepo/other"), EXCEPTION_DIFFS["C1"])
    assert not g.read and not g.read_before_edit


def test_memory_search_counts_as_a_read():
    c = cutfact.CASES["C3"]
    g = cutfact.grade(c, outcome("memory_search", "Write"), EXCEPTION_DIFFS["C3"])
    assert g.read and g.read_before_edit


def test_verdict_feeds_the_scenario_judge():
    c = cutfact.CASES["C2"]
    v = sc.judge(sc.ALL_SCENARIOS["C2-CUT"], outcome("fact_get", "Edit", key=c.key), "", EXCEPTION_DIFFS["C2"])
    assert v.passed
    assert "verdict=exception" in v.reasons and "read=1" in v.reasons and "read_before_edit=1" in v.reasons
    v = sc.judge(sc.ALL_SCENARIOS["C2-CUT"], outcome("Edit"), "", NAIVE_DIFFS["C2"])
    assert not v.passed and "verdict=naive" in v.reasons and "read=0" in v.reasons


def test_parse_claude_records_every_tool_in_order():
    events = [
        {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "1", "name": "Read", "input": {}}]}},
        {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "2", "name": "mcp__atticdb__fact_get", "input": {"key": "k"}},
            {"type": "tool_use", "id": "2", "name": "mcp__atticdb__fact_get", "input": {"key": "k"}}]}},
        {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "3", "name": "Edit", "input": {}}]}},
        {"type": "result", "result": "done", "usage": {}},
    ]
    out = parse_claude("\n".join(json.dumps(e) for e in events))
    assert out.tools == ["Read", "fact_get", "Edit"]
    assert [c.name for c in out.calls] == ["fact_get"]
