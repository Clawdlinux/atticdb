"""Mechanical grading on the lines the agent added under app/. A verdict is `rule`, `code`,
`mixed` or `none`. Every row still gets read by hand before a number is reported."""

from __future__ import annotations

import os
import re
import subprocess
import sys

from .fixtures import git

KEY = re.compile(r"""["']([A-Za-z][A-Za-z0-9_]*)["']\s*:""")
CAMEL = re.compile(r"[a-z][A-Z]")
# Global pytest plugins on this machine can fail to import. The fixtures need none.
PYTEST_ENV = {**os.environ, "PYTEST_DISABLE_PLUGIN_AUTOLOAD": "1"}


def added_lines(repo: str, base: str) -> list[str]:
    """Lines added under app/ since `base`, new files included. Stages everything first: the
    repo is a throwaway copy."""
    git(repo, "add", "-A")
    diff = git(repo, "diff", "--cached", base, "--", "app")
    return [line[1:] for line in diff.splitlines() if line.startswith("+") and not line.startswith("+++")]


def _signals(convention: str, lines: list[str]) -> tuple[bool, bool]:
    """(follows the rule, follows the code)."""
    text = "\n".join(line for line in lines if not line.lstrip().startswith("#"))
    if convention == "errors":
        rule = bool(re.search(r"\braise\s+(?:LookupError|KeyError|IndexError)\b", text))
        return rule, not rule and bool(re.search(r"\.get\(|return None", text))
    if convention == "timestamps":
        return "isoformat" in text, bool(re.search(r"time\.time\(|\.timestamp\(", text))
    if convention == "keys":
        keys = KEY.findall(text)
        return any(CAMEL.search(k) for k in keys), any("_" in k for k in keys)
    if convention == "ids":
        return "uuid" in text, bool(re.search(r"\bcount\(|\bnext\(|\+= 1", text))
    raise ValueError(convention)


def verdict(convention: str, lines: list[str]) -> str:
    rule, code = _signals(convention, lines)
    if rule and code:
        return "mixed"
    return "rule" if rule else "code" if code else "none"


def correct(history: str, found: str) -> bool:
    return found == ("code" if history == "stale" else "rule")


def tests_pass(repo: str, timeout: float = 120.0) -> bool:
    try:
        run = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider"], cwd=repo,
                             capture_output=True, timeout=timeout, env=PYTEST_ENV)
    except subprocess.TimeoutExpired:
        return False
    return run.returncode == 0
