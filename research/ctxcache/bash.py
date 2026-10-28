"""Which files a shell command reads, writes or names.

Only common forms are recognized. The rules are in README.md. A command that is
not a pure read of 1 file is plain output, and the known files it names are uses.
"""

from __future__ import annotations

import posixpath
import re
import shlex
from dataclasses import dataclass, field

READERS = {"cat", "head", "tail", "nl", "wc", "less", "more", "sed", "grep", "egrep", "fgrep", "rg", "awk"}
FILTERS = {"head", "tail", "grep", "egrep", "fgrep", "rg", "sed", "awk", "cut", "sort", "uniq", "wc", "nl", "tr", "cat"}
GREPS = {"grep", "egrep", "fgrep"}
WRAPPERS = {"timeout", "time", "sudo", "env", "nohup", "nice", "stdbuf", "command", "exec"}
REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&", "<", "<<", "<<<", "<&", "<>"}
WRITE_REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&"}
OPERATORS = "();<>|&\n"
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
HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_]\w*)\1")
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


@dataclass
class Stage:
    sep: str  # the operator before it
    prog: str
    args: list[str]
    redirects: list[tuple[str, str]]
    cwd: str | None


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
        stages.append(Stage(sep, prog, args, redirects, eff.cwd))
        _classify(stages[-1], eff, known)

    pure = _pure_read(stages)
    if pure is not None and not any(kind == "write" for kind, _ in eff.events):
        eff.read = resolve(pure, stages[0].cwd, known)
        eff.events = []
    elif len(stages) == 1 and stages[0].prog in ("curl", "wget") and len(eff.urls) == 1 and not eff.events:
        if not any(a in ("-o", "-O") or a.startswith("--output") for a in stages[0].args):
            eff.url, eff.urls = eff.urls[0], []
    eff.events = list(dict.fromkeys(eff.events))
    return eff


def split(command: str) -> list[tuple[str, list[str]]] | None:
    """Simple commands as (operator before it, words). None if the quotes do not parse."""
    lex = shlex.shlex(strip_heredocs(command), posix=True, punctuation_chars=OPERATORS)
    lex.whitespace, lex.whitespace_split = " \t\r", True
    try:
        tokens = list(lex)
    except ValueError:
        return None
    out, words, sep = [], [], ""
    for tok in tokens:
        if tok and all(c in OPERATORS for c in tok) and tok not in REDIRECTS:
            if words:
                out.append((sep, words))
            words, sep = [], tok.strip("\n") or "\n"
        else:
            words.append(tok)
    if words:
        out.append((sep, words))
    return out


def strip_heredocs(command: str) -> str:
    lines, out, i = command.split("\n"), [], 0
    while i < len(lines):
        out.append(lines[i])
        i += 1
        for m in HEREDOC.finditer(out[-1]):
            while i < len(lines) and lines[i].strip() != m.group(2):
                i += 1
            i += 1
    return "\n".join(out)


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
        if w not in REDIRECTS:
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
    if stage.prog in ("rm", "unlink", "mv", "touch", "truncate", "tee"):
        uses, writes = [], paths
    elif stage.prog in ("cp", "install", "ln"):
        uses, writes = paths[:-1], paths[-1:]
    elif _in_place(stage.prog, flags):
        writes = paths
    elif stage.prog == "git" and stage.args[:1] in (["checkout"], ["restore"]):
        uses, writes = [], [p for p in paths if p in known]
    for op, target in stage.redirects:
        if pathlike(target) and not target.startswith("/dev/"):
            (writes if op in WRITE_REDIRECTS else uses).append(resolve(target, stage.cwd, known))
    eff.events += [("use", p) for p in uses if p in known]
    eff.events += [("write", p) for p in writes]
    eff.urls += [a for a in stage.args if URL.match(a)]


def _pure_read(stages: list[Stage]) -> str | None:
    """The file a command shows when it is 1 reader on 1 file, maybe piped into filters."""
    if not stages or any(s.redirects for s in stages) or any(s.sep != "|" for s in stages[1:]):
        return None
    first = stages[0]
    if first.prog not in READERS:
        return None
    files, flags = _files(first.prog, first.args)
    if len(files) != 1 or _in_place(first.prog, flags):
        return None
    if first.prog in GREPS and ("--recursive" in flags or any("r" in f or "R" in f for f in flags if f[1] != "-")):
        return None
    if any(s.prog not in FILTERS or _files(s.prog, s.args)[0] for s in stages[1:]):
        return None
    return files[0]
