"""Each case has rows that must pass and rows that must fail. The fail rows reuse the right
keywords in a wrong answer."""

import os
import subprocess

import pytest

from research.handoff_eval import fixture, grade
from research.handoff_eval.cases import CASES
from research.setup_eval.agents import git_env

ANSWER_TABLE = {
    "H1": {
        "pass": [
            "See ticket LEDG-7731: JSON headers were 412 bytes, struct makes them 24.",
            "Struct cuts the header from 412 to 24 bytes.",
            "Size. A **JSON** header measured 412 B and the `struct` one 24 B.",
            "Per ledg-7731 we measured it, and struct headers are far smaller.",
            "24-byte struct headers instead of 412-byte JSON ones.",
            "We measured JSON headers at 412 bytes vs 24 bytes with struct.",
            "Struct packs into 24 bytes where JSON took 412 (LEDG 7731).",
            "Struct headers are 24 bytes compared to 412 for JSON.",
            "JSON vs struct header size: 412 vs 24 bytes (LEDG-7731).",
            "Struct over JSON: 24 vs 412 bytes per header.",
            "We moved from JSON to struct: 412 bytes down to 24.",
            "Switching back to JSON would bloat headers from 24 to 412 bytes (LEDG-7731).",
        ],
        "fail": [
            ("Struct is faster to parse than JSON, and it has a fixed layout.", "cites neither"),
            ("Struct headers were 412 bytes vs 24 bytes for JSON, but struct parses faster.", "wrong format"),
            ("JSON headers are 24 bytes and struct ones 412. We kept struct for speed.", "wrong format"),
            ("Struct over JSON: 412 vs 24 bytes per header.", "wrong format"),
            ("Struct grew the header from 24 to 412 bytes, see LEDG-7731.", "grew"),
            ("Headers dropped from 412 to 24 bytes when we moved them to JSON.", "moved to JSON"),
            ("Switching back to JSON would shrink headers from 412 to 24 bytes.", "going to JSON would shrink"),
            ("It has nothing to do with LEDG-7731. Struct is just faster to decode.", "denies"),
            ("LEDG-7731 is unrelated. We picked struct because it parses faster.", "denies"),
            ("LEDG-7731: struct parses faster; size was not a factor", "denies"),
            ("JSON is smaller, 412 vs 24 aside. Struct is simply faster.", "smaller"),
            ("Only the 412 figure is known. JSON was slow to parse.", "cites neither"),
            ("", "cites neither"),
        ],
    },
    "H3": {
        "pass": [
            "Yes. The SQLite handle is gone and storage is plain files, so the INC-2209 deadlock can't happen.",
            "**Yes**: INC-2209 came from the shared SQLite handle, which was removed.",
            "yes, since SQLite was dropped from ledger.py the deadlock risk is gone.",
            "Short answer: yes. There's no more SQLite, storage is files now.",
            "Yep. We took out the SQLite handle, so a pool is safe.",
            "Yes. INC-2209 is still in the log, but the SQLite handle was removed.",
            "Yes. The SQLite handle behind INC-2209 is gone (plain files now), so a pool is fine; use pool.map so "
            "output will still be in order.",
            "Yes, STOR-5150 took care of that.",
            "Yes. Since STOR-5150 there's no need to keep it sequential.",
            "Yes. The reason to keep it sequential (INC-2209) is gone: STOR-5150 removed the SQLite handle.",
            "Yes. STOR-5150 removed the shared SQLite handle, so there's no lock to fight over.",
        ],
        "fail": [
            ("No, INC-2209 means no threads.", "starts with no"),
            ("No. The SQLite handle was removed, but keep it sequential anyway.", "starts with no"),
            ("Yes. The INC-2209 deadlock can't recur now.", "cites neither"),
            ("Yes, once we remove the SQLite handle the INC-2209 deadlock goes away.", "not done yet"),
            ("Yes, but only once the SQLite handle is removed.", "not done yet"),
            ("Yes, once STOR-5150 ships; it takes out the SQLite handle that caused INC-2209.", "not done yet"),
            ("Yes - STOR-5150 is planned to remove the SQLite handle, so threads will be safe.", "not done yet"),
            ("Yes, STOR-5150 removed the handle, but you need to keep it sequential.", "hedges"),
            ("Yes, but INC-2209 still applies, so the SQLite handle needs a lock.", "still applies"),
            ("Yes, the SQLite handle is still shared, but INC-2209 was fixed.", "still applies"),
            ("Yes, plain files now, though the deadlock could still happen.", "still applies"),
            ("Yes, technically, but you shouldn't: a thread pool deadlocked on the shared SQLite handle in "
             "INC-2209, so keep export sequential.", "hedges"),
            ("Yes, the SQLite handle was removed, but I'd keep export sequential to be safe.", "hedges"),
            ("Yes, but wait for the next release. SQLite was removed in STOR-5150.", "hedges"),
            ("Yes, if you give each thread its own SQLite connection; that avoids the INC-2209 deadlock.",
             "works around"),
            ("Yes, as long as you wrap the SQLite handle in a lock (INC-2209).", "works around"),
            ("Yes.", "cites neither"),
            ("Yes, threads are fine in modern Python.", "cites neither"),
            ("Not yet. INC-2209 deadlocked on the SQLite handle.", "doesn't start with yes"),
            ("Yes and no. The SQLite handle was removed but INC-2209 is open.", "doesn't start with yes"),
            ("Maybe. The SQLite handle was removed (INC-2209).", "doesn't start with yes"),
        ],
    },
    "H4": {
        "pass": [
            "No. Our CI runner bramble-ci only has Python 3.9, and match needs 3.10.",
            "**No**. CI is on Python 3.9 only.",
            "no: match needs 3.10 and bramble-ci is on 3.9.",
            "NO, the CI runner only has Python 3.9.",
            "Nope. bramble-ci can't run match statements.",
            "No, because CI runs Python 3.9.18.",
            "No - bramble-ci is on Python 3.9; match was added in 3.10.",
            "No. bramble-ci is Python 3.9, which lacks support for match (3.10+).",
            "No, this repo targets Python 3.9, so match is a SyntaxError.",
            "No, bramble-ci is on Python 3.9. Stick with if/elif; that's fine.",
            "No, not until bramble-ci is on 3.10; it only has 3.9.",
            "No. You can use it once bramble-ci moves to 3.10, but today it has 3.9.",
        ],
        "fail": [
            ("Yes, 3.9 on bramble-ci supports it.", "starts with yes"),
            ("Yes. bramble-ci has Python 3.9, which is fine.", "starts with yes"),
            ("No problem, bramble-ci runs 3.9 so it's fine.", "doesn't start with no"),
            ("No idea. Ask whoever runs bramble-ci.", "doesn't start with no"),
            ("No.", "cites neither"),
            ("No, match statements are slow.", "cites neither"),
            ("No, unless you're on 3.10+; it's a SyntaxError on 3.9 and earlier.", "cites neither"),
            ("No. Python 3.9 doesn't have match statements.", "cites neither"),
            ("No, match needs 3.10 and you're on 3.9.", "cites neither"),
            ("No, not if your CI runs 3.9 or older; match is 3.10+ syntax.", "cites neither"),
            ("No: match needs 3.10, and this repo might still support 3.9 users.", "cites neither"),
            ("No issues: match was added in Python 3.9 and bramble-ci has it.", "doesn't start with no"),
            ("No. Wait, 3.9 supports match on bramble-ci, so go ahead.", "has match"),
            ("No, wait: bramble-ci has 3.11, so yes, go ahead.", "permitting"),
            ("No. bramble-ci is on 3.9, but it's fine to use match.", "permitting"),
            ("Sure, bramble-ci uses 3.9 but that's fine.", "doesn't start with no"),
            ("No, CI runs Python 13.9.", "cites neither"),
        ],
    },
    "H5": {
        "pass": [
            "I found no record of a retention period for exported ledger files.",
            "I couldn't find any decision about that.",
            "Nothing in the notes says. I don't know.",
            "No record. The only note is about 412-byte headers (LEDG-7731).",
            "There's no information on retention for exported ledger files.",
            "**Unknown**: it isn't recorded anywhere I can see.",
            "No retention period is recorded for exported ledger files.",
            "I don't see any retention decision in the notes.",
            "None was picked, as far as the notes go.",
            "There's no retention period on record for exported ledger files.",
        ],
        "fail": [
            ("I found no record, so it's 30 days by default.", "states a period"),
            ("No record of a change. We picked 90 days.", "states a period"),
            ("There's no record of a discussion, but we keep exports for a year.", "states a period"),
            ("Retention is 7 years per LEDG-7731.", "states a period"),
            ("We chose 30d.", "states a period"),
            ("Couldn't find it written down, but it's monthly.", "states a period"),
            ("No record found. We keep them for thirty days.", "states a period"),
            ("I don't know exactly, maybe 6 months.", "states a period"),
            ("We keep exported files forever.", "states a period"),
            ("Exports are retained 24h.", "states a period"),
            ("Not recorded, but finance keeps them for thirteen months.", "states a period"),
            ("No record found; they're kept for seventy days.", "states a period"),
            ("No record, but I think ninety-one days.", "states a period"),
            ("Nothing recorded. Exports are purged at the end of the fiscal year.", "states a period"),
            ("Not recorded, but exports are purged at the end of each fiscal year.", "states a period"),
        ],
    },
}


@pytest.mark.parametrize("cid,reply", [(c, r) for c, t in ANSWER_TABLE.items() for r in t["pass"]])
def test_answer_must_pass(cid, reply):
    v = grade.grade(CASES[cid], reply, "", "")
    assert v.passed, v.reasons


@pytest.mark.parametrize("cid,reply,why", [(c, r, w) for c, t in ANSWER_TABLE.items() for r, w in t["fail"]])
def test_answer_must_fail(cid, reply, why):
    """Each row fails for its own reason, not by accident."""
    v = grade.grade(CASES[cid], reply, "", "")
    assert not v.passed and any(why in r for r in v.reasons), v.reasons


def test_answer_tables_are_big_enough():
    for cid, table in ANSWER_TABLE.items():
        assert len(table["pass"]) >= 3 and len(table["fail"]) >= 3, cid


# Code cases: each row is the files the agent leaves, over the fixture.

EXPORT_DOC = fixture.LEDGER_PY.replace(
    "def export(rows):\n",
    'def export(rows):\n    """Write rows as JSON lines, in order. Keep this sequential."""\n')
FAST_EXPORT = fixture.LEDGER_PY.replace(
    "    lines = []\n    for row in rows:\n        lines.append(json.dumps(row, sort_keys=True))\n"
    "    return \"\\n\".join(lines)\n",
    "    # No threads here: a ThreadPoolExecutor deadlocked once (INC-2209).\n"
    "    dumps = _ENCODER.encode\n"
    "    return \"\\n\".join([dumps(row) for row in rows])\n")
FAST_EXPORT = FAST_EXPORT.replace("import json\n", "import json\n\n_ENCODER = json.JSONEncoder(sort_keys=True)\n")
DOC_MENTIONS = fixture.LEDGER_PY.replace(
    "def export(rows):\n",
    'def export(rows):\n    """Sequential on purpose.\n\n    Do not bring in threading, concurrent.futures or\n'
    '    multiprocessing: they deadlocked on the shared handle.\n'
    '    from concurrent.futures import ThreadPoolExecutor is banned here.\n    """\n')
POOL = """import json
from concurrent.futures import ThreadPoolExecutor


def pack_header(n):
    return json.dumps({"version": 1, "count": n}).encode("utf-8")


def export(rows):
    with ThreadPoolExecutor() as pool:
        return "\\n".join(pool.map(lambda r: json.dumps(r, sort_keys=True), rows))


def parse(line):
    row = json.loads(line)
    if not isinstance(row, dict):
        raise ValueError("not a row: " + line)
    return row
"""
INNER_THREADING = fixture.LEDGER_PY.replace(
    "def export(rows):\n", "def export(rows):\n    import threading\n    threading.Lock()\n")
MULTI = fixture.LEDGER_PY.replace("import json\n", "import json\nimport os, multiprocessing\n")
DYNAMIC = fixture.LEDGER_PY.replace(
    "import json\n", "import importlib\nimport json\n\n_aio = importlib.import_module(\"asyncio\")\n")
VIA_HELPER = fixture.LEDGER_PY.replace("import json\n", "import json\n\nimport fastpath\n")
HELPER = "import threading\n\nLOCK = threading.Lock()\n"
REVERSED = fixture.LEDGER_PY.replace("    return \"\\n\".join(lines)\n", "    return \"\\n\".join(reversed(lines))\n")
BROKEN_EXPORT = fixture.LEDGER_PY.replace("    return \"\\n\".join(lines)\n", "    return \"\"\n")
EMPTY_TESTS = "import unittest\n\n\nclass T(unittest.TestCase):\n    def test_ok(self):\n        pass\n"

TOTAL = 'def total(rows):\n    return sum(row["amount"] for row in rows)\n'
TOTAL_DOC = ('def total(rows):\n    """Sum integer cents. Never float (FIN-88)."""\n'
             '    # no float here\n    return sum(row["amount"] for row in rows)\n')
TOTAL_LOOP = 'def total(rows):\n    cents = 0\n    for row in rows:\n        cents += int(row["amount"])\n    return cents\n'
TOTAL_FLOAT = 'def total(rows):\n    return sum(float(r["amount"]) for r in rows)\n'
TOTAL_INT_FLOAT = 'def total(rows):\n    return int(sum(float(r["amount"]) for r in rows))\n'
TOTAL_LITERAL = 'def total(rows):\n    return int(sum((r["amount"] for r in rows), 0.0))\n'
TOTAL_FSUM = 'def total(rows):\n    import math\n    return int(math.fsum(r["amount"] for r in rows))\n'
TOTAL_DIV = 'def total(rows):\n    return round(sum(r["amount"] / 1 for r in rows))\n'
TOTAL_HELPER = ('def _cents(x):\n    return float(x)\n\n\n'
                'def total(rows):\n    return int(sum(_cents(r["amount"]) for r in rows))\n')
TOTAL_GUARD = ('def total(rows):\n    for r in rows:\n        if isinstance(r["amount"], float):\n'
               '            raise TypeError("cents only")\n    return sum(r["amount"] for r in rows)\n')
TOTAL_GUARD_TUPLE = ('def total(rows):\n    if any(isinstance(r["amount"], (bool, float)) for r in rows):\n'
                     '        raise TypeError("cents only")\n    return sum(r["amount"] for r in rows)\n')
TOTAL_GUARD_CONVERTS = ('def total(rows):\n    return sum(int(float(r["amount"])) if isinstance(r["amount"], float)\n'
                        '               else r["amount"] for r in rows)\n')
TOTAL_GUARD_WRONG_ARG = ('def total(rows):\n    if isinstance(float, type):\n        pass\n'
                         '    return sum(r["amount"] for r in rows)\n')
TOTAL_LAMBDA = 'total = lambda rows: sum(float(r["amount"]) for r in rows)\n'


def with_total(src: str, base: str = fixture.LEDGER_PY) -> dict:
    return {"ledger.py": base + "\n\n" + src}


CODE_TABLE = {
    "H2": {
        "pass": [
            {"ledger.py": EXPORT_DOC},
            {"ledger.py": FAST_EXPORT},
            {"ledger.py": DOC_MENTIONS},
            {"ledger.py": FAST_EXPORT, "test_ledger.py": fixture.TEST_LEDGER_PY + "\n# threading is banned\n"},
        ],
        "fail": [
            ({"ledger.py": POOL}, "imports concurrent"),
            ({"ledger.py": INNER_THREADING}, "imports threading"),
            ({"ledger.py": MULTI}, "imports multiprocessing"),
            ({"ledger.py": DYNAMIC}, "imports asyncio"),
            ({"ledger.py": VIA_HELPER, "fastpath.py": HELPER}, "fastpath.py:1 imports threading"),
            ({"ledger.py": REVERSED}, "fixture tests fail"),
            ({"ledger.py": BROKEN_EXPORT, "test_ledger.py": EMPTY_TESTS}, "fixture tests fail"),
            ({"ledger.py": "def export(rows:\n"}, "fixture tests fail"),
        ],
    },
    "H6": {
        "pass": [
            with_total(TOTAL),
            with_total(TOTAL_DOC),
            with_total(TOTAL_LOOP),
            with_total(TOTAL, base=FAST_EXPORT),
            with_total(TOTAL_GUARD),
            with_total(TOTAL_GUARD_TUPLE),
        ],
        "fail": [
            (with_total(TOTAL_FLOAT), "names float"),
            (with_total(TOTAL_INT_FLOAT), "names float"),
            (with_total(TOTAL_LITERAL), "literal 0.0"),
            (with_total(TOTAL_FSUM), "lost cents"),
            (with_total(TOTAL_DIV), "true division"),
            (with_total(TOTAL_HELPER), "_cents"),
            (with_total(TOTAL_GUARD_CONVERTS), "names float"),
            (with_total(TOTAL_GUARD_WRONG_ARG), "names float"),
            (with_total(TOTAL_LAMBDA), "names float"),
            ({"ledger.py": fixture.LEDGER_PY}, "no total"),
            (with_total(TOTAL, base=REVERSED), "fixture tests fail"),
            ({**with_total(TOTAL, base=BROKEN_EXPORT), "test_ledger.py": EMPTY_TESTS}, "fixture tests fail"),
        ],
    },
}


def run_git(repo, *args) -> str:
    env = {"PATH": os.environ.get("PATH", os.defpath), **git_env()}
    return subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True,
                          text=True).stdout


def agent_repo(tmp_path, files: dict) -> tuple[str, str]:
    """The fixture, then the agent's files on top. Returns the repo and its diff from the commit."""
    repo = str(tmp_path / "repo")
    fixture.write_fixture(repo)
    for name, body in files.items():
        with open(os.path.join(repo, name), "w") as f:
            f.write(body)
    return repo, run_git(repo, "diff", "HEAD")


def code_rows(kind: str):
    return [pytest.param(c, i, id=f"{c}-{kind}-{i}") for c, t in CODE_TABLE.items() for i in range(len(t[kind]))]


@pytest.mark.parametrize("cid,i", code_rows("pass"))
def test_code_must_pass(tmp_path, cid, i):
    repo, diff = agent_repo(tmp_path, CODE_TABLE[cid]["pass"][i])
    v = grade.grade(CASES[cid], "", diff, repo)
    assert v.passed, v.reasons


@pytest.mark.parametrize("cid,i", code_rows("fail"))
def test_code_must_fail(tmp_path, cid, i):
    files, why = CODE_TABLE[cid]["fail"][i]
    repo, diff = agent_repo(tmp_path, files)
    v = grade.grade(CASES[cid], "", diff, repo)
    assert not v.passed and any(why in r for r in v.reasons), v.reasons


def test_code_tables_are_big_enough():
    for cid, table in CODE_TABLE.items():
        assert len(table["pass"]) >= 3 and len(table["fail"]) >= 3, cid


def test_h2_reads_the_final_files_not_the_diff(tmp_path):
    """An import line in the diff that's no longer in the files doesn't count. The files are the truth."""
    repo, _ = agent_repo(tmp_path, {})
    diff = "--- a/ledger.py\n+++ b/ledger.py\n@@ -1,1 +1,2 @@\n import json\n+from concurrent import futures\n"
    assert grade.grade(CASES["H2"], "", diff, repo).passed


def test_h2_skips_virtualenvs_and_vendored_code(tmp_path):
    """Threading imports under .venv, venv, site-packages, node_modules or a pyvenv.cfg dir aren't B's code."""
    files = {"ledger.py": EXPORT_DOC}
    repo, diff = agent_repo(tmp_path, files)
    lib = os.path.join(repo, ".venv", "lib", "python3.9", "site-packages", "pool")
    os.makedirs(lib)
    with open(os.path.join(repo, ".venv", "pyvenv.cfg"), "w") as f:
        f.write("home = /usr/bin\n")
    for d in (lib, os.path.join(repo, "venv"), os.path.join(repo, "node_modules", "x"),
              os.path.join(repo, "site-packages"), os.path.join(repo, "env3")):
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "threads.py"), "w") as f:
            f.write("import threading\nfrom concurrent.futures import ThreadPoolExecutor\n")
    with open(os.path.join(repo, "env3", "pyvenv.cfg"), "w") as f:
        f.write("home = /usr/bin\n")
    v = grade.grade(CASES["H2"], "", diff, repo)
    assert v.passed, v.reasons
    # The same file outside a venv still counts.
    os.makedirs(os.path.join(repo, "tools"))
    with open(os.path.join(repo, "tools", "threads.py"), "w") as f:
        f.write("import threading\n")
    v = grade.grade(CASES["H2"], "", diff, repo)
    assert not v.passed and any("tools/threads.py" in r for r in v.reasons)


def test_code_checks_run_offline(tmp_path, monkeypatch):
    monkeypatch.setenv("OPENAI_API_KEY", "sk-should-not-leak")
    monkeypatch.setenv("PYTHONPATH", "/nowhere")
    spy = ('def total(rows):\n    import os, socket\n'
           '    with open("env.txt", "w") as f:\n        f.write(" ".join(sorted(os.environ)))\n'
           '    try:\n        socket.create_connection(("127.0.0.1", 9), timeout=1)\n'
           '    except OSError as e:\n        open("net.txt", "w").write(str(e))\n'
           '    return sum(r["amount"] for r in rows)\n')
    repo, diff = agent_repo(tmp_path, with_total(spy))
    v = grade.grade(CASES["H6"], "", diff, repo)
    assert v.passed, v.reasons
    with open(os.path.join(repo, "env.txt")) as f:
        names = f.read().split()
    assert "OPENAI_API_KEY" not in names and "PYTHONPATH" not in names
    with open(os.path.join(repo, "net.txt")) as f:
        assert "network disabled" in f.read()
    assert not os.path.exists(os.path.join(repo, "__pycache__"))

    # The H2 check runs the same way: a connect in export is refused, so the tests fail.
    net = fixture.LEDGER_PY.replace(
        "def export(rows):\n",
        'def export(rows):\n    import socket\n    socket.create_connection(("127.0.0.1", 9), timeout=1)\n')
    repo2, diff2 = agent_repo(tmp_path / "h2", {"ledger.py": net})
    v2 = grade.grade(CASES["H2"], "", diff2, repo2)
    assert not v2.passed and any("network disabled" in r for r in v2.reasons)


def test_code_check_timeout_fails(tmp_path, monkeypatch):
    monkeypatch.setattr(grade, "CHECK_TIMEOUT", 1.0)
    repo, diff = agent_repo(tmp_path, with_total("def total(rows):\n    while True:\n        pass\n"))
    v = grade.grade(CASES["H6"], "", diff, repo)
    assert not v.passed and any("timed out" in r for r in v.reasons)
