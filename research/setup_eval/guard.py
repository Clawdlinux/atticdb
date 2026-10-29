"""Safety checks: the eval config dirs, every child's env, the real zerodb home, and redaction.

The eval must never touch the user's own Claude Code, Codex or Copilot config, or the
user's own zerodb store. So:

- `check_config_dirs` refuses eval dirs that are, or lie in, `~/.claude*`, `~/.codex` or
  `~/.copilot`, through any symlink. Each existing such entry is compared by file identity
  and ancestry, both ways. An entry that can't be inspected is refused.
- `check_env` refuses to start a child whose env has no `ZERODB_HOME` under the run's root,
  after both resolve.
- `snapshot` records the size and mtime of each file in the real home's `data/`, and in
  `$ZERODB_HOME/data` when the parent env sets it. `config_snapshot` records the names, sizes
  and mtimes at the top level of `~/.claude*`, `~/.codex` and `~/.copilot`. Both only stat,
  and never open a file.
"""

from __future__ import annotations

import os
import re
import stat
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


def _own_name(name: str) -> bool:
    low = name.lower()
    return low.startswith(".claude") or low in (".codex", ".copilot")


def _key(path: str) -> str:
    """`path` for string comparison: case-folded on macOS, whose disks usually ignore case."""
    return path.casefold() if sys.platform == "darwin" else path


def _related(a: str, b: str) -> bool:
    """True if `a` and `b` are 1 file, or 1 lies in the other: by file identity where the
    paths exist, and by resolved path text where they don't yet."""
    if _below(a, b) or _below(b, a):
        return True
    ka, kb = _key(a), _key(b)
    return ka == kb or ka.startswith(kb.rstrip(os.sep) + os.sep) or kb.startswith(ka.rstrip(os.sep) + os.sep)


def own_entries(home: str) -> list[str]:
    """Every `~/.claude*`, `~/.codex` and `~/.copilot` entry, by name only. Refuses when the
    home can't be listed, as the eval dirs could not be checked against them."""
    try:
        names = os.listdir(home)
    except OSError as e:
        raise Refused(f"cannot list the home to find ~/.claude* entries ({e.strerror}). Refused") from None
    return sorted(os.path.join(home, n) for n in names if _own_name(n))


def _check_against_own(what: str, path: str, home: str) -> None:
    """Refuses `path` when it and any existing user config entry are 1 file, or 1 lies in the
    other, as written or resolved. Refuses when an entry can't be inspected."""
    forms = {os.path.abspath(path), os.path.realpath(path)}
    for entry in own_entries(home):
        name = "~/" + os.path.basename(entry)
        try:
            os.lstat(entry)
        except FileNotFoundError:
            continue  # removed since the listing
        except OSError as e:
            raise Refused(f"{name} can't be inspected ({e.strerror}). Refused") from None
        try:
            os.stat(entry)  # follows a link, so a loop or a denied target shows here
        except FileNotFoundError:
            pass  # a dangling link: its resolved path is still compared
        except OSError as e:
            raise Refused(f"{name} can't be inspected ({e.strerror}). Refused") from None
        for own in {entry, os.path.realpath(entry)}:
            if any(_related(form, own) for form in forms):
                raise Refused(f"{what} {path} and {name}, the user's own config, overlap. Refused")


def check_config_dirs(claude_dir: str, codex_dir: str, home: str) -> None:
    """Refuses unless both dirs are the eval's own: absolute, distinct, and neither `~` nor
    in `~/.claude*`, `~/.codex` or `~/.copilot`, as written or after symlinks resolve. Every
    existing `~/.claude*`, `~/.codex` and `~/.copilot` entry is also compared by file identity
    and ancestry, both ways, so a link from either side is caught."""
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
        _check_against_own(what, path, home)
    if os.path.realpath(claude_dir) == os.path.realpath(codex_dir):
        raise Refused("CLAUDE_CONFIG_DIR and CODEX_HOME must differ")


def check_env(env: dict, root: str) -> None:
    """Raises UnsafeEnv unless `env` sets `ZERODB_HOME` to an absolute path strictly under
    `root`, after both resolve. An explicit raise, not `assert`, so `python -O` keeps it."""
    home = env.get(HOME_ENV, "")
    if not home or not os.path.isabs(home):
        raise UnsafeEnv(f"a child env has no absolute {HOME_ENV}. Refused")
    # Both resolved first, so a link at or above `<root>/zh` that points out is refused.
    real_home, real_root = os.path.realpath(home), os.path.realpath(root)
    inside = (real_home.startswith(real_root.rstrip(os.sep) + os.sep)
              or _below(os.path.dirname(real_home), real_root))
    if real_home == real_root or not inside:
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


def watched_data_dirs(environ=None) -> list[tuple[str, str]]:
    """The zerodb data dirs a batch must leave alone, as (label, path): the default home's,
    and `$ZERODB_HOME/data` when the parent env sets an absolute `ZERODB_HOME`. The label
    never holds the env value."""
    environ = os.environ if environ is None else environ
    dirs = [("the real zerodb home's data dir", real_data_dir(environ))]
    parent = environ.get(HOME_ENV, "")
    if os.path.isabs(parent):
        data = os.path.join(parent, "data")
        if os.path.realpath(data) != os.path.realpath(dirs[0][1]):
            dirs.append(("$ZERODB_HOME/data of the parent env", data))
    return dirs


def _stat(path: str, follow: bool = False) -> tuple:
    try:
        st = os.stat(path, follow_symlinks=follow)
    except FileNotFoundError:
        return ("missing",)
    except OSError as e:
        return ("error", e.errno)
    return (stat.S_IFMT(st.st_mode), st.st_size, st.st_mtime_ns)


def config_snapshot(home: str) -> dict[str, tuple]:
    """The top level of the user's own agent config, as label -> (type, size, mtime_ns): each
    `~/.claude*` entry (`~/.claude.json` too), `~/.codex` and `~/.copilot`, and each entry
    directly inside those that are dirs. Names and stats only: no file is opened, and no
    dir below the top level is listed."""
    out: dict[str, tuple] = {}
    try:
        entries = own_entries(home)
    except Refused:
        return {"~": ("unlistable",)}
    for entry in entries:
        label = "~/" + os.path.basename(entry)
        out[label] = _stat(entry)
        if not os.path.isdir(entry):  # follows a link to a dir
            continue
        try:
            with os.scandir(entry) as it:
                for child in it:
                    out[f"{label}/{child.name}"] = _stat(child.path)
        except OSError as e:
            out[f"{label}/"] = ("unlistable", e.errno)
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
