import pytest

from research.ctxcache.bash import effects

REPO = "/w/repo"
KNOWN = {"/w/repo/src/m.py", "/w/repo/a.py", "/w/repo/reproduce.py"}
M, A, R = "/w/repo/src/m.py", "/w/repo/a.py", "/w/repo/reproduce.py"
TWO_M = KNOWN | {"/w/repo/lib/m.py"}

# name, command, cwd, known files, expected (events, pure read, URL read)
CASES = [
    ("a redirect writes its target", "python a.py > out.txt", REPO, KNOWN,
     ([("use", A), ("write", "/w/repo/out.txt")], None, None)),
    ("an fd redirect is not a file", "python a.py 2>&1 | tail -5", REPO, KNOWN, ([("use", A)], None, None)),
    ("an append is a write", "echo done >> notes.md", REPO, KNOWN, ([("write", "/w/repo/notes.md")], None, None)),
    ("sed -i references, then writes", "sed -i 's/x/y/' src/m.py", REPO, KNOWN, ([("use", M), ("write", M)], None, None)),
    ("sed -n is a pure read", "sed -n '1,20p' src/m.py", REPO, KNOWN, ([], M, None)),
    ("cp uses its source and writes its target", "cp a.py /tmp/b.py", REPO, KNOWN,
     ([("use", A), ("write", "/tmp/b.py")], None, None)),
    ("git checkout of a known file is a write", "git checkout -- src/m.py", REPO, KNOWN, ([("write", M)], None, None)),
    ("git diff names a file", "git diff src/m.py", REPO, KNOWN, ([("use", M)], None, None)),
    ("a heredoc body is not a command", "cat > t.py << 'EOF'\nimport os; os.remove('a.py')\nEOF\npython t.py",
     REPO, KNOWN, ([("write", "/w/repo/t.py")], None, None)),
    ("env words and wrappers are skipped", "PYTHONPATH=src timeout -s KILL 30 python reproduce.py", REPO, KNOWN,
     ([("use", R)], None, None)),
    ("with no dir, a relative path takes the 1 known file that ends with it", "cat m.py", None, KNOWN, ([], M, None)),
    ("with no dir, an ambiguous suffix stays unresolved", "cat m.py", None, TWO_M, ([], "m.py", None)),
    ("a backslash-newline joins the lines", "python \\\n    src/m.py", REPO, KNOWN, ([("use", M)], None, None)),
    ("a comment ends at the newline, which still separates", "cat src/m.py  # then rm a.py\nrm reproduce.py",
     REPO, KNOWN, ([("use", M), ("write", R)], None, None)),
    ("a quoted operator is a word, not a pipe", "grep -n '|' src/m.py", REPO, KNOWN, ([], M, None)),
    ("escaped quotes stay inside their string", 'python -c "print(\\"a|b\\")" && python a.py', REPO, KNOWN,
     ([("use", A)], None, None)),
    ("rg on a directory is an output", "rg -n foo src/", REPO, KNOWN, ([], None, None)),
    ("rg on a file is a read", "rg -n foo src/m.py", REPO, KNOWN, ([], M, None)),
    ("curl of 1 URL is a URL read", "curl -s https://x.dev/doc", REPO, KNOWN, ([], None, "https://x.dev/doc")),
]


@pytest.mark.parametrize("name, command, cwd, known, expected", CASES, ids=[c[0] for c in CASES])
def test_shell_effects(name, command, cwd, known, expected):
    eff = effects(command, cwd, known)
    assert (eff.events, eff.read, eff.url) == expected, name
