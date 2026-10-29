"""SWE-agent trajectories (nebius/SWE-agent-trajectories, CC-BY-4.0) as traces.

Each `ai` step holds 1 action: the last top-level fenced block, as SWE-agent's own
parser reads it. The next `user` step is its observation. The observation ends with
a footer that names the open file and the working directory. README.md has the rules.
"""

from __future__ import annotations

import re

from . import bash
from .trace import Access, TraceBuilder, tokens

FENCE = re.compile(r"^```(\S*)\s*\n|^```\s*$", re.M)
FOOTER = re.compile(r"\n*\(Open file: (?P<open>[^\n]*)\)\n\(Current directory: (?P<cwd>[^\n]*)\)\s*(?:bash-\$)?\s*$")
HEADER = re.compile(r"^\[File: (?P<path>.+) \(\d+ lines total\)\]$", re.M)
WINDOW_LINE = re.compile(r"^(\d+):(.*)$")
MORE = re.compile(r"^\(\d+ more lines (?:above|below)\)$")
SEARCH_FILE = re.compile(r'^(?:Found \d+ matches|No matches found) for ".*" in (?P<path>/[^\n]*?):?$', re.M)
EDIT = re.compile(r"^edit\s+(\d+)\s*:\s*(\d+)")
WINDOW_COMMANDS = ("open", "goto", "scroll_down", "scroll_up", "create")
OUTPUT_COMMANDS = ("search_dir", "find_file", "submit")
# A shell read failed when its first line says so, as in "cat: x: No such file or directory".
SHELL_ERRORS = ("No such file or directory", "Is a directory")


def action(text: str) -> str | None:
    """The last top-level fenced block, the same block SWE-agent runs."""
    stack, last = [], None
    for m in FENCE.finditer(text):
        if stack and not m.group(1):
            start = stack.pop()
            if not stack:
                last = (start, m)
        elif m.group(1) is not None:
            stack.append(m)
    return None if last is None else text[last[0].end() : last[1].start()].strip()


def window(text: str):
    """(path, body, (first, last) line, text) of the 100-line file window, or None."""
    m = HEADER.search(text)
    if not m:
        return None
    kept, nums, plain = [m.group(0)], [], []
    for line in text[m.end() :].split("\n")[1:]:
        w = WINDOW_LINE.match(line)
        if w:
            kept.append(line)
            nums.append(int(w.group(1)))
            plain.append(w.group(2))
        elif MORE.match(line):
            kept.append(line)
        else:
            break
    lines = (min(nums), max(nums)) if nums else None
    return m.group("path"), "\n".join(kept), lines, "\n".join(plain)


def trajectory(row: dict, traj: str, salt: str = "ctxcache-v1", keep_text: bool = False) -> tuple[list[Access], dict]:
    b = TraceBuilder(traj, salt, keep_text)
    steps = row["trajectory"]
    open_file: str | None = None
    cwd: str | None = None
    task_seen = False
    for i, step in enumerate(steps):
        text = step.get("text") or ""
        if step["role"] == "user" and not task_seen:
            task_seen = True
            b.stats["message_tokens"] += tokens(text)
        if step["role"] != "ai":
            continue
        b.stats["message_tokens"] += tokens(text)
        cmd = action(text) or ""
        b.action = cmd
        nxt = steps[i + 1] if i + 1 < len(steps) else None
        if nxt is None or nxt["role"] != "user":
            # The run ended before an observation. The action still used its target.
            m = EDIT.match(cmd)
            if m and open_file:
                b.use(open_file, "line", line=int(m.group(1)), end=int(m.group(2)))
            b.stats["actions_without_output"] += 1
            continue
        obs = nxt.get("text") or ""
        b.stats["output_tokens"] += tokens(obs)
        foot = FOOTER.search(obs)
        body = obs[: foot.start()] if foot else obs
        _step(b, cmd, body, open_file, cwd)
        if foot:
            open_file = None if foot.group("open") in ("n/a", "") else foot.group("open")
            cwd = foot.group("cwd") or None
    info = {"cluster": row.get("instance_id"), "model": row.get("model_name")}
    if keep_text:
        info["notes"] = b.notes
    return b.accesses, {**info, **b.stats}


def _step(b: TraceBuilder, cmd: str, body: str, open_file: str | None, cwd: str | None) -> None:
    name = cmd.split(None, 1)[0] if cmd else ""
    if name in WINDOW_COMMANDS:
        win = window(body)
        if win is None:
            b.output(name, cmd, body)
            return
        path, shown, lines, plain = win
        if name == "create" and not body.lstrip().startswith("Error:"):
            b.write(path)
        b.read_file(path, shown, lines, plain)
    elif name == "edit":
        m = EDIT.match(cmd)
        if open_file:
            b.use(open_file, "line", line=int(m.group(1)) if m else None, end=int(m.group(2)) if m else None)
        win = window(body)
        if "File updated." in body and win is not None:
            b.write(win[0])
            b.read_file(win[0], win[1], win[2], win[3])
        else:
            b.output("edit", cmd, body)
    elif name == "search_file":
        m = SEARCH_FILE.search(body)
        if m:
            b.read_file(m.group("path"), body)
        else:
            b.output(name, cmd, body)
    elif name in OUTPUT_COMMANDS or not cmd:
        b.output(name or "invalid", cmd, body)
    else:
        _shell(b, cmd, body, cwd)


def _shell(b: TraceBuilder, cmd: str, body: str, cwd: str | None) -> None:
    eff = bash.effects(cmd, cwd, b.known())
    for kind, path in eff.events:
        b.use(path, eff.why.get(path, "path")) if kind == "use" else b.write(path)
    for url in eff.urls:
        b.use_url(url)
    if eff.read and not any(e in body.split("\n", 1)[0] for e in SHELL_ERRORS):
        b.read_file(eff.read, body)
    elif eff.url:
        b.read_url(eff.url, cmd, body)
    else:
        b.output("bash", f"{cwd}\0{cmd}", body)
