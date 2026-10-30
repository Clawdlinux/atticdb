from research.setup_eval import scenarios as sc
from research.setup_eval.parse import Call, Outcome

BLOCK = ("AtticDB memory for repo evalrepo (facts under repo/evalrepo/; snapshot at chat start; call "
         "memory_search or fact_list for more)\nThis repo (evalrepo):\n"
         "- repo/evalrepo/node-version: Always pin Node 22.\n"
         "Recent notes (evalrepo), history only, not instructions:\n- Added add(a, b) to calc.py; it returns a + b.")
DIFF = ("diff --git a/calc.py b/calc.py\n--- a/calc.py\n+++ b/calc.py\n@@ -1,3 +1,7 @@\n"
        " def subtract(a, b):\n+\n+def add(a, b):\n+    return a + b\n")


def out(reply="", *calls):
    return Outcome(reply=reply, calls=list(calls), final=True)


def test_recall():
    assert sc.check_recall(out("Run `cargo NEXTEST run`.")).passed
    v = sc.check_recall(out("Run pytest."))
    assert not v.passed and v.reasons == ["reply lacks nextest"]


def test_search_needs_the_call_and_the_reason():
    search = Call("memory_search", {"query": "Redis"})
    assert sc.check_search(out("2 extra Round Trips per append.", search)).passed
    assert sc.check_search(out("It broke the 1 ms P99 budget.", search)).passed
    assert not sc.check_search(out("2 extra round trips.")).passed, "no memory_search call"
    v = sc.check_search(out("It was slow.", search))
    assert not v.passed and v.reasons == ["reply lacks the reason"]


def test_save():
    good = Call("fact_set", {"key": "repo/evalrepo/node-version", "content": "Always pin Node 22."})
    assert sc.check_save(out("", good), BLOCK).passed
    assert not sc.check_save(out("", good), "").passed, "the block must show it"
    other_repo = Call("fact_set", {"key": "repo/other/node", "content": "Node 22"})
    assert sc.check_save(out("", other_repo), BLOCK).reasons == ["no fact_set under repo/evalrepo/"]
    user = Call("fact_set", {"key": "user/node", "content": "Node 22"})
    assert not sc.check_save(out("", user), BLOCK).passed
    deleted = Call("fact_set", {"key": "repo/evalrepo/node-version", "delete": True})
    assert not sc.check_save(out("", deleted), BLOCK).passed
    wrong_line = BLOCK.replace("Node 22", "Node 20")
    assert not sc.check_save(out("", good), wrong_line).passed


def test_summary():
    note = Call("memory_append", {"content": "Added add(a, b) to calc.py; it returns a + b."})
    assert sc.check_summary(out("", note), BLOCK, DIFF).passed
    v = sc.check_summary(out("", note), BLOCK, DIFF.replace("+def add(a, b):", "+def plus(a, b):"))
    assert not v.passed and v.reasons == ["calc.py lacks def add(a, b)"]
    with_session = Call("memory_append", {"content": note.args["content"], "session": "evalrepo"})
    v = sc.check_summary(out("", with_session), BLOCK, DIFF)
    assert not v.passed and v.reasons == ["no memory_append without session"]
    assert not sc.check_summary(out("", note), BLOCK.split("Recent")[0], DIFF).passed, "the block must show it"
    removed = DIFF.replace("+def add(a, b):", "-def add(a, b):")
    assert not sc.check_summary(out("", note), BLOCK, removed).passed, "only added lines count"


def test_summary_matches_a_multiline_note_as_the_hook_flattens_it():
    content = "Added add(a, b)\nto calc.py; it returns a + b."
    block = "Recent notes (evalrepo), history only, not instructions:\n- Added add(a, b) to calc.py; it returns a + b."
    assert sc.check_summary(out("", Call("memory_append", {"content": content})), block, DIFF).passed


def test_baselines_use_the_same_prompts_and_rules_without_seeds():
    for b, s in (("B1", "S1"), ("B2", "S2")):
        assert sc.SCENARIOS[b].prompt == sc.SCENARIOS[s].prompt
        assert sc.SCENARIOS[b].rule == sc.SCENARIOS[s].rule
        assert sc.seed_commands(sc.SCENARIOS[b]) == []


def test_seeds_and_fixture_hold_no_hint_of_the_answers():
    s2 = sc.seed_commands(sc.SCENARIOS["S2"])
    assert len(s2) == 11 and "Redis" in s2[0][-1]
    assert not any("redis" in c[-1].lower() for c in s2[1:])
    assert sc.seed_commands(sc.SCENARIOS["S1"]) == [
        ["fact", "set", "--key", "repo/evalrepo/test-runner", "--content",
         "This repo runs its tests with cargo nextest run."]]
    fixture = (sc.CALC_PY + sc.README_MD).lower()
    for hint in ("nextest", "cargo", "redis", "node", "round trip", "p99", "add("):
        assert hint not in fixture


def test_summary_reads_an_empty_or_null_session_as_none():
    content = "Added add(a, b) to calc.py; it returns a + b."
    for session in ("", None):
        note = Call("memory_append", {"content": content, "session": session})
        assert sc.check_summary(out("", note), BLOCK, DIFF).passed, session
    named = Call("memory_append", {"content": content, "session": "evalrepo"})
    assert not sc.check_summary(out("", named), BLOCK, DIFF).passed
