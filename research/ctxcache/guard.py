"""Keeps downloaded rows, exported traces and labeling sheets out of the repo.

Every check compares files, not strings, after symlinks resolve. So an alias of the
checkout still counts: another case on a case-insensitive disk, a firmlink such as
/System/Volumes/Data/..., or a symlink anywhere on the path.
"""

from __future__ import annotations

import os

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class InsideRepo(OSError):
    """A write would land in the repo."""


def inside(path: str, root: str = REPO) -> bool:
    """True if `path` is `root` or lies below it. Parts that do not exist yet are skipped."""
    p = os.path.realpath(path)
    while True:
        try:
            if os.path.samefile(p, root):
                return True
        except OSError:
            pass  # not created yet
        parent = os.path.dirname(p)
        if parent == p:
            return False
        p = parent


def outside(path: str, what: str, root: str = REPO) -> str:
    """`path`, if a write there lands outside `root`. Else raises InsideRepo."""
    if inside(path, root):
        raise InsideRepo(f"{what} {path} resolves inside the repo, so it is not written")
    return path
