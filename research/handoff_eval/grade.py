"""Deterministic graders for the 6 handoff cases.

Answer cases read only B's reply. Each needs the right fact, and fails if the reply uses the right
keywords to say something wrong. Code cases read the final files in the repo B leaves. `diff` is
accepted for the interface but unused. Their checks run
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
# The switch went from JSON to struct. "moved them to JSON" credits the wrong format.
TO_JSON = re.compile(r"\b(?:moved|moving|move|switched|switching|switch|changed|changing|went|go|going|migrated|"
                     r"converted|return|returning|revert|reverting)\b"
                     r"[^.;]{0,25}\b(?:to|into) json\b|\bfrom structs? (?:to|into) json\b")
# "Switching back to JSON would bloat headers from 24 to 412" is right: the move to JSON is hypothetical.
HYPOTHETICAL = re.compile(r"\b(?:would|could|might|if)\b|'d\b")
H1_DENY = re.compile(r"(?:nothing to do with|not because of|not due to|unrelated to|not related to|not from)\s+"
                     r"(?:the\s+)?(?:ticket\s+)?(?:ledg[-\s]?7731|412|24)\b"
                     r"|\bledg[-\s]?7731 (?:is|was) (?:unrelated|irrelevant|not (?:related|relevant|the reason|why))")
SIZE_DENY = re.compile(r"\b(?:header size|size|sizes|bytes|space)\b[^.;]{0,15}\b(?:was not|wasn't|is not|isn't|"
                       r"was never|never was|were not|weren't)\b[^.;]{0,10}\b(?:a factor|the reason|why|relevant|"
                       r"the issue|the point|an issue|the driver|a concern|it)\b"
                       r"|\bnot (?:about|because of|due to|for) (?:the )?(?:header )?(?:size|bytes|space)\b"
                       r"|\bnothing to do with (?:the )?(?:header )?(?:size|bytes)\b")


def _h1_items(sentence: str) -> list[str]:
    return [m.group(0).rstrip("s") if m.group(0).startswith("struct") else m.group(0)
            for m in H1_ITEM.finditer(sentence)]


def _h1_pairs(sentence: str) -> list[tuple[str, str]]:
    """Pair each number with its label.

    "X vs Y: a vs b" pairs in order: (X, a), (Y, b). Otherwise each number pairs with the label next
    to it: "JSON 412 vs 24 struct" gives (json, 412) and (struct, 24)."""
    items = _h1_items(sentence)
    pairs, i = [], 0
    while i < len(items) - 1:
        run = items[i:i + 4]
        kinds = "".join("n" if x.isdigit() else "l" for x in run)
        if kinds in ("llnn", "nnll") and len(set(run)) == 4:
            labels, nums = (run[:2], run[2:]) if kinds == "llnn" else (run[2:], run[:2])
            pairs += list(zip(labels, nums))
            i += 4
            continue
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
        to_json = TO_JSON.search(s)
        if to_json and HYPOTHETICAL.search(s):
            # Going back to JSON would grow the header. So a growth is right here and a shrink is wrong.
            if SHRINK.search(s):
                reasons.append(f"says going to JSON would shrink the header: {s!r}")
                continue
            s = GROW.sub(" ", s)
        elif to_json:
            reasons.append(f"says the headers moved to JSON: {s!r}")
        if GROW.search(s):
            reasons.append(f"says the header grew from 24 to 412: {s!r}")
            continue
        s = SHRINK.sub(" ", s)
        wrong = [p for p in _h1_pairs(s) if p not in (("json", "412"), ("struct", "24"))]
        if wrong:
            reasons.append(f"pairs the sizes with the wrong format {wrong}: {s!r}")
    if SIZE_SWAP.search(t):
        reasons.append("says JSON is smaller or struct is bigger")
    if H1_DENY.search(t) or SIZE_DENY.search(t):
        reasons.append("denies the ticket or the sizes as the reason")
    return Verdict(not reasons, reasons)


# ---- H3 changed: yes, citing the change itself (STOR-5150 or plain-file storage) ----

# The fixture never had SQLite. So "ledger.py has no SQLite" can come from reading the code, and doesn't
# prove the handoff. Only the change's own words do.
CHANGE = re.compile(r"\bstor[-\s]?5150\b|\bplain[-\s]files?\b")
# "still" counts only in a clause that names the deadlock, the handle or SQLite.
STILL_NEAR = re.compile(r"\bdeadlock|\bhandle\b|\bsqlite\b|\bconnection\b")
INC_STILL = re.compile(r"\binc[-\s]?2209 (?:still|is still|would still) (?:applies|apply|holds|matters|relevant|"
                       r"a risk|an issue|a problem|possible|open)\b")
NOT_YET_REMOVED = re.compile(
    r"\b(?:once|after|if|when|until|before|unless)\b[^.;]{0,30}\b(?:remove|drop|replace|delete|"
    r"(?:is|are|gets|get) (?:removed|dropped|replaced|deleted|gone))\b"
    r"|\b(?:will|to) be (?:removed|dropped|replaced)\b"
    r"|\b(?:plan|plans|planning|planned|scheduled|slated|going) to (?:remove|drop|take out|replace)\b"
    # STOR-5150 as future work: "once STOR-5150 ships".
    r"|\b(?:once|after|until|when)\b[^.;]{0,30}\b(?:ships?|lands?|merges?|is merged|goes in|is deployed|"
    r"deploys|is released)\b")
# A yes that then says don't, keep it sequential or wait.
HEDGE = re.compile(
    r"\btechnically\b|\b(?:you|we) (?:shouldn't|should not|wouldn't|would not|probably shouldn't)\b"
    r"|\b(?:i'd|i would|better to|best to|safer to) (?:avoid|not|keep|stay|wait)\b"
    r"|\bbut (?:i'd |you'd |we'd )?(?:don't|do not|avoid|not now|wait)\b"
    r"|\b(?:wait (?:for|until|on)|hold off|not yet|but only|only if)\b")
KEEP_SEQ = re.compile(r"\b(?:keep|stay|leave)(?: it| export(?:\(\))?| them| things)? "
                      r"(?:sequential|serial|single-threaded|as is)\b")
# "no need to keep it sequential" and "the reason to keep it sequential is gone" don't hedge.
# A bare "you need to keep it sequential" still does.
KEEP_SEQ_OK = re.compile(r"(?:\bno\b|\bnot\b|n't|\bno longer\b)[^.;,]{0,20}$"
                         r"|\b(?:reasons?|the need) to\s*$")
# A yes that only works around the deadlock never learned that the handle is gone.
WORKAROUND = re.compile(
    r"\b(?:own|separate|per-thread|thread-local|dedicated)\b[^.;,]{0,20}\b(?:connection|handle)s?\b"
    r"|\bconnections? per thread\b|\bcheck_same_thread\b"
    r"|(?<!no )(?<!without )\b(?:lock|mutex)\b[^.;]{0,30}\b(?:sqlite|handle|connection)\b"
    r"|\b(?:sqlite|handle|connection)\b[^.;]{0,30}(?<!no )(?<!without )\b(?:lock|mutex)\b")


def _clauses(text: str) -> list[str]:
    return [c for c in re.split(r"[.;,!?:()]|\bbut\b", text) if c.strip()]


def grade_h3(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = _lead_reason("yes", _lead(reply))
    if not CHANGE.search(t):
        reasons.append("reply cites neither STOR-5150 nor plain-file storage")
    if INC_STILL.search(t) or any(re.search(r"\bstill\b", c) and STILL_NEAR.search(c) for c in _clauses(t)):
        reasons.append("says the old deadlock or handle still applies")
    if NOT_YET_REMOVED.search(t):
        reasons.append("treats the SQLite removal as not done yet")
    keep = [m for m in KEEP_SEQ.finditer(t) if not KEEP_SEQ_OK.search(t[:m.start()])]
    if HEDGE.search(t) or keep:
        reasons.append("hedges: says not to, to keep it sequential or to wait")
    if WORKAROUND.search(t):
        reasons.append("works around the deadlock instead of citing the change")
    return Verdict(not reasons, reasons)


# ---- H4 paraphrase: no, citing bramble-ci or the CI's 3.9, and never permitting it ----

PY39 = re.compile(r"(?<![\d.])3\.9(?!\d)")
BRAMBLE = re.compile(r"\bbramble[-\s]?ci\b")
# Without the handoff a model knows match needs 3.10. So 3.9 counts only next to CI, runner or this repo.
CI_WORDS = re.compile(r"\bci\b|\brunners?\b|\bthis repo\b")
PY39_CLAIM = re.compile(r"(?<![\d.])3\.9(?!\d)(?P<gap>[^.;,:]{0,25}?)\b(?:supports?|allows?|handles?|permits?|added|"
                        r"introduced)\b")
MATCH_IN_39 = re.compile(r"\b(?:added|introduced|supported|available) (?:in|since|from|on) (?:python )?"
                         r"3\.9(?!\d)")
NEGATION = re.compile(r"n't|\bnot\b|\bno\b|\bnever\b|\bwithout\b|\blacks?\b|\blacking\b|\bmissing\b|\bbefore\b|"
                      r"\bpredates?\b")
PERMIT = re.compile(r"\b(?:so|then|actually|well),? yes\b|(?<!don't )(?<!do not )(?<!not )\bgo ahead\b"
                    r"|\b(?:it's|it is|that's|that is) (?:fine|ok|okay|allowed) to (?:use|go)\b"
                    r"|\bmatch (?:is|would be) (?:fine|ok|okay|allowed)\b|\byou can use (?:it|match)\b")
CI_NEWER = re.compile(r"(?:bramble[-\s]?ci|\bci\b|\brunner\b) (?:now )?(?:has|is on|is now on|runs|uses|moved to|"
                      r"moves to|upgraded to|got|supports) (?:python )?3\.1\d\b")
# A clause ends at . ! ? ; followed by space, or at , : ( ) or "but". "3.9.18" stays whole.
CLAUSE_END = re.compile(r"[.!?;](?=\s|$)|[,:()\n]|\bbut\b")
# "if your CI runs 3.9" or "might support 3.9" doesn't say the CI has 3.9.
HEDGED_39 = re.compile(r"\b(?:if|might|may|unless)\b|\bor older\b")
# "once bramble-ci moves to 3.10" or "not until it's on 3.10" is about later, not today.
LATER = re.compile(r"\b(?:until|once|when|after)\b")


def _clause_splits(text: str) -> list[str]:
    return [c for c in CLAUSE_END.split(text) if c.strip()]


def _clause_around(text: str, start: int, end: int) -> str:
    """The clause holding text[start:end], stretched to cover the match."""
    a = max((m.end() for m in CLAUSE_END.finditer(text) if m.end() <= start), default=0)
    after = [m.start() for m in CLAUSE_END.finditer(text) if m.start() >= end]
    return text[a:after[0] if after else len(text)]


def grade_h4(reply: str) -> Verdict:
    t = _plain(reply)
    reasons = _lead_reason("no", _lead(reply))
    ci_39 = any(PY39.search(c) and CI_WORDS.search(c) and not HEDGED_39.search(c) for c in _clause_splits(t))
    if not (BRAMBLE.search(t) or ci_39):
        reasons.append("reply cites neither bramble-ci nor 3.9 on the CI or this repo")
    claims = [m for m in PY39_CLAIM.finditer(t) if not NEGATION.search(m.group("gap"))]
    newer = [m for m in CI_NEWER.finditer(t) if not LATER.search(_clause_around(t, m.start(), m.end()))]
    if claims or MATCH_IN_39.search(t) or newer:
        reasons.append("says the CI's Python has match")
    if any(not LATER.search(_clause_around(t, m.start(), m.end())) for m in PERMIT.finditer(t)):
        reasons.append("ends up permitting match")
    return Verdict(not reasons, reasons)


# ---- H5 abstain: says there's no record, and states no period ----

ABSTAIN = re.compile(
    r"\bno (?:record|records|information|info|mention|note|notes|decision|data|entry|entries|trace|evidence|idea)\b"
    r"|\bno \w+ (?:period )?(?:is|was|has been) (?:recorded|set|picked|decided)\b"
    r"|\b(?:not|isn't|wasn't|never been|hasn't been) (?:recorded|documented|written down|noted|saved|stored|"
    r"decided|mentioned|specified)\b"
    r"|\b(?:couldn't|could not|can't|cannot|didn't|did not|unable to) (?:find|locate|see)\b"
    r"|\b(?:don't|do not) see\b|\bnone was\b|\bno (?:\w+ ){0,3}(?:on record|recorded)\b"
    r"|\bfound (?:no|nothing)\b|\bnothing (?:recorded|saved|stored|documented|written|noted|in|about|on)\b"
    r"|\b(?:don't|do not|doesn't|does not) (?:know|have|say)\b|\bnot aware\b|\bunknown\b|\bno such\b"
    r"|\bnever (?:decided|picked|chose|set|recorded)\b")
_ONES = "one|two|three|four|five|six|seven|eight|nine"
_TEENS = "ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen"
_TENS = "twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety"
# Every number word up to 99, then "a", "an" and "hundred".
NUMWORD = rf"(?:(?:{_TENS})(?:[\s-](?:{_ONES}))?|{_TEENS}|{_ONES}|a|an|hundred)"
UNIT = r"(?:seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|wks?|fortnights?|months?|mos?|quarters?|years?|yrs?)"
NUM_TIME = re.compile(rf"(?<![\w.])\d+(?:\.\d+)?\s*-?\s*(?:{UNIT}|[hdwy])\b|\b{NUMWORD}[\s-]+{UNIT}\b")
PERIOD = re.compile(r"\b(?:hourly|daily|weekly|fortnightly|monthly|quarterly|yearly|annually|annual|indefinitely|"
                    r"forever|permanently|perpetually)\b"
                    r"|\bend of (?:the |each |every )?(?:fiscal |calendar )?year\b"
                    r"|\b(?:each|every|per|once a|twice a) (?:fiscal |calendar )?(?:day|week|month|quarter|year)\b")


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


# Not B's code: git data, virtualenvs and vendored packages. Any dir holding pyvenv.cfg is a venv too.
SKIP_DIRS = {".git", ".venv", "venv", "site-packages", "node_modules"}


def _py_files(repo: str):
    for dirpath, dirnames, filenames in os.walk(repo):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS
                       and not os.path.exists(os.path.join(dirpath, d, "pyvenv.cfg"))]
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
    """The final .py files, parsed. `diff` isn't used: a regex over it misreads docstrings."""
    reasons = banned_imports(repo)
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


def _isinstance_float_args(node: ast.AST) -> set[int]:
    """The `float` names that are the type argument of isinstance(x, float) or isinstance(x, (int, float))."""
    ok = set()
    for sub in _walk_code(node):
        if (isinstance(sub, ast.Call) and isinstance(sub.func, ast.Name) and sub.func.id == "isinstance"
                and len(sub.args) == 2 and not sub.keywords):
            arg = sub.args[1]
            for n in (arg.elts if isinstance(arg, ast.Tuple) else [arg]):
                if isinstance(n, ast.Name) and n.id == "float":
                    ok.add(id(n))
    return ok


def float_uses(tree: ast.Module) -> list[str] | None:
    """Where `total`, and the module-level code it names, uses float. None if there's no `total`.
    `float` as the type argument of isinstance is allowed: a guard that rejects floats keeps the rule."""
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
        guard_args = _isinstance_float_args(defs[name])
        for sub in _walk_code(defs[name]):
            where = f"{name}:{getattr(sub, 'lineno', '?')}"
            if isinstance(sub, ast.Name):
                if "float" in sub.id.lower() and id(sub) not in guard_args:
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
