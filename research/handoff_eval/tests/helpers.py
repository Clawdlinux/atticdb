"""setup_eval's test home, with the handoff stub installed as `claude` and `codex`."""

from __future__ import annotations

import os
import stat
import sys

from research.setup_eval.tests.helpers import ATTICDB, fake_home as _fake_home, stub_calls  # noqa: F401

HERE = os.path.dirname(os.path.abspath(__file__))
STUB_VARS = ("STUB_WRITER", "STUB_NATIVE_MEMORY")


def install_stubs(bin_dir) -> None:
    with open(os.path.join(HERE, "stub_agent.py")) as f:
        body = f.read()
    for name in ("claude", "codex"):
        path = os.path.join(bin_dir, name)
        with open(path, "w") as f:
            f.write(f"#!{sys.executable}\n{body}")
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def fake_home(monkeypatch, tmp_path, make_eval_dirs: bool = True) -> dict:
    paths = _fake_home(monkeypatch, tmp_path, make_eval_dirs)
    install_stubs(paths["bin"])
    for name in STUB_VARS:
        monkeypatch.delenv(name, raising=False)
    return paths


def agent_calls(log: str) -> list[dict]:
    """The stub calls that were agent sessions, in order."""
    return [c for c in stub_calls(log) if c["argv"][:1] in (["-p"], ["exec"])]
