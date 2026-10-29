"""Safety checks: the eval config dirs, every child's env, the real zerodb home, and redaction.

The eval must never touch the user's own Claude Code, Codex or Copilot config, or the
user's own zerodb store. So:

- `check_config_dirs` refuses eval dirs that are, or lie in, `~/.claude*`, `~/.codex` or
  `~/.copilot`, through any symlink.
- `check_env` refuses to start a child whose env has no `ZERODB_HOME` under the run's root.
- `snapshot` records the size and mtime of each file in the real home's `data/`. It only
  stats them and never opens one.
"""

from __future__ import annotations

import os
import re
import sys

EVAL_DIR = os.path.join(".cache", "zerodb-setup-eval")
HOME_ENV = "ZERODB_HOME"


class Refused(Exception):
    """A setting the guard refuses. Nothing ran."""


class UnsafeEnv(Refused):
    """A child env without `ZERODB_HOME` under the run root. The child is not started."""


def user_home(environ=None) -> str:
    environ = os.environ if environ is None else environ
    home = environ.get("HOME", "")
    if not os.path.isabs(home):
        raise Refused("HOME must be set to an absolute path")
    return home


def eval_dirs(home: str) -> tuple[str, str]:
    """The `CLAUDE_CONFIG_DIR` and `CODEX_HOME` that only the eval uses."""
    root = os.path.join(home, EVAL_DIR)
    return os.path.join(root, "claude"), os.path.join(root, "codex")


def _below(path: str, root: str) -> bool:
    """True if `path` is `root` or lies below it, comparing folders as files, so case and
    firmlink aliases count. Parts that do not exist are skipped."""
    p = path
    while True:
        try:
            if os.path.samefile(p, root):
                return True
        except OSError:
            pass
        parent = os.path.dirname(p)
        if parent == p:
            return False
        p = parent


def _forbidden_name(path: str, home: str) -> str | None:
    """The `~/.claude*`, `~/.codex` or `~/.copilot` entry that `path` is or lies in, by name."""
    rel = os.path.relpath(path, home)
    if rel == ".":
        return "~"
    if rel == ".." or rel.startswith(".." + os.sep):
        return None
    first = rel.split(os.sep)[0].lower()
    if first.startswith(".claude") or first in (".codex", ".copilot"):
        return "~/" + rel.split(os.sep)[0]
    return None


def check_config_dirs(claude_dir: str, codex_dir: str, home: str) -> None:
    """Refuses unless both dirs are the eval's own: absolute, distinct, and neither `~` nor
    in `~/.claude*`, `~/.codex` or `~/.copilot`, as written or after symlinks resolve."""
    real_home = os.path.realpath(home)
    for what, path in (("CLAUDE_CONFIG_DIR", claude_dir), ("CODEX_HOME", codex_dir)):
        if not os.path.isabs(path):
            raise Refused(f"{what} must be an absolute path")
        for form, base in ((os.path.abspath(path), home), (os.path.realpath(path), real_home)):
            hit = _forbidden_name(form, base)
            if hit:
                raise Refused(f"{what} {path} is or lies in {hit}, the user's own config. Refused")
        for name in (".claude", ".codex", ".copilot"):
            own = os.path.join(home, name)
            if _below(os.path.realpath(path), own) or _below(os.path.abspath(path), own):
                raise Refused(f"{what} {path} is or lies in ~/{name}, the user's own config. Refused")
        if _below(home, os.path.realpath(path)) or os.path.realpath(path) == real_home:
            raise Refused(f"{what} {path} holds the home directory. Refused")
    if os.path.realpath(claude_dir) == os.path.realpath(codex_dir):
        raise Refused("CLAUDE_CONFIG_DIR and CODEX_HOME must differ")


def check_env(env: dict, root: str) -> None:
    """Raises UnsafeEnv unless `env` sets `ZERODB_HOME` to an absolute path under `root`.
    An explicit raise, not `assert`, so `python -O` keeps it."""
    home = env.get(HOME_ENV, "")
    if not home or not os.path.isabs(home):
        raise UnsafeEnv(f"a child env has no absolute {HOME_ENV}. Refused")
    if not _below(home, root) and not os.path.realpath(home).startswith(os.path.realpath(root) + os.sep):
        raise UnsafeEnv(f"a child env has {HOME_ENV} outside its run root. Refused")


def real_data_dir(environ=None) -> str:
    """The default zerodb home's `data/`, as zerodb picks it with no `ZERODB_HOME`."""
    environ = os.environ if environ is None else environ
    home = user_home(environ)
    if sys.platform == "darwin":
        return os.path.join(home, "Library", "Application Support", "zerodb", "data")
    xdg = environ.get("XDG_DATA_HOME", "")
    base = xdg if os.path.isabs(xdg) else os.path.join(home, ".local", "share")
    return os.path.join(base, "zerodb", "data")


def snapshot(data_dir: str) -> dict[str, tuple[int, int]]:
    """Each file under `data_dir` as relative path -> (size, mtime_ns). Stats only: no file is
    opened. A missing dir gives {}."""
    out: dict[str, tuple[int, int]] = {}
    if not os.path.isdir(data_dir):
        return out
    for folder, dirs, files in os.walk(data_dir, followlinks=False):
        for name in files:
            path = os.path.join(folder, name)
            try:
                st = os.lstat(path)
            except OSError:
                continue  # removed while walking
            out[os.path.relpath(path, data_dir)] = (st.st_size, st.st_mtime_ns)
    return out


def compare(before: dict, after: dict) -> list[str]:
    """The relative paths that were added, removed or changed in size or mtime."""
    return sorted(k for k in before.keys() | after.keys() if before.get(k) != after.get(k))


# Simple token shapes. A match is replaced whole, so a reply excerpt never carries one.
_TOKEN_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{16,}"),
    re.compile(r"gh[pousr]_[A-Za-z0-9]{20,}"),
    re.compile(r"github_pat_[A-Za-z0-9_]{20,}"),
    re.compile(r"xox[abprs]-[A-Za-z0-9\-]{10,}"),
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*"),
    re.compile(r"(?i)\bbearer\s+\S+"),
    re.compile(r"(?i)\b(token|secret|password|passwd|api[_-]?key)(\s*[:=]\s*)\S+"),
    re.compile(r"\b[0-9a-fA-F]{32,}\b"),
    re.compile(r"[A-Za-z0-9+_\-]{40,}={0,2}"),
]


def redact(text: str, paths: dict[str, str] | None = None) -> str:
    """`text` with token shapes replaced by [redacted], and each path in `paths` replaced by
    its label, longest first, so env values such as the run root never reach the results."""
    for path, label in sorted((paths or {}).items(), key=lambda kv: -len(kv[0])):
        if path:
            text = text.replace(path, label)
    for pattern in _TOKEN_PATTERNS:
        if pattern.groups == 2:
            text = pattern.sub(lambda m: f"{m.group(1)}{m.group(2)}[redacted]", text)
        else:
            text = pattern.sub("[redacted]", text)
    return text


def excerpt(text: str, paths: dict[str, str] | None = None, limit: int = 200) -> str:
    """The first `limit` characters of `text` after redaction, on 1 line."""
    flat = " ".join(redact(text or "", paths).split())
    return flat[:limit]
