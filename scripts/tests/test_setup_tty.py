"""Drives `atticdb setup` through a real terminal, as a person would, against ATTICDB_BIN.

Each test gets a short temp HOME under /tmp, with a minimal PATH, so the real HOME is
never touched and detection finds only the apps the test creates. Removing the home stops
any server setup started, as `serve` exits when its LOCK is gone (D114).
"""

import os
import re
import select
import shutil
import subprocess
import tempfile
import time

import pytest

BIN = os.environ.get("ATTICDB_BIN", "")

pytestmark = [
    pytest.mark.skipif(os.name == "nt", reason="pty is Unix only"),
    pytest.mark.skipif(not BIN or not os.path.isfile(BIN), reason="ATTICDB_BIN is unset or not a file"),
]

APPS = "Which apps? [claude-code,codex] "
HOOK = "Load memory at chat start, and save Claude Code's compaction summaries? [Y/n] "
HOOK_CODEX = "Load memory at the start of each chat? [Y/n] "
NOTES = "Save notes without asking? (Claude Code and Codex. Saving a rule always asks.) [Y/n] "
APPLY = "Apply? [y/N] "


@pytest.fixture
def home():
    # Short, so the server's socket path stays under the 104-byte sun_path limit on macOS.
    root = tempfile.mkdtemp(prefix="su", dir="/tmp")
    path = os.path.join(root, "h")
    os.mkdir(path, 0o700)
    # A config dir each, so setup finds exactly Claude Code and Codex.
    os.mkdir(os.path.join(path, ".claude"))
    os.mkdir(os.path.join(path, ".codex"))
    yield path
    shutil.rmtree(root, ignore_errors=True)
    deadline = time.time() + 10
    while time.time() < deadline and servers(root):
        time.sleep(0.2)
    assert not servers(root), "a server outlived its home"


def servers(root):
    ps = subprocess.run(["ps", "-eo", "args="], capture_output=True, text=True, check=True).stdout
    return [line for line in ps.splitlines() if root in line and " serve " in line]


def setup(home, answers, timeout=60):
    """Runs `atticdb setup` on a pty. Sends each answer once its prompt shows. Returns the
    exit code, the output without colors or CRs, and the prompts that weren't reached."""
    import pty

    env = {"PATH": "/usr/bin:/bin", "HOME": home, "TERM": "xterm-256color", "LANG": "C.UTF-8"}
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(BIN, [BIN, "setup"], env)
    pending, out, seen = list(answers), "", 0
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096).decode(errors="replace")
        except OSError:
            break
        if not chunk:
            break
        out += chunk
        if pending and pending[0][0] in out[seen:]:
            seen = out.index(pending[0][0], seen) + len(pending[0][0])
            os.write(fd, (pending.pop(0)[1] + "\n").encode())
    else:
        os.kill(pid, 9)
    _, status = os.waitpid(pid, 0)
    os.close(fd)
    text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", out).replace("\r", "")
    return os.waitstatus_to_exitcode(status), text, [prompt for prompt, _ in pending]


def test_setup_asks_then_connects_and_checks(home):
    # Codex only: Claude Code's MCP entry needs the claude CLI, which CI doesn't have.
    code, out, missed = setup(home, [(APPS, "codex"), (HOOK_CODEX, ""), (NOTES, ""), (APPLY, "y")])
    assert missed == [], out
    assert code == 0, out
    # The prompts come in this order, and the plan shows before anything is written.
    order = [out.index(text) for text in (APPS, HOOK_CODEX, NOTES, "Plan:\n", APPLY, "Applying:\n")]
    assert order == sorted(order), out
    assert "\n1 connected, 0 with skipped parts.\n" in out, out
    assert "\n  Codex: ok\n" in out, out
    assert "\nNext:\n" in out and "\nTry it: " in out, out
    assert os.path.isfile(os.path.join(home, ".codex", "config.toml"))
    assert not os.path.exists(os.path.join(home, ".claude.json"))


def test_setup_writes_nothing_when_declined(home):
    before = sorted(os.listdir(home))
    code, out, missed = setup(home, [(APPS, "codex"), (HOOK_CODEX, "n"), (NOTES, "n"), (APPLY, "")])
    assert missed == [], out
    assert code == 0, out
    assert "Nothing changed.\n" in out, out
    assert sorted(os.listdir(home)) == before
    assert os.listdir(os.path.join(home, ".codex")) == []


def test_setup_asks_again_after_a_bad_answer(home):
    code, out, missed = setup(home, [(APPS, "cursor"), (APPS, "codex"), (HOOK_CODEX, "maybe"),
                                     ("Answer y or n.", ""), (NOTES, ""), (APPLY, "n")])
    assert missed == [], out
    assert code == 0, out
    assert "Answer with a comma list of copilot, vscode, claude-code, codex" in out, out
    assert "Nothing changed.\n" in out, out
