"""Keeps downloaded rows, exported traces and labeling sheets out of the repo.

Every check compares files, not strings, so an alias of the checkout still counts:
another case on a case-insensitive disk, a firmlink such as /System/Volumes/Data/...,
or a symlink anywhere on the path. A path counts as inside if it lies in the repo as
written or after symlinks resolve.

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
    """A write the guard refuses, because it could land in the repo."""


def inside(path: str, root: str = REPO) -> bool:
    """True if `path` is `root` or lies below it, as written or after symlinks resolve.
    As written counts too: a link in the repo that points out still sits in the repo."""
    return _below(os.path.abspath(path), root) or _below(os.path.realpath(path), root)


def _below(p: str, root: str) -> bool:
    """Walks up from `p`, comparing each folder with `root` as a file. Parts that do
    not exist yet are skipped."""
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
    """`path`, if it lies outside `root`. Else raises InsideRepo."""
    if inside(path, root):
        raise InsideRepo(f"{what} {path} lies inside the repo, so it is not written")
    return path


@contextlib.contextmanager
def replacing(path: str, what: str, mode: str = "w", root: str = REPO, **kw):
    """A new file that replaces `path` when the block ends without error. On error,
    `path` stays as it was. The new file is made and renamed in `path`'s own folder,
    so the rename is atomic. That folder and `path` must lie outside `root`. A symlink
    at `path` is refused: the rename would replace the link where it sits."""
    if os.path.islink(path):
        raise InsideRepo(f"{what} {path} is a symlink, so it is not written")
    folder = os.path.dirname(os.path.abspath(path))
    outside(folder, f"the folder of {what}", root)  # where the new file is made and renamed
    outside(path, what, root)
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
