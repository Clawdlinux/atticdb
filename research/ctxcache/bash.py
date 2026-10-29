"""Which files a shell command reads, writes or names.

Only common forms are recognized. The rules are in README.md. A command that is
not a pure read of 1 file is plain output, and the known files it names are uses.
"""

from __future__ import annotations

import posixpath
import re
from dataclasses import dataclass, field

READERS = {"cat", "head", "tail", "nl", "wc", "less", "more", "sed", "grep", "egrep", "fgrep", "rg", "awk"}
FILTERS = {"head", "tail", "grep", "egrep", "fgrep", "rg", "sed", "awk", "cut", "sort", "uniq", "wc", "nl", "tr", "cat"}
GREPS = {"grep", "egrep", "fgrep"}
# Programs whose file operands are scripts or tests to run.
RUNNERS = {"python", "python2", "python3", "pytest", "py.test", "nosetests", "coverage", "ipython", "bash", "sh",
           "zsh", "node", "ruby", "perl", "php", "Rscript", "julia", "tox"}
WRAPPERS = {"timeout", "time", "sudo", "env", "nohup", "nice", "stdbuf", "command", "exec"}
REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&", "<", "<<", "<<-", "<<<", "<&", "<>"}
WRITE_REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&"}
OPERATORS = "();<>|&\n"
# Longest first, so "&&" wins over "&" and "2>&1" reads as 2, >&, 1.
OPS = sorted(["&&", "||", ";;", "|&", "&>>", "&>", ">>", ">&", ">|", "<<<", "<<-", "<<", "<&", "<>", ">", "<", "|", "&",
              ";", "(", ")", "\n"], key=len, reverse=True)
# Options that take the next word as their value, per program.
VALUED = {
    "grep": {"-e", "-f", "-m", "-A", "-B", "-C", "-d", "-D", "--regexp", "--file", "--max-count"},
    "rg": {"-e", "-f", "-g", "-t", "-T", "-m", "-A", "-B", "-C", "-M", "-j", "--type", "--glob", "--regexp"},
    "sed": {"-e", "-f", "-l", "--expression", "--file"},
    "awk": {"-f", "-F", "-v"},
    "head": {"-n", "-c", "--lines", "--bytes"},
    "tail": {"-n", "-c", "--lines", "--bytes"},
    "python": {"-m", "-c", "-W", "-X"},
    "git": {"-C", "-c"},
}
# The first operand of these is a pattern or a script, unless an option gives it.
PATTERN_FIRST = {
    "grep": {"-e", "-f", "--regexp", "--file"},
    "rg": {"-e", "-f", "--regexp"},
    "sed": {"-e", "-f", "--expression", "--file"},
    "awk": {"-f"},
}
for _p in ("egrep", "fgrep"):
    VALUED[_p], PATTERN_FIRST[_p] = VALUED["grep"], PATTERN_FIRST["grep"]
VALUED["python3"] = VALUED["python"]
DELIMITER = re.compile(r"^[A-Za-z_]\w*$")  # a heredoc delimiter, after its quotes go
PATHISH = re.compile(r"^[\w.~/+@%,:=-]+$")
EXTENSION = re.compile(r"\.[A-Za-z0-9]{1,8}$")
URL = re.compile(r"^https?://\S+$")


@dataclass
class Effects:
    events: list[tuple[str, str]] = field(default_factory=list)  # ("use" or "write", path), in order
    read: str | None = None  # the 1 file a pure read shows
    url: str | None = None  # the 1 URL a pure fetch shows
    urls: list[str] = field(default_factory=list)  # other URLs named
    cwd: str | None = None  # the working directory after the command
    why: dict[str, str] = field(default_factory=dict)  # used path -> "run", "git", "edit" or "path"


@dataclass
class Stage:
    sep: str  # the operator before it
    prog: str
    args: list[str]
    redirects: list[tuple[str, str]]
    cwd: str | None
    word: str = ""  # the program as written, such as ./run.sh


def effects(command: str, cwd: str | None, known: set[str]) -> Effects:
    eff = Effects(cwd=cwd)
    parts = split(command)
    if parts is None:
        return eff
    stages: list[Stage] = []
    for sep, words in parts:
        words = _unwrap(words)
        if not words:
            continue
        prog = posixpath.basename(words[0])
        args, redirects = _redirects(words[1:])
        if prog == "cd":
            eff.cwd = _cd(eff.cwd, args)
            if not stages:
                continue  # leading cd commands do not stop a pure read
        stages.append(Stage(sep, prog, args, redirects, eff.cwd, words[0]))
        _classify(stages[-1], eff, known)

    pure = _pure_read(stages, known)
    if pure is not None and not any(kind == "write" for kind, _ in eff.events):
        eff.read = pure
        eff.events = []
    elif len(stages) == 1 and stages[0].prog in ("curl", "wget") and len(eff.urls) == 1 and not eff.events:
        if not any(a in ("-o", "-O") or a.startswith("--output") for a in stages[0].args):
            eff.url, eff.urls = eff.urls[0], []
    eff.events = list(dict.fromkeys(eff.events))
    return eff


class Op(str):
    """An unquoted operator. A quoted ">" or "|" stays a plain word."""


def split(command: str) -> list[tuple[str, list[str]]] | None:
    """Simple commands as (operator before it, words). None if a quote is not closed."""
    toks = tokens(command)
    if toks is None:
        return None
    out, words, sep = [], [], ""
    for kind, text in toks:
        if kind == "op" and text not in REDIRECTS:
            if words:
                out.append((sep, words))
            words, sep = [], text
        else:
            words.append(Op(text) if kind == "op" else text)
    if words:
        out.append((sep, words))
    return out


def tokens(command: str) -> list[tuple[str, str]] | None:
    """("op", text) and ("word", text) pairs, in 1 pass over the raw text.
    Quotes and escapes resolve inside words, so an operator in quotes is part of its word.
    A backslash-newline joins lines outside quotes and inside double quotes. An escaped
    backslash before a newline joins nothing. A word that starts with # is a comment up
    to the newline, which stays. Only an unquoted << or <<- starts a heredoc, and only
    with a delimiter such as EOF or 'EOF', so a shift such as $((1 << 16)) does not.
    Its body is skipped from the next newline through the delimiter line. None if a
    quote is not closed."""
    s, out, heredocs = command, [], []
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c in " \t\r":
            i += 1
        elif c == "\\" and s.startswith("\n", i + 1):
            i += 2  # a line continuation between words
        elif c in OPERATORS:
            op = next(o for o in OPS if s.startswith(o, i))
            out.append(("op", op))
            i += len(op)
            if op == "\n" and heredocs:
                i = _skip_heredocs(s, i, heredocs)
                heredocs = []
        elif c == "#":
            j = s.find("\n", i)
            i = n if j < 0 else j
        else:
            word, i = _word(s, i)
            if word is None:
                return None
            if out and out[-1][0] == "op" and out[-1][1] in ("<<", "<<-") and DELIMITER.match(word):
                heredocs.append((word, out[-1][1] == "<<-"))
            out.append(("word", word))
    return out


def _word(s: str, i: int) -> tuple[str | None, int]:
    """1 word from position i, with quotes and escapes resolved. None if a quote is open."""
    word: list[str] = []
    n = len(s)
    while i < n and s[i] not in " \t\r" and s[i] not in OPERATORS:
        c = s[i]
        if c == "'":
            j = s.find("'", i + 1)
            if j < 0:
                return None, n
            word.append(s[i + 1 : j])
            i = j + 1
        elif c == '"':
            i += 1
            while i < n and s[i] != '"':
                if s[i] == "\\" and i + 1 < n and s[i + 1] == "\n":
                    i += 2  # a line continuation inside double quotes
                    continue
                if s[i] == "\\" and i + 1 < n and s[i + 1] in '"\\$`':
                    i += 1
                word.append(s[i])
                i += 1
            if i >= n:
                return None, n
            i += 1
        elif c == "\\" and i + 1 < n:
            if s[i + 1] != "\n":  # a backslash-newline joins the lines
                word.append(s[i + 1])
            i += 2
        else:
            word.append(c)
            i += 1
    return "".join(word), i


def _skip_heredocs(s: str, i: int, heredocs: list[tuple[str, bool]]) -> int:
    """Skips each pending heredoc body, in order, from position i: the start of the line
    after the << operator. Body lines are raw text: no quotes, escapes or comments."""
    for delimiter, tabs in heredocs:
        while i < len(s):
            j = s.find("\n", i)
            line = s[i:] if j < 0 else s[i:j]
            i = len(s) if j < 0 else j + 1
            if (line.lstrip("\t") if tabs else line) == delimiter:
                break
    return i


def pathlike(word: str) -> bool:
    if not word or word[0] in "-+" or "://" in word or word in (".", "..", "/") or not PATHISH.match(word):
        return False
    return "/" in word or EXTENSION.search(word) is not None


def resolve(word: str, cwd: str | None, known: set[str]) -> str:
    """An absolute path when the directory is known. With no directory, a relative
    word matches the known path that ends with it, if exactly 1 does."""
    if word.startswith("/") or cwd:
        return posixpath.normpath(posixpath.join(cwd or "/", word))
    word = posixpath.normpath(word)
    hits = [k for k in known if k.endswith("/" + word)]
    return hits[0] if len(hits) == 1 else word


def _cd(cwd: str | None, args: list[str]) -> str | None:
    target = next((a for a in args if not a.startswith("-")), None)
    if target is None or target.startswith("~") or "$" in target:
        return None
    if target.startswith("/"):
        return posixpath.normpath(target)
    return posixpath.normpath(posixpath.join(cwd, target)) if cwd else None


def _unwrap(words: list[str]) -> list[str]:
    """Drops leading VAR=value words and wrappers such as `timeout 10`."""
    i = 0
    while i < len(words):
        if re.match(r"^[A-Za-z_]\w*=", words[i]):
            i += 1
        elif words[i] in WRAPPERS:
            i += 1
            while i < len(words) and (words[i].startswith("-") or re.match(r"^\d+(\.\d+)?[smhd]?$", words[i])):
                i += 1
        else:
            break
    return words[i:]


def _redirects(words: list[str]) -> tuple[list[str], list[tuple[str, str]]]:
    args, redirects, i = [], [], 0
    while i < len(words):
        w = words[i]
        if not isinstance(w, Op):
            args.append(w)
            i += 1
            continue
        target = words[i + 1] if i + 1 < len(words) else ""
        if args and args[-1].isdigit():
            args.pop()  # the fd number in 2>file
        if target and not (w == ">&" and target.isdigit()):
            redirects.append((w, target))
        i += 2
    return args, redirects


def _files(prog: str, args: list[str]) -> tuple[list[str], set[str]]:
    """Path-like operands, and the options seen."""
    valued, ops, flags, i = VALUED.get(prog, set()), [], set(), 0
    while i < len(args):
        a = args[i]
        if a == "--":
            ops += args[i + 1 :]
            break
        if a.startswith("-") and len(a) > 1:
            flags.add(a)
            i += 2 if a in valued else 1
            continue
        ops.append(a)
        i += 1
    if prog in PATTERN_FIRST and ops and not flags & PATTERN_FIRST[prog]:
        ops = ops[1:]
    return [o for o in ops if pathlike(o)], flags


def _in_place(prog: str, flags: set[str]) -> bool:
    return prog == "sed" and any(f.startswith(("-i", "--in-place")) for f in flags)


def _classify(stage: Stage, eff: Effects, known: set[str]) -> None:
    files, flags = _files(stage.prog, stage.args)
    paths = [resolve(f, stage.cwd, known) for f in files]
    uses, writes = paths, []
    why = "run" if stage.prog in RUNNERS else ("git" if stage.prog in ("git", "diff") else "path")
    if pathlike(stage.word) and resolve(stage.word, stage.cwd, known) in known:
        uses, why = [resolve(stage.word, stage.cwd, known), *paths], "run"  # ./run.sh runs the file
    if stage.prog in ("rm", "unlink", "mv", "touch", "truncate", "tee"):
        uses, writes = [], paths
    elif stage.prog in ("cp", "install", "ln"):
        uses, writes = paths[:-1], paths[-1:]
    elif _in_place(stage.prog, flags):
        writes, why = paths, "edit"
    elif stage.prog == "git" and stage.args[:1] in (["checkout"], ["restore"]):
        uses, writes = [], [p for p in paths if p in known]
    for op, target in stage.redirects:
        if pathlike(target) and not target.startswith("/dev/"):
            (writes if op in WRITE_REDIRECTS else uses).append(resolve(target, stage.cwd, known))
    for p in uses:
        if p in known:
            eff.why.setdefault(p, why)
    eff.events += [("use", p) for p in uses if p in known]
    eff.events += [("write", p) for p in writes]
    eff.urls += [a for a in stage.args if URL.match(a)]


def _pure_read(stages: list[Stage], known: set[str]) -> str | None:
    """The file a command shows when it is 1 reader on 1 file, maybe piped into filters.
    rg searches directories, so it reads a file only when the operand is a known file
    or has an extension. A trailing slash is never a file."""
    if not stages or any(s.redirects for s in stages) or any(s.sep != "|" for s in stages[1:]):
        return None
    first = stages[0]
    if first.prog not in READERS:
        return None
    files, flags = _files(first.prog, first.args)
    if len(files) != 1 or _in_place(first.prog, flags) or files[0].endswith("/"):
        return None
    if first.prog in GREPS and ("--recursive" in flags or any("r" in f or "R" in f for f in flags if f[1] != "-")):
        return None
    path = resolve(files[0], first.cwd, known)
    if first.prog == "rg" and path not in known and not EXTENSION.search(posixpath.basename(path)):
        return None
    if any(s.prog not in FILTERS or _files(s.prog, s.args)[0] for s in stages[1:]):
        return None
    return path
