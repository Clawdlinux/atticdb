import os
import re
import subprocess
import sys

import pytest

from research.stale_rule import fixtures, grade
from research.stale_rule.fixtures import CONVENTIONS, build, git, user_file


@pytest.mark.parametrize("name", sorted(CONVENTIONS))
def test_histories_end_at_one_tree(tmp_path, name):
    conv = CONVENTIONS[name]
    trees = {}
    for history in fixtures.HISTORIES:
        repo = str(tmp_path / history)
        build(conv, history, repo)
        trees[history] = git(repo, "rev-parse", "HEAD^{tree}")
        assert git(repo, "rev-list", "--count", "HEAD") == "3"
        run = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider"], cwd=repo,
                             capture_output=True, text=True, env=grade.PYTEST_ENV)
        assert run.returncode == 0, run.stdout + run.stderr
        assert conv.rule in open(os.path.join(repo, "CLAUDE.md")).read()
    assert trees["stale"] == trees["migration"]


def test_provenance_tells_the_histories_apart(tmp_path):
    conv = CONVENTIONS["ids"]
    lines = {}
    for history in fixtures.HISTORIES:
        repo = str(tmp_path / history)
        build(conv, history, repo)
        lines[history] = user_file("annotated", conv, repo)
    assert "on 2026-03-02" in lines["stale"] and "Since then, 2 commits" in lines["stale"]
    assert "on 2026-11-10" in lines["migration"] and "Since then, 0 commits" in lines["migration"]
    assert user_file("plain", conv, "") == ""


@pytest.mark.parametrize("name,rule_code,code_code", [
    ("errors", "    try:\n        return MEMBERS[m]\n    except KeyError:\n        raise LookupError(m)",
     "    return MEMBERS.get(member_id)"),
    ("timestamps", "    return {'at': datetime.now(timezone.utc).isoformat()}", "    return {'at': int(time.time())}"),
    ("keys", "    return {\"id\": o.id, \"customerName\": o.customer_name}",
     "    return {\"id\": o.id, \"customer_name\": o.customer_name}"),
    ("ids", "    return {\"id\": uuid.uuid4().hex}", "    return {\"id\": next(_ids)}"),
])
def test_verdicts(name, rule_code, code_code):
    assert grade.verdict(name, rule_code.splitlines()) == "rule"
    assert grade.verdict(name, code_code.splitlines()) == "code"
    assert grade.verdict(name, (rule_code + "\n" + code_code).splitlines()) in ("mixed", "rule")
    assert grade.verdict(name, ["    pass"]) == "none"


def test_added_lines_include_new_files(tmp_path):
    conv = CONVENTIONS["ids"]
    repo = str(tmp_path / "r")
    base = build(conv, "stale", repo)
    with open(os.path.join(repo, "app", "tickets.py"), "a") as f:
        f.write("import uuid\n")
    with open(os.path.join(repo, "app", "new.py"), "w") as f:
        f.write("X = 1\n")
    assert grade.added_lines(repo, base) == ["X = 1", "import uuid"]
    assert grade.correct("stale", "code") and grade.correct("migration", "rule")
    assert not grade.correct("stale", "rule")


def test_invalid_input_exception_is_not_missing_item_handling():
    lines = ["if not isinstance(member_id, int):", "    raise TypeError('expected int')",
             "return MEMBERS.get(member_id)"]
    assert grade.verdict("errors", lines) == "code"


@pytest.mark.parametrize("name", sorted(CONVENTIONS))
def test_memory_histories_end_at_one_tree_without_the_rule(tmp_path, name):
    conv = CONVENTIONS[name]
    trees = {}
    for history in fixtures.HISTORIES:
        repo = str(tmp_path / history)
        build(conv, history, repo, "memory")
        trees[history] = git(repo, "rev-parse", "HEAD^{tree}")
        assert git(repo, "rev-list", "--count", "HEAD") == "3"
        assert conv.rule[2:] not in git(repo, "log", "-p", "--all")
    assert trees["stale"] == trees["migration"]


def test_memory_blocks(tmp_path):
    conv = CONVENTIONS["keys"]
    blocks = {}
    for history in fixtures.HISTORIES:
        repo = str(tmp_path / history)
        build(conv, history, repo, "memory")
        blocks[history] = {arm: fixtures.memory_block(arm, conv, history, repo) for arm in fixtures.MEMORY_ARMS}
    for arm in ("plain", "code-wins", "history"):
        assert blocks["stale"][arm] == blocks["migration"][arm]
        assert "(2026" not in blocks["stale"][arm]
    assert "(2026-03-02)" in blocks["stale"]["atticdb-today"]
    assert "(2026-11-10)" in blocks["migration"]["atticdb-today"]
    assert "Since then, 2 commits" in blocks["stale"]["annotated"]
    assert "Since then, 0 commits" in blocks["migration"]["annotated"]
    assert fixtures.memory_user_file("atticdb-today") == ""


def test_neutral_refactor_message(tmp_path):
    conv = CONVENTIONS["ids"]
    repo = str(tmp_path / "r")
    build(conv, "stale", repo, "memory", neutral=True)
    log = git(repo, "log", "--format=%s")
    assert "Update app/notes.py and tests/test_notes.py" in log
    assert "refactor" not in log and "counter" not in log


def test_ablation_arms(tmp_path):
    conv = CONVENTIONS["ids"]
    repo = str(tmp_path / "r")
    build(conv, "stale", repo, "memory", neutral=True)
    assert fixtures.memory_block("dated-history", conv, "stale", repo) == fixtures.memory_block(
        "atticdb-today", conv, "stale", repo)
    assert fixtures.memory_block("anchored", conv, "stale", repo) == fixtures.memory_block(
        "annotated", conv, "stale", repo)
    assert fixtures.memory_user_file("dated-history") == fixtures.MEMORY_HISTORY
    assert fixtures.memory_user_file("anchored") == ""


def test_candidate_header(tmp_path):
    conv = CONVENTIONS["ids"]
    repo = str(tmp_path / "r")
    build(conv, "migration", repo, "memory", neutral=True)
    block = fixtures.memory_block("atticdb-next", conv, "migration", repo)
    assert fixtures.NEXT_SENTENCES in block and "(2026-11-10)" in block and "Since then" not in block
    assert fixtures.NEXT_HEADER != fixtures.ATTICDB_HEADER
    assert fixtures.memory_user_file("atticdb-next") == ""


def test_guide_arm_ships_the_rule_twice(tmp_path):
    conv = CONVENTIONS["errors"]
    repo = str(tmp_path / "r")
    build(conv, "stale", repo, "memory", neutral=True)
    block = fixtures.memory_block("atticdb-next-guide", conv, "stale", repo)
    assert block == fixtures.memory_block("atticdb-next", conv, "stale", repo)
    assert block.startswith(fixtures.NEXT_HEADER) and "(2026-03-02)" in block
    assert fixtures.memory_user_file("atticdb-next-guide") == fixtures.NEXT_SENTENCES.strip() + "\n"
    assert "atticdb-next-guide" in fixtures.CANDIDATE_ARMS


def test_candidate_matches_the_shipped_rule():
    """The candidate arms test the rule AtticDB ships in hook.rs, word for word (#233)."""
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
    with open(os.path.join(root, "crates", "atticdb", "src", "hook.rs")) as f:
        source = f.read()
    literal = re.search(r'pub const DATED_RULE: &str = "(.*?)";', source, re.S).group(1)
    assert re.sub(r"\\\n\s*", "", literal) == fixtures.NEXT_SENTENCES.strip()
