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
SEARCHES = GREPS | {"rg"}  # these exit 1 when nothing matches
RECURSIVE = {"-r", "-R", "--recursive", "--dereference-recursive"}
# Programs whose file operands are scripts or tests to run.
RUNNERS = {"python", "python2", "python3", "pytest", "py.test", "nosetests", "coverage", "ipython", "bash", "sh",
           "zsh", "node", "ruby", "perl", "php", "Rscript", "julia", "tox"}
# Every operand of these is a file they change.
WRITERS = {"rm", "unlink", "mv", "touch", "truncate", "tee"}
# These read or link their sources and write their target.
COPIERS = {"cp", "install", "ln"}
# A wrapper runs the command after its own options. Per wrapper: the options that take
# a value, and the operands before the command, such as timeout's duration.
WRAPPERS = {
    "timeout": ({"-s", "-k", "--signal", "--kill-after"}, 1),
    "time": ({"-f", "-o", "--format", "--output"}, 0),
    "sudo": ({"-C", "-D", "-g", "-p", "-r", "-t", "-T", "-u", "-U", "--close-from", "--chdir", "--group", "--prompt",
              "--role", "--type", "--command-timeout", "--user", "--other-user"}, 0),
    "env": ({"-u", "-C", "-S", "--unset", "--chdir", "--split-string"}, 0),
    "nice": ({"-n", "--adjustment"}, 0),
    "stdbuf": ({"-i", "-o", "-e", "--input", "--output", "--error"}, 0),
    "nohup": (set(), 0),
    "command": (set(), 0),
    "exec": ({"-a"}, 0),
}
REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&", "<", "<<", "<<-", "<<<", "<&", "<>"}
WRITE_REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&"}
READ_REDIRECTS = {"<", "<>"}  # the others take a heredoc delimiter, a string or a descriptor
OPERATORS = "();<>|&\n"
# Longest first, so "&&" wins over "&" and "2>&1" reads as 2, >&, 1.
OPS = sorted(["&&", "||", ";;", "|&", "&>>", "&>", ">>", ">&", ">|", "<<<", "<<-", "<<", "<&", "<>", ">", "<", "|", "&",
              ";", "(", ")", "\n"], key=len, reverse=True)
# Options that take a value, per program. A short one takes the rest of its word, or
# else the next word, so -nA 3 is -n, then -A with 3. A long one takes =value or the next word.
VALUED = {
    "grep": {"-e", "-f", "-m", "-A", "-B", "-C", "-d", "-D", "--regexp", "--file", "--max-count", "--after-context",
             "--before-context", "--context", "--include", "--exclude", "--exclude-dir"},
    "rg": {"-e", "-f", "-g", "-t", "-T", "-m", "-A", "-B", "-C", "-M", "-j", "-r", "-E", "-d", "--type", "--type-not",
           "--glob", "--iglob", "--regexp", "--replace", "--max-count", "--max-depth", "--context", "--after-context",
           "--before-context"},
    "sed": {"-e", "-f", "-l", "--expression", "--file", "--line-length"},
    "awk": {"-f", "-F", "-v"},
    "head": {"-n", "-c", "--lines", "--bytes"},
    "tail": {"-n", "-c", "-s", "--lines", "--bytes", "--sleep-interval", "--pid"},
    "nl": {"-b", "-d", "-f", "-h", "-i", "-l", "-n", "-s", "-v", "-w"},
    "python": {"-m", "-c", "-W", "-X"},
    "git": {"-C", "-c"},
    "cp": {"-t", "-S", "--target-directory", "--suffix"},
    "install": {"-t", "-S", "-m", "-o", "-g", "--target-directory", "--suffix", "--mode", "--owner", "--group"},
    "touch": {"-d", "-r", "-t", "--date", "--reference"},
    "truncate": {"-r", "-s", "--reference", "--size"},
}
# Options whose value can only be attached, such as sed -i.bak.
ATTACHED = {"sed": {"-i"}}
# Programs that take +N or +cmd options.
PLUS_OPTIONS = {"tail", "less", "more"}
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
VALUED["mv"] = VALUED["ln"] = VALUED["cp"]
ASSIGNMENT = re.compile(r"^[A-Za-z_]\w*=")
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
    progs: list[str] = field(default_factory=list)  # each simple command's program, leading cd aside
    undo: list[str] = field(default_factory=list)  # the writes a failure of the whole command rules out

    def succeeded(self, code: int | None) -> bool:
        """Whether an exit code means success. An unknown code counts as success. grep and
        rg exit 1 when nothing matches, so 1 is success when the pipeline ends in them."""
        return code is None or code == 0 or (code == 1 and bool(self.progs) and self.progs[-1] in SEARCHES)

    def applied(self, failed: bool | None) -> list[tuple[str, str]]:
        """The events that stand, given the observed outcome. None means unknown. A failed
        command loses only the writes in `undo`. Its uses stay, since the action named them."""
        if not failed:
            return self.events
        return [e for e in self.events if not (e[0] == "write" and e[1] in self.undo)]

    def error_line(self, output: str) -> bool:
        """True if a simple command's output opens with its program's own error, as in
        "sed: -e expression #1, char 9: unterminated `s' command"."""
        return len(self.progs) == 1 and output.lstrip().startswith(self.progs[0] + ": ")


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
    undo: list[str] = []
    for sep, words in parts:
        # Redirects first: they can come anywhere, even with no command, as in `> Makefile`.
        args, redirects = _redirects(words)
        args = _unwrap(args)
        if not args and not redirects:
            continue
        word = args[0] if args else ""
        prog = posixpath.basename(word)
        if prog == "cd":
            eff.cwd = _cd(eff.cwd, args[1:])
            if not stages:
                continue  # leading cd commands do not stop a pure read
        stages.append(Stage(sep, prog, args[1:], redirects, eff.cwd, word))
        undo = _classify(stages[-1], eff, known)
    eff.progs = [s.prog for s in stages]
    # Only a whole command's outcome is seen, so only a simple command's failure rules out its writes.
    eff.undo = undo if len(stages) == 1 else []

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


def literal(word: str) -> bool:
    """A word that names 1 file as written: no glob, variable, option or URL."""
    if not word or word[0] in "-+" or "://" in word or word in (".", "..", "/", "~"):
        return False
    return PATHISH.match(word) is not None


def pathlike(word: str) -> bool:
    """A literal word with a slash or an extension. Only where operand roles are unknown,
    so `make test` names no file."""
    return literal(word) and ("/" in word or EXTENSION.search(word) is not None)


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
    """Drops leading VAR=value words, and wrappers with their options and operands,
    as in `timeout -s KILL 30` or `sudo -u dev env -u X`."""
    i = 0
    while i < len(words):
        if ASSIGNMENT.match(words[i]):
            i += 1
            continue
        if words[i] not in WRAPPERS:
            break
        valued, operands = WRAPPERS[words[i]]
        i += 1
        while i < len(words) and words[i].startswith("-"):
            if words[i] == "--":
                i += 1
                break
            i += 2 if _option(words[i], valued)[2] else 1
        i += operands
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


def _option(word: str, valued: set[str], attached: set[str] = frozenset()) -> tuple[list[str], str | None, bool]:
    """The option names in 1 word, the value it carries, and whether its value is the
    next word. -nA is -n then -A. A short option that takes a value takes the rest of
    its word, so -sKILL carries KILL. An attached-only option, such as sed -i, never
    takes the next word."""
    if word.startswith("--"):
        name, eq, value = word.partition("=")
        return [name], (value if eq else None), name in valued and not eq
    names = []
    for k in range(1, len(word)):
        name = "-" + word[k]
        names.append(name)
        if name in valued or name in attached:
            rest = word[k + 1 :]
            return names, (rest or None), name in valued and not rest
    return names, None, False


def _operands(prog: str, args: list[str]) -> tuple[list[str], dict[str, str | None]]:
    """The operands in order, and each option seen with its value. Options can come
    anywhere before a --. For tail, less and more, a word that starts with + is an
    option, as in tail +5. Elsewhere it is an operand, such as grep's pattern +00:59."""
    valued, attached = VALUED.get(prog, set()), ATTACHED.get(prog, set())
    ops: list[str] = []
    opts: dict[str, str | None] = {}
    i = 0
    while i < len(args):
        a = args[i]
        i += 1
        if a == "--":
            ops += args[i:]
            break
        if len(a) > 1 and a[0] == "+" and prog in PLUS_OPTIONS:
            opts[a] = None
        elif len(a) > 1 and a[0] == "-":
            names, value, next_word = _option(a, valued, attached)
            if next_word and i < len(args):
                value, i = args[i], i + 1
            opts.update(dict.fromkeys(names))
            opts[names[-1]] = value
        else:
            ops.append(a)
    return ops, opts


def _reader_files(prog: str, ops: list[str], opts: dict) -> list[str]:
    """A reader's file operands: all but the pattern or script, unless an option gives it."""
    if prog in PATTERN_FIRST and ops and not opts.keys() & PATTERN_FIRST[prog]:
        return ops[1:]
    return ops


def _in_place(prog: str, opts: dict) -> bool:
    return prog == "sed" and ("-i" in opts or "--in-place" in opts)


def _copy(ops: list[str], opts: dict) -> tuple[list[str], list[str]]:
    """The sources of cp, mv, install or ln, and the paths written. Positions come before
    any filtering, so a target such as $DEST never turns a source into a write. With -t,
    more than 1 source, or a target ending in /, each source lands inside the target.
    A source whose name is unknown, such as *.py, writes the target directory itself."""
    target = opts.get("-t") or opts.get("--target-directory")
    if target is None:
        if len(ops) < 2:
            return ops, []
        *ops, target = ops
        if len(ops) == 1 and not target.endswith("/"):
            return ops, [target]
    names = [posixpath.basename(s.rstrip("/")) for s in ops]
    return ops, list(dict.fromkeys(posixpath.join(target, n) if literal(n) else target for n in names))


def _roles(prog: str, ops: list[str], opts: dict) -> tuple[list[str], list[str], int] | None:
    """The operand words a command uses and writes, by role, and how many separate changes
    it makes: 1 per source of a copy or move, else 1 per file changed. None where roles
    are unknown."""
    if prog == "mv":
        sources, targets = _copy(ops, opts)
        return [], sources + targets, len(sources)
    if prog in WRITERS:
        return [], ops, len(ops)
    if prog in COPIERS:
        sources, targets = _copy(ops, opts)
        return sources, targets, len(sources)
    if prog in READERS:
        files = _reader_files(prog, ops, opts)
        return (files, files, len(files)) if _in_place(prog, opts) else (files, [], 0)
    return None


def _classify(stage: Stage, eff: Effects, known: set[str]) -> list[str]:
    """Adds the stage's events to `eff`. Returns the writes its failure would rule out:
    those of a command that makes 1 change, such as sed -i on 1 file or cp of 1 source.
    A failure after part of a bigger change, as in rm a b, leaves its writes standing."""
    ops, opts = _operands(stage.prog, stage.args)
    where = lambda words: [resolve(w, stage.cwd, known) for w in words if literal(w)]  # noqa: E731
    roles = _roles(stage.prog, ops, opts)
    changes = 0
    if roles is not None:  # the operands' roles are known, so any literal word is a file
        uses, writes, changes = where(roles[0]), where(roles[1]), roles[2]
    elif stage.prog == "git" and ops[:1] in (["checkout"], ["restore"]):
        uses, writes, changes = [], [p for p in where(ops[1:]) if p in known], len(ops) - 1
    else:  # roles unknown: only a word with a slash or an extension may be a path
        uses, writes = [resolve(w, stage.cwd, known) for w in ops if pathlike(w)], []
    undo = list(writes) if changes == 1 else []
    why = "run" if stage.prog in RUNNERS else ("git" if stage.prog in ("git", "diff") else "path")
    if _in_place(stage.prog, opts):
        why = "edit"
    if pathlike(stage.word) and resolve(stage.word, stage.cwd, known) in known:
        uses, why = [resolve(stage.word, stage.cwd, known), *uses], "run"  # ./run.sh runs the file
    redirected = set()  # the shell opens these before the command runs, so their writes stand
    for op, target in stage.redirects:
        if op in WRITE_REDIRECTS | READ_REDIRECTS and literal(target) and not target.startswith("/dev/"):
            path = resolve(target, stage.cwd, known)
            if op in WRITE_REDIRECTS:
                writes.append(path)
                redirected.add(path)
            else:
                uses.append(path)
    for p in uses:
        if p in known:
            eff.why.setdefault(p, why)
    eff.events += [("use", p) for p in uses if p in known]
    eff.events += [("write", p) for p in writes]
    eff.urls += [a for a in stage.args if URL.match(a)]
    return [p for p in undo if p not in redirected]


def _pure_read(stages: list[Stage], known: set[str]) -> str | None:
    """The file a command shows when it is 1 reader on 1 file, maybe piped into filters.
    The 1 operand must be a literal word. rg searches directories, so it reads a file
    only when the operand is a known file or has an extension. A trailing slash is never
    a file."""
    if not stages or any(s.redirects for s in stages) or any(s.sep != "|" for s in stages[1:]):
        return None
    first = stages[0]
    if first.prog not in READERS:
        return None
    ops, opts = _operands(first.prog, first.args)
    files = _reader_files(first.prog, ops, opts)
    if len(files) != 1 or not literal(files[0]) or files[0].endswith("/") or _in_place(first.prog, opts):
        return None
    if first.prog in GREPS and opts.keys() & RECURSIVE:
        return None
    path = resolve(files[0], first.cwd, known)
    if first.prog == "rg" and path not in known and not EXTENSION.search(posixpath.basename(path)):
        return None
    if any(s.prog not in FILTERS or _reads_own_file(s) for s in stages[1:]):
        return None
    return path


def _reads_own_file(stage: Stage) -> bool:
    """True if a filter after a pipe names a file to read, not only the pipe."""
    ops, opts = _operands(stage.prog, stage.args)
    if stage.prog in READERS:
        return any(f != "-" for f in _reader_files(stage.prog, ops, opts))
    return any(pathlike(o) for o in ops)
