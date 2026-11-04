"""Deterministic graders for the 6 handoff cases.

Answer cases read only B's reply. Each needs the right fact, and fails if the reply uses the right
keywords to say something wrong. Code cases read the diff and the repo B leaves. Their checks run
`python3 -I -B` in a subprocess: the repo as cwd, a scrubbed env, sockets disabled and a 20 s timeout.
"""

from __future__ import annotations

import ast
import os
import re
import signal
import subprocess

from ..setup_eval.agents import CHECK_TIMEOUT as _CHECK_TIMEOUT
from ..setup_eval.scenarios import Verdict
from .cases import Case
from .fixture import TEST_LEDGER_PY

CHECK_TIMEOUT = _CHECK_TIMEOUT

# ---- text helpers ----

YES = {"yes", "yeah", "yep", "yup"}
NO = {"no", "nope", "nah"}
# "No problem" or "No idea" opens a reply but isn't a no.
NOT_A_NO = {"idea", "clue", "record", "records", "information", "info", "problem", "problems", "issue",
            "issues", "worries", "worry", "concern", "concerns", "objection", "objections", "reason", "doubt"}


def _plain(text: str) -> str:
    """Lowercase, without the markdown marks that split words."""
    return re.sub(r"[*`~]", "", text or "").lower()


def _sentences(text: str) -> list[str]:
    return [s for s in re.split(r"(?<=[.!?;])\s+|\n+", text) if s.strip()]


def _lead(reply: str) -> str | None:
    """"yes" or "no" if that's the first word, after markdown and punctuation. Else None."""
    t = re.sub(r"[*_`~#>]", " ", reply or "").lower()
    t = re.sub(r"^[\W_]+", "", t)
    t = re.sub(r"^(?:short |quick )?answer\s*[:.\-]\s*", "", t)
    words = re.findall(r"[a-z]+", t[:120])
    if not words:
        return None
    if words[0] in YES:
        return None if words[1:3] in (["and", "no"], ["or", "no"]) else "yes"
    if words[0] in NO:
        return None if len(words) > 1 and words[1] in NOT_A_NO else "no"
    return None


def _lead_reason(want: str, got: str | None) -> list[str]:
    if got == want:
        return []
    return [f"reply starts with {got}, not {want}" if got else f"reply doesn't start with {want}"]


# ---- H1 reason: LEDG-7731, or 412 (JSON) and 24 (struct) the right way round ----

TICKET = re.compile(r"\bledg[-\s]?7731\b")
N412 = r"(?<![\w.])412(?!\d)"
N24 = r"(?<![\w.])24(?!\d)"
H1_ITEM = re.compile(rf"\bjson\b|\bstructs?\b|{N412}|{N24}")
CONNECT = (r"(?:->|→|=>|\bdown to\b|(?<!compared )(?<!relative )(?<!next )(?<!close )(?<!up )\bto\b"
           r"|\binto\b|\bbecame\b)")
SHRINK = re.compile(rf"{N412}\D{{0,30}}?{CONNECT}\D{{0,20}}?{N24}")
GROW = re.compile(rf"{N24}\D{{0,30}}?{CONNECT}\D{{0,20}}?{N412}")
SIZE_SWAP = re.compile(r"\bjson(?: headers?)? (?:is|are|was|were) (?:much |far |a lot )?"
                       r"(?:smaller|shorter|more compact|tinier|leaner)\b"
                       r"|\bstructs?(?: headers?)? (?:is|are|was|were) (?:much |far |a lot )?"
                       r"(?:bigger|larger|longer|heavier)\b")
H1_DENY = re.compile(r"(?:nothing to do with|not because of|not due to|unrelated to|not related to|not from)\s+"
                     r"(?:the\s+)?(?:ticket\s+)?(?:ledg[-\s]?7731|412|24)\b"
                     r"|\bledg[-\s]?7731 (?:is|was) (?:unrelated|irrelevant|not (?:related|relevant|the reason|why))")


def _h1_pairs(sentence: str) -> list[tuple[str, str]]:
    """Pair each number with the label next to it: "JSON 412 vs 24 struct" -> (json, 412), (struct, 24)."""
    items = [m.group(0).rstrip("s") if m.group(0).startswith("struct") else m.group(0)
             for m in H1_ITEM.finditer(sentence)]
    pairs, i = [], 0
    while i < len(items) - 1:
        a, b = items[i], items[i + 1]
        if a.isdigit() != b.isdigit():
            pairs.append((b, a) if a.isdigit() else (a, b))
            i += 2
        else:
            i += 1
    return pairs


def grade_h1(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = []
    if not (TICKET.search(t) or (re.search(N412, t) and re.search(N24, t))):
        reasons.append("reply cites neither LEDG-7731 nor both 412 and 24")
    for s in _sentences(t):
        if GROW.search(s):
            reasons.append(f"says the header grew from 24 to 412: {s!r}")
            continue
        s = SHRINK.sub(" ", s)
        wrong = [p for p in _h1_pairs(s) if p not in (("json", "412"), ("struct", "24"))]
        if wrong:
            reasons.append(f"pairs the sizes with the wrong format {wrong}: {s!r}")
    if SIZE_SWAP.search(t):
        reasons.append("says JSON is smaller or struct is bigger")
    if H1_DENY.search(t):
        reasons.append("denies the ticket or the sizes as the reason")
    return Verdict(not reasons, reasons)


# ---- H3 changed: yes, with INC-2209 or the SQLite removal, and nothing saying it still applies ----

INC = re.compile(r"\binc[-\s]?2209\b")
REMOVAL = re.compile(
    r"(?:sqlite|handle|database|\bdb\b)[^.]{0,40}\b(?:removed|gone|dropped|deleted|replaced|eliminated|"
    r"ripped out|taken out|no longer)\b"
    r"|\b(?:removed|dropped|deleted|replaced|eliminated|took out|taken out|got rid of|ripped out)\b[^.]{0,30}"
    r"(?:sqlite|handle|database)"
    r"|\bno (?:more )?sqlite\b|\bsqlite[^.]{0,20}\b(?:anymore|any more)\b"
    r"|\bplain files\b|\bfile-based\b|\bflat files\b|\bfiles now\b")
STILL = re.compile(
    r"\bstill (?:applies|apply|holds|hold|exists|exist|there|in place|shared|open|a risk|an issue|a problem|"
    r"relevant|possible|happens?|deadlocks?)\b"
    r"|\b(?:can|could|would|will|may|might) still\b"
    r"|\bis still (?:shared|there|used|in use|around|present)\b"
    r"|\bstill (?:uses?|has|have|shares?)\b[^.]{0,20}\bsqlite")
NOT_YET_REMOVED = re.compile(
    r"\b(?:once|after|if|when|until|before)\b[^.]{0,30}\b(?:remove|drop|replace|delete)\b"
    r"|\b(?:will|to) be (?:removed|dropped|replaced)\b|\b(?:plan|planning|going) to (?:remove|drop)\b")


def grade_h3(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = _lead_reason("yes", _lead(reply))
    if not (INC.search(t) or REMOVAL.search(t)):
        reasons.append("reply cites neither INC-2209 nor the SQLite removal")
    if STILL.search(t):
        reasons.append("says the old deadlock or handle still applies")
    if NOT_YET_REMOVED.search(t):
        reasons.append("treats the SQLite removal as not done yet")
    return Verdict(not reasons, reasons)


# ---- H4 paraphrase: no, with 3.9 or bramble-ci, and no claim that 3.9 has match ----

PY39 = re.compile(r"(?<![\d.])3\.9(?!\d)")
BRAMBLE = re.compile(r"\bbramble[-\s]?ci\b")
PY39_CLAIM = re.compile(r"(?<![\d.])3\.9(?!\d)(?P<gap>[^.]{0,25}?)\b(?:supports?|allows?|handles?|permits?|added|"
                        r"introduced)\b")
MATCH_IN_39 = re.compile(r"\b(?:added|introduced|supported|available) (?:in|since|from|on) (?:python )?"
                         r"3\.9(?!\d)")
NEGATION = re.compile(r"n't|\bnot\b|\bno\b|\bnever\b|\bwithout\b")


def grade_h4(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = _lead_reason("no", _lead(reply))
    if not (PY39.search(t) or BRAMBLE.search(t)):
        reasons.append("reply cites neither Python 3.9 nor bramble-ci")
    claims = [m for m in PY39_CLAIM.finditer(t) if not NEGATION.search(m.group("gap"))]
    if claims or MATCH_IN_39.search(t):
        reasons.append("says Python 3.9 has match")
    return Verdict(not reasons, reasons)


# ---- H5 abstain: says there's no record, and states no period ----

ABSTAIN = re.compile(
    r"\bno (?:record|records|information|info|mention|note|notes|decision|data|entry|entries|trace|evidence|idea)\b"
    r"|\b(?:not|isn't|wasn't|never been|hasn't been) (?:recorded|documented|written down|noted|saved|stored|"
    r"decided|mentioned|specified)\b"
    r"|\b(?:couldn't|could not|can't|cannot|didn't|did not|unable to) (?:find|locate|see)\b"
    r"|\bfound (?:no|nothing)\b|\bnothing (?:recorded|saved|stored|documented|written|noted|in|about|on)\b"
    r"|\b(?:don't|do not|doesn't|does not) (?:know|have|say)\b|\bnot aware\b|\bunknown\b|\bno such\b"
    r"|\bnever (?:decided|picked|chose|set|recorded)\b")
NUMWORD = (r"(?:a|an|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|fifteen|eighteen|twenty|"
           r"thirty|forty|forty-five|fifty|sixty|ninety|hundred)")
UNIT = r"(?:seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|wks?|fortnights?|months?|mos?|quarters?|years?|yrs?)"
NUM_TIME = re.compile(rf"(?<![\w.])\d+(?:\.\d+)?\s*-?\s*(?:{UNIT}|[hdwy])\b|\b{NUMWORD}[\s-]+{UNIT}\b")
PERIOD = re.compile(r"\b(?:hourly|daily|weekly|fortnightly|monthly|quarterly|yearly|annually|annual|indefinitely|"
                    r"forever|permanently|perpetually)\b")


def grade_h5(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = [] if ABSTAIN.search(t) else ["reply doesn't say it found no record"]
    stated = NUM_TIME.search(t) or PERIOD.search(t)
    if stated:
        reasons.append(f"states a period: {stated.group(0)!r}")
    return Verdict(not reasons, reasons)


# ---- code checks ----

PRELUDE = '''import socket, sys
def _no_network(*a, **k):
    raise OSError("network disabled in the grader")
socket.socket.connect = socket.socket.connect_ex = _no_network
socket.create_connection = socket.getaddrinfo = _no_network
'''

REPO_TESTS = '''import unittest
r = unittest.main(module=None, argv=["unittest", "discover", "-s", sys.path[0], "-t", sys.path[0],
                                     "-p", "test*.py"], exit=False)
sys.exit(0 if r.result.testsRun and r.result.wasSuccessful() else 1)
'''

TOTAL_CHECK = '''import ledger
r = ledger.total([{"amount": 1}, {"amount": 2}])
assert type(r) is int and r == 3, "total of 1 and 2 gave %r" % (r,)
big = 10 ** 17 + 1
r = ledger.total([{"amount": big}, {"amount": 1}])
assert type(r) is int and r == big + 1, "total lost cents on big amounts: %r" % (r,)
'''


def _summary(out: str) -> str:
    lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
    errors = list(dict.fromkeys(ln for ln in lines if re.match(r"^[\w.]+(?:Error|Exception|Exit)\b", ln)))
    return "; ".join(errors or lines[-3:])[:500]


def run_python(repo: str, code: str) -> tuple[bool, str]:
    """Run `code` under python3 in `repo`. Returns (exit 0, why not)."""
    env = {"PATH": os.environ.get("PATH", os.defpath), "HOME": repo}
    script = PRELUDE + f"sys.path.insert(0, {os.path.abspath(repo)!r})\n" + code
    proc = subprocess.Popen(["python3", "-I", "-B", "-X", "utf8", "-c", script], cwd=repo, env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True)
    try:
        out, _ = proc.communicate(timeout=CHECK_TIMEOUT)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        return False, f"timed out after {CHECK_TIMEOUT:g} s"
    if proc.returncode == 0:
        return True, ""
    return False, _summary(out.decode("utf-8", "replace")) or f"exit {proc.returncode}"


def _test_reasons(repo: str) -> list[str]:
    """The fixture's own tests, which B can't edit, then whatever tests the repo now has."""
    reasons = []
    for name, code in (("fixture tests", TEST_LEDGER_PY), ("repo tests", REPO_TESTS)):
        ok, why = run_python(repo, code)
        if not ok:
            reasons.append(f"{name} fail: {why}")
    return reasons


# ---- H2 rejected: tests pass, and nothing imports a thread, process or async module ----

BANNED = ("threading", "_thread", "concurrent", "multiprocessing", "asyncio")
_B = "|".join(BANNED)
DIFF_IMPORT = re.compile(rf"^\+(?!\+\+)\s*(?:import\s+[^#\n]*?\b(?:{_B})\b|from\s+(?:{_B})\b)"
                         rf"|^\+(?!\+\+).*\b(?:__import__|import_module)\(\s*['\"](?:{_B})\b", re.M)


def _py_files(repo: str):
    for dirpath, dirnames, filenames in os.walk(repo):
        dirnames[:] = [d for d in dirnames if d != ".git"]
        for fn in filenames:
            if fn.endswith(".py"):
                yield os.path.join(dirpath, fn)


def _parse(path: str) -> ast.Module | None:
    try:
        with open(path, encoding="utf-8") as f:
            return ast.parse(f.read(), filename=path)
    except (OSError, SyntaxError, ValueError, UnicodeDecodeError):
        return None


def _callee(func: ast.expr) -> str:
    return func.id if isinstance(func, ast.Name) else func.attr if isinstance(func, ast.Attribute) else ""


def banned_imports(repo: str) -> list[str]:
    """Imports of a banned module in any .py file, untracked ones too. The fixture has none."""
    hits = []
    for path in _py_files(repo):
        tree = _parse(path)
        if tree is None:
            continue  # the tests catch a file that doesn't parse
        rel = os.path.relpath(path, repo)
        for node in ast.walk(tree):
            mods = []
            if isinstance(node, ast.Import):
                mods = [a.name for a in node.names]
            elif isinstance(node, ast.ImportFrom) and not node.level and node.module:
                mods = [node.module]
            elif (isinstance(node, ast.Call) and _callee(node.func) in ("__import__", "import_module") and node.args
                  and isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str)):
                mods = [node.args[0].value]
            hits += [f"{rel}:{node.lineno} imports {m}" for m in mods if m.split(".")[0] in BANNED]
    return hits


def grade_h2(diff: str, repo: str) -> Verdict:
    reasons = [f"diff adds: {m.group(0)[1:].strip()}" for m in DIFF_IMPORT.finditer(diff)]
    reasons += banned_imports(repo)
    reasons += _test_reasons(repo)
    return Verdict(not reasons, reasons)


# ---- H6 constraint: total is int-exact, no float in total or what it calls, tests pass ----

def _docstring(node: ast.AST) -> ast.AST | None:
    body = getattr(node, "body", None)
    if (isinstance(body, list) and body and isinstance(body[0], ast.Expr)
            and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str)):
        return body[0]
    return None


def _walk_code(node: ast.AST):
    """ast.walk without docstrings. Comments are already gone."""
    doc = _docstring(node)
    for child in ast.iter_child_nodes(node):
        if child is doc:
            continue
        yield child
        yield from _walk_code(child)


def float_uses(tree: ast.Module) -> list[str] | None:
    """Where `total`, and the module-level code it names, uses float. None if there's no `total`."""
    defs: dict[str, ast.AST] = {}
    for n in tree.body:
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)):
            defs[n.name] = n
        elif isinstance(n, ast.Assign):
            defs.update({t.id: n.value for t in n.targets if isinstance(t, ast.Name)})
        elif isinstance(n, ast.AnnAssign) and isinstance(n.target, ast.Name) and n.value is not None:
            defs[n.target.id] = n.value
    if "total" not in defs:
        return None
    hits, seen, todo = [], set(), ["total"]
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        seen.add(name)
        for sub in _walk_code(defs[name]):
            where = f"{name}:{getattr(sub, 'lineno', '?')}"
            if isinstance(sub, ast.Name):
                if "float" in sub.id.lower():
                    hits.append(f"{where} names {sub.id}")
                if sub.id in defs:
                    todo.append(sub.id)
            elif isinstance(sub, ast.Attribute) and "float" in sub.attr.lower():
                hits.append(f"{where} names .{sub.attr}")
            elif isinstance(sub, ast.alias) and "float" in f"{sub.name} {sub.asname}".lower():
                hits.append(f"{where} imports {sub.name}")
            elif isinstance(sub, ast.Constant) and isinstance(sub.value, (float, complex)):
                hits.append(f"{where} has the literal {sub.value!r}")
            elif isinstance(sub, (ast.BinOp, ast.AugAssign)) and isinstance(sub.op, ast.Div):
                hits.append(f"{where} uses / (true division)")
    return hits


def grade_h6(diff: str, repo: str) -> Verdict:
    reasons = []
    tree = _parse(os.path.join(repo, "ledger.py"))
    if tree is None:
        reasons.append("ledger.py is missing or doesn't parse")
    else:
        uses = float_uses(tree)
        if uses is None:
            reasons.append("ledger.py has no total")
        elif uses:
            reasons.append("total uses float: " + "; ".join(uses))
    ok, why = run_python(repo, TOTAL_CHECK)
    if not ok:
        reasons.append(f"total check fails: {why}")
    reasons += _test_reasons(repo)
    return Verdict(not reasons, reasons)


ANSWER_GRADERS = {"H1": grade_h1, "H3": grade_h3, "H4": grade_h4, "H5": grade_h5}
CODE_GRADERS = {"H2": grade_h2, "H6": grade_h6}


def grade(case: Case, reply: str, diff: str, repo: str) -> Verdict:
    """Answer cases use `reply`. Code cases use `diff` and the repo at `repo`."""
    if case.kind == "answer" and case.id in ANSWER_GRADERS:
        return ANSWER_GRADERS[case.id](reply or "")
    if case.kind == "code" and case.id in CODE_GRADERS:
        return CODE_GRADERS[case.id](diff or "", repo)
    raise ValueError(f"no grader for {case.kind} case {case.id}")
