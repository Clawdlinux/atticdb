"""Stub agents on PATH, and a home that is not the user's."""

from __future__ import annotations

import json
import os
import stat
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(os.path.dirname(HERE), "fixtures")
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
ZERODB = os.path.join(REPO_ROOT, "target", "debug", "zerodb")


def install_stubs(bin_dir) -> None:
    """`claude` and `codex` in `bin_dir`: the stub, run by this Python."""
    os.makedirs(bin_dir, exist_ok=True)
    with open(os.path.join(HERE, "stub_agent.py")) as f:
        body = f.read()
    for name in ("claude", "codex"):
        path = os.path.join(bin_dir, name)
        with open(path, "w") as f:
            f.write(f"#!{sys.executable}\n{body}")
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def fake_home(monkeypatch, tmp_path, make_eval_dirs: bool = True) -> dict:
    """HOME in tmp_path, stubs first on a PATH with only system dirs after them, and a stub log.
    Nothing under the user's real home is used."""
    home = tmp_path / "home"
    home.mkdir()
    bin_dir = tmp_path / "bin"
    install_stubs(bin_dir)
    log = tmp_path / "stub-log.jsonl"
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("PATH", f"{bin_dir}:/usr/bin:/bin")
    monkeypatch.setenv("STUB_LOG", str(log))
    monkeypatch.delenv("XDG_DATA_HOME", raising=False)
    for name in ("ZERODB_HOME", "ZERODB_TOKEN", "CLAUDE_CONFIG_DIR", "CODEX_HOME", "STUB_REPLAY",
                 "STUB_SLEEP", "STUB_LOGGED_OUT", "STUB_EXIT", "STUB_STDERR"):
        monkeypatch.delenv(name, raising=False)
    claude_dir = home / ".cache" / "zerodb-setup-eval" / "claude"
    codex_dir = home / ".cache" / "zerodb-setup-eval" / "codex"
    if make_eval_dirs:
        claude_dir.mkdir(parents=True)
        codex_dir.mkdir(parents=True)
    return {"home": str(home), "bin": str(bin_dir), "log": str(log), "claude_dir": str(claude_dir),
            "codex_dir": str(codex_dir)}


def stub_calls(log: str) -> list[dict]:
    if not os.path.exists(log):
        return []
    with open(log) as f:
        return [json.loads(line) for line in f if line.strip()]
