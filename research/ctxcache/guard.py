"""Keeps downloaded rows, exported traces and labeling sheets out of the repo.

Every check compares files, not strings, after symlinks resolve. So an alias of the
checkout still counts: another case on a case-insensitive disk, a firmlink such as
/System/Volumes/Data/..., or a symlink anywhere on the path.

No path check can see a hard link: an outside name for a repo file. So every write
goes through `replacing`, which renames a new file over the destination. A rename
replaces the name, so the linked repo file keeps its content.
"""

from __future__ import annotations

import contextlib
import os
import tempfile

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


@contextlib.contextmanager
def replacing(path: str, what: str, mode: str = "w", root: str = REPO, **kw):
    """A new file that replaces `path` when the block ends without error. On error,
    `path` stays as it was. The new file sits in the same folder, so the rename is
    atomic. Refuses a `path` that resolves inside `root`."""
    outside(path, what, root)
    folder = os.path.dirname(os.path.abspath(path))
    os.makedirs(folder, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".", suffix=".tmp", dir=folder)
    try:
        with os.fdopen(fd, mode, **kw) as f:
            os.chmod(tmp, 0o666 & ~_umask())  # as a plain open() would create it
            yield f
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise


def _umask() -> int:
    mask = os.umask(0)
    os.umask(mask)
    return mask
