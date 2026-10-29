"""Our own Copilot CLI session logs as traces. Report aggregates only.

Reads ~/.copilot/session-state/*/events.jsonl. Only the main agent's tool calls are
accesses. Sub-agent edits still make new file versions. A session that shares at
least half its tool calls with a longer session is a fork, and is skipped.
Keys are salted with a random value per run, so a hashed path cannot be guessed.
"""

from __future__ import annotations

import hashlib
import json
import os
import posixpath
import re

from . import bash
from .trace import Access, TraceBuilder, tokens

ROOT = os.path.expanduser("~/.copilot/session-state")
PATCH = re.compile(r"^\*\*\* (Update|Add|Delete) File: (.+?)\s*$|^\*\*\* Move to: (.+?)\s*$", re.M)
NOT_CONTENT = ("File too large", "Output too large", "Path does not exist", "Error")
# The harness ends a bash result with 1 line such as "<shellId: 12 completed with exit code 0>",
# or a note that the command still runs. It is chrome, and the id differs on every call.
TRAILER = re.compile(r"\n?<(?:shellId: [^\n>]+ completed with exit code (-?\d+)|command [^\n]*shellId[^\n]*)>\s*$")


def session(path: str, traj: str, salt: str) -> tuple[list[Access], dict, set[str]]:
    """1 session: its accesses, its counts, and the ids of its main-agent tool calls."""
    b = TraceBuilder(traj, salt)
    requests: dict[str, dict] = {}
    ids: set[str] = set()
    window: list[int] = []  # compactions before each access
    compactions = 0
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                o = json.loads(line)
            except ValueError:
                continue
            kind, x = o.get("type"), o.get("data") or {}
            if kind in ("assistant.message", "user.message") and not x.get("parentToolCallId"):
                text = x.get("content")
                b.stats["message_tokens"] += tokens(text) if isinstance(text, str) and text else 0
                for r in x.get("toolRequests") or []:
                    requests[r.get("toolCallId")] = r
            elif kind == "tool.execution_complete":
                r = requests.pop(x.get("toolCallId"), None)
                if x.get("parentToolCallId") or r is None:
                    for e in x.get("fileEdits") or []:
                        if isinstance(e, dict) and e.get("path"):
                            b.write(posixpath.normpath(e["path"]))
                    continue
                ids.add(x.get("toolCallId"))
                content = (x.get("result") or {}).get("content")
                if not isinstance(content, str):
                    content = json.dumps(content) if content is not None else ""
                b.stats["output_tokens"] += tokens(content)
                before = len(b.accesses)
                _call(b, r.get("name") or "", r.get("arguments"), content, bool(x.get("success")))
                window += [compactions] * (len(b.accesses) - before)
            elif kind == "session.compaction_complete" and x.get("success"):
                compactions += 1
    b.stats["compactions"] = compactions
    _observed(b.accesses, window, b.stats)
    return b.accesses, dict(b.stats), ids


def _observed(acc: list[Access], window: list[int], stats) -> None:
    """What the agent paid: re-reads in the same window as the last access, or after a compaction."""
    last: dict[str, int] = {}
    for t, a in enumerate(acc):
        if not a.first and not a.ref:
            where = "same_window" if window[last[a.obj]] == window[t] else "after_compaction"
            stats[f"reread_{where}"] += 1
            stats[f"reread_{where}_tokens"] += a.tokens
        last[a.obj] = t


def _call(b: TraceBuilder, name: str, args, content: str, ok: bool) -> None:
    if isinstance(args, str):
        args = {"input": args}
    if not isinstance(args, dict):
        args = {}
    request = json.dumps(args, sort_keys=True, default=str)
    path = posixpath.normpath(args["path"]) if isinstance(args.get("path"), str) and args["path"] else None
    if name == "view" and path and ok and content and not content.startswith(NOT_CONTENT):
        rng = args.get("view_range")
        lines = (int(rng[0]), int(rng[1])) if isinstance(rng, list) and len(rng) == 2 and all(isinstance(v, int) for v in rng) else None
        b.read_file(path, content, lines if lines and lines[1] > 0 else None, content)
        return
    if name == "edit" and path:
        b.use(path, "edit", text=args["old_str"] if isinstance(args.get("old_str"), str) else None)
        if ok and not content.startswith("No changes made"):
            b.write(path)
    elif name == "create" and path and ok:
        b.write(path)
    elif name == "apply_patch":
        for m in PATCH.finditer(str(args.get("input") or args.get("str") or "")):
            p = bash.resolve(m.group(2) or m.group(3), None, b.known())
            if m.group(1) == "Update":
                b.use(p, "edit")
            if ok:
                b.write(p)
    elif name == "bash" and isinstance(args.get("command"), str):
        body, code = _shell_result(content)
        eff = bash.effects(args["command"], None, b.known())
        # The tool reports success even when the command fails, so the exit code decides.
        # A failed simple command changed nothing. With no exit code, its writes stand.
        for kind, p in eff.applied(not eff.succeeded(code)):
            b.use(p, eff.why.get(p, "path")) if kind == "use" else b.write(p)
        for url in eff.urls:
            b.use_url(url)
        if eff.read and ok and eff.succeeded(code):
            b.read_file(eff.read, body)
        elif eff.url and ok and eff.succeeded(code):
            b.read_url(eff.url, args["command"], body)
        else:
            b.output(name, request, body)
        return
    elif isinstance(args.get("url"), str) and ok:
        rest = json.dumps({k: v for k, v in args.items() if k != "url"}, sort_keys=True, default=str)
        b.read_url(args["url"], f"{name}\0{rest}", content)
        return
    b.output(name, request, content)


def _shell_result(content: str) -> tuple[str, int | None]:
    """A bash result without the harness's last line, and the exit code that line gives."""
    m = TRAILER.search(content)
    if not m:
        return content, None
    return content[: m.start()], (int(m.group(1)) if m.group(1) else None)


def traces(root: str = ROOT, salt: str | None = None):
    """Yields (trajectory id, cluster, accesses, info) per session: most tool calls
    first, then by directory name. A skipped fork yields accesses None and info {"fork": 1}."""
    salt = salt or os.urandom(16).hex()
    found = []
    for d in sorted(os.listdir(root)):
        p = os.path.join(root, d, "events.jsonl")
        if os.path.isfile(p):
            traj = hashlib.sha256(f"{salt}\0{d}".encode()).hexdigest()[:12]
            acc, info, ids = session(p, traj, salt)
            found.append((len(ids), d, traj, acc, info, ids))
    # Sort by the directory name, not the salted id, so the order and the kept fork
    # are the same on every run.
    found.sort(key=lambda s: (-s[0], s[1]))
    kept: set[str] = set()
    for n, _, traj, acc, info, ids in found:
        if ids and len(ids & kept) * 2 >= len(ids):
            yield traj, traj, None, {"fork": 1}
            continue
        kept |= ids
        yield traj, traj, acc, info
