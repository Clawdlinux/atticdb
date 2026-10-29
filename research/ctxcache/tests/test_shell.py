import pytest

from research.ctxcache.bash import effects

REPO = "/w/repo"
M, A, R, X = "/w/repo/src/m.py", "/w/repo/a.py", "/w/repo/reproduce.py", "/w/repo/x.py"
MK, DF = "/w/repo/Makefile", "/w/repo/Dockerfile"
KNOWN = {M, A, R, MK, DF}
TWO_M = KNOWN | {"/w/repo/lib/m.py"}

# name, command, cwd, known files, expected (events, pure read, URL read). A use shows its
# reference subtype: "run", "git", "edit" or "path".
CASES = [
    ("a redirect writes its target", "python a.py > out.txt", REPO, KNOWN,
     ([("run", A), ("write", "/w/repo/out.txt")], None, None)),
    ("an fd redirect is not a file", "python a.py 2>&1 | tail -5", REPO, KNOWN, ([("run", A)], None, None)),
    ("an append is a write", "echo done >> notes.md", REPO, KNOWN, ([("write", "/w/repo/notes.md")], None, None)),
    ("sed -i references, then writes", "sed -i 's/x/y/' src/m.py", REPO, KNOWN,
     ([("edit", M), ("write", M)], None, None)),
    ("sed -n is a pure read", "sed -n '1,20p' src/m.py", REPO, KNOWN, ([], M, None)),
    ("cp uses its source and writes its target", "cp a.py /tmp/b.py", REPO, KNOWN,
     ([("path", A), ("write", "/tmp/b.py")], None, None)),
    ("git checkout of a known file is a write", "git checkout -- src/m.py", REPO, KNOWN, ([("write", M)], None, None)),
    ("git diff names a file", "git diff src/m.py", REPO, KNOWN, ([("git", M)], None, None)),
    ("a heredoc body is not a command", "cat > t.py << 'EOF'\nimport os; os.remove('a.py')\nEOF\npython t.py",
     REPO, KNOWN, ([("write", "/w/repo/t.py")], None, None)),
    ("env words and wrappers are skipped", "PYTHONPATH=src timeout -s KILL 30 python reproduce.py", REPO, KNOWN,
     ([("run", R)], None, None)),
    ("with no dir, a relative path takes the 1 known file that ends with it", "cat m.py", None, KNOWN, ([], M, None)),
    ("with no dir, an ambiguous suffix stays unresolved", "cat m.py", None, TWO_M, ([], "m.py", None)),
    ("a backslash-newline joins the lines", "python \\\n    src/m.py", REPO, KNOWN, ([("run", M)], None, None)),
    ("a comment ends at the newline, which still separates", "cat src/m.py  # then rm a.py\nrm reproduce.py",
     REPO, KNOWN, ([("path", M), ("write", R)], None, None)),
    ("a quoted operator is a word, not a pipe", "grep -n '|' src/m.py", REPO, KNOWN, ([], M, None)),
    ("escaped quotes stay inside their string", 'python -c "print(\\"a|b\\")" && python a.py', REPO, KNOWN,
     ([("run", A)], None, None)),
    ("rg on a directory is an output", "rg -n foo src/", REPO, KNOWN, ([], None, None)),
    ("rg on a file is a read", "rg -n foo src/m.py", REPO, KNOWN, ([], M, None)),
    ("curl of 1 URL is a URL read", "curl -s https://x.dev/doc", REPO, KNOWN, ([], None, "https://x.dev/doc")),
    ("an escaped backslash before a newline joins nothing", "echo a \\\\\nrm x.py", REPO, KNOWN,
     ([("write", X)], None, None)),
    ("a heredoc line that ends in a backslash still ends at its delimiter",
     "cat > t.sh << 'EOF'\necho hi \\\nEOF\nrm x.py", REPO, KNOWN, ([("write", "/w/repo/t.sh"), ("write", X)], None, None)),
    ("a << in a comment starts no heredoc", "# cat <<EOF\nrm x.py", REPO, KNOWN, ([("write", X)], None, None)),
    ("a << in quotes starts no heredoc", "git commit -m 'parse <<EOF'\nrm x.py", REPO, KNOWN,
     ([("write", X)], None, None)),
    ("a shift is not a heredoc", "echo $((1 << 16))\nrm x.py", REPO, KNOWN, ([("write", X)], None, None)),
    # Where operand roles are known, any literal word is a file, extension or not.
    ("cp keeps operand positions with an extensionless target", "cp a.py Makefile", REPO, KNOWN,
     ([("path", A), ("write", MK)], None, None)),
    ("a target that is no literal word leaves the source a source", 'cp a.py "$DEST"', REPO, KNOWN,
     ([("path", A)], None, None)),
    ("a copy of unknown names into a dir writes the dir", 'cp "$SRC"/*.py build/', REPO, KNOWN,
     ([("write", "/w/repo/build")], None, None)),
    ("rm of an extensionless name is a write", "rm -f Makefile", REPO, KNOWN, ([("write", MK)], None, None)),
    ("a redirect into an extensionless name is a write", "cat > Makefile << 'EOF'\nall:\nEOF", REPO, KNOWN,
     ([("write", MK)], None, None)),
    ("a redirect with no command still writes", "> Makefile", REPO, KNOWN, ([("write", MK)], None, None)),
    ("a reader on an extensionless file is a pure read", "head -n 20 Dockerfile", REPO, KNOWN, ([], DF, None)),
    ("where roles are unknown, an extensionless word is no path", "echo Makefile", REPO, KNOWN, ([], None, None)),
    ("combined short options pass their value on", "grep -nA 3 'def f' src/m.py", REPO, KNOWN, ([], M, None)),
    ("a pattern that starts with + is still the pattern", 'grep -n "+00:59" src/m.py', REPO, KNOWN, ([], M, None)),
    # A wrapper's options that take a value are skipped with it, before the command.
    ("a wrapper's valued options do not hide a read", "timeout -s KILL 30 cat src/m.py", REPO, KNOWN,
     ([], M, None)),
    ("a wrapper's valued options do not hide a write", "timeout -s KILL 30 sed -i 's/x/y/' src/m.py", REPO, KNOWN,
     ([("edit", M), ("write", M)], None, None)),
    ("a run behind a chain of wrappers keeps its subtype",
     "sudo -u dev env -u DEBUG nice -n 5 stdbuf -o L timeout -k 5 30 python a.py", REPO, KNOWN,
     ([("run", A)], None, None)),
]


@pytest.mark.parametrize("name, command, cwd, known, expected", CASES, ids=[c[0] for c in CASES])
def test_shell_effects(name, command, cwd, known, expected):
    eff = effects(command, cwd, known)
    events = [(kind if kind == "write" else eff.why[path], path) for kind, path in eff.events]
    assert (events, eff.read, eff.url) == expected, name
