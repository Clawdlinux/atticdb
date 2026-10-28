"""Trace records and the object model that every adapter shares.

The context window is a cache over the trajectory, which acts as the durable log.
Objects are what tool outputs put in context: a file version as shown, a URL, or
another output. An access is 1 tool output, or 1 later action that names an object.
README.md states the rules. This module is the only place that applies them.
"""

from __future__ import annotations

import hashlib
import json
from collections import Counter
from dataclasses import dataclass


def tokens(text: str) -> int:
    """Characters / 4, at least 1."""
    return max(1, len(text) // 4)


@dataclass(frozen=True)
class Access:
    obj: str  # hashed key
    tokens: int  # the object's size, fixed at its first access
    kind: str  # "file", "url" or "output"
    first: bool  # the first access to the object: a compulsory miss
    ref: bool  # a later action named the object. False for a tool output.


@dataclass
class View:
    obj: str
    lines: tuple[int, int] | None  # first and last line number shown
    text: str  # the shown text without line numbers, to match edit targets


class TraceBuilder:
    """Turns 1 trajectory's tool outputs and actions into accesses."""

    def __init__(self, traj: str, salt: str = "ctxcache-v1"):
        self.traj, self.salt = traj, salt
        self.accesses: list[Access] = []
        self.version: dict[str, int] = {}  # path -> writes seen so far
        self.world = 0  # writes so far, over all paths
        self.views: dict[tuple[str, int], list[View]] = {}  # newest last
        self.urls: dict[str, str] = {}  # url -> newest object
        self.size: dict[str, int] = {}
        self.stats: Counter = Counter()

    def key(self, *parts) -> str:
        h = hashlib.sha256(self.salt.encode())
        for p in (self.traj, *parts):
            h.update(b"\0" + str(p).encode("utf-8", "surrogatepass"))
        return h.hexdigest()[:16]

    def _access(self, obj: str, kind: str, size: int, ref: bool) -> None:
        first = obj not in self.size
        if first:
            self.size[obj] = size
        self.accesses.append(Access(obj, self.size[obj], kind, first, ref))

    def read_file(self, path: str, body: str, lines=None, text: str | None = None) -> str:
        """A tool output that shows `path` at its current version. `body` is the shown
        text without tool chrome, so the same lines shown twice are the same object.
        The key and the size both come from `body`."""
        v = self.version.setdefault(path, 0)
        obj = self.key("file", path, v, body)
        self._access(obj, "file", tokens(body), ref=False)
        views = [w for w in self.views.get((path, v), []) if w.obj != obj]
        views.append(View(obj, lines, body if text is None else text))
        self.views[(path, v)] = views
        return obj

    def read_url(self, url: str, request: str, body: str) -> str:
        obj = self.key("url", url, request, body)
        self._access(obj, "url", tokens(body), ref=False)
        self.urls[url] = obj
        return obj

    def output(self, tool: str, request: str, body: str) -> str:
        """Any other tool output. The same request and text with no write in between
        is the same object, so an unchanged rerun counts as a re-read."""
        obj = self.key("output", tool, request, body, self.world)
        self._access(obj, "output", tokens(body), ref=False)
        return obj

    def use(self, path: str, line: int | None = None, text: str | None = None) -> bool:
        """An action names `path` without reading it. That references a view of the current
        version: the newest that shows `line`, or else the newest that contains all of `text`,
        or else its longest line. Otherwise the newest view. With no view of the current
        version there is nothing to reference."""
        views = self.views.get((path, self.version.get(path, 0)))
        if not views:
            self.stats["refs_without_view"] += 1
            return False
        self._access(_pick(views, line, text).obj, "file", 0, ref=True)
        return True

    def use_url(self, url: str) -> bool:
        obj = self.urls.get(url)
        if obj is not None:
            self._access(obj, "url", 0, ref=True)
        return obj is not None

    def write(self, path: str) -> None:
        """An edit, create, delete or move. The old version is dead after it."""
        self.version[path] = self.version.get(path, 0) + 1
        self.world += 1
        self.stats["writes"] += 1

    def known(self) -> set[str]:
        return set(self.version)


def _pick(views: list[View], line: int | None, text: str | None) -> View:
    newest_first = views[::-1]
    if line is not None:
        return next((w for w in newest_first if w.lines and w.lines[0] <= line <= w.lines[1]), views[-1])
    if text:
        # An edit's old text appears verbatim in the file, so a view that shows it holds all of it.
        match = next((w for w in newest_first if text in w.text), None)
        longest = max((part.strip() for part in text.split("\n")), key=len)
        if match is None and len(longest) >= 4:
            match = next((w for w in newest_first if longest in w.text), None)
        if match is not None:
            return match
    return views[-1]


def check(accesses: list[Access]) -> None:
    """Raises ValueError unless first flags and sizes are consistent per object."""
    size: dict[str, int] = {}
    for t, a in enumerate(accesses):
        if (a.obj not in size) != a.first:
            raise ValueError(f"access {t}: first={a.first} does not match the object's history")
        if a.first and a.ref:
            raise ValueError(f"access {t}: a reference cannot be a first access")
        if size.setdefault(a.obj, a.tokens) != a.tokens:
            raise ValueError(f"access {t}: object size changed")
        if a.tokens < 1:
            raise ValueError(f"access {t}: tokens must be at least 1")


def dump(traj: str, accesses: list[Access], out) -> None:
    for t, a in enumerate(accesses):
        line = {"traj": traj, "t": t, "obj": a.obj, "tokens": a.tokens, "kind": a.kind, "first": a.first, "ref": a.ref}
        out.write(json.dumps(line) + "\n")


def load(lines) -> dict[str, list[Access]]:
    """Reads trace lines, groups them by trajectory in `t` order, and checks them."""
    rows: dict[str, list[tuple[int, Access]]] = {}
    for line in lines:
        if not line.strip():
            continue
        o = json.loads(line)
        a = Access(o["obj"], int(o["tokens"]), o["kind"], bool(o["first"]), bool(o.get("ref", False)))
        rows.setdefault(str(o["traj"]), []).append((int(o["t"]), a))
    out = {}
    for traj, items in rows.items():
        items.sort(key=lambda x: x[0])
        out[traj] = [a for _, a in items]
        check(out[traj])
    return out
