"""OpenHands 0.54 trajectories (nebius/SWE-rebench-openhands-trajectories, CC-BY-4.0) as traces.

Messages are system, user (the task), then assistant messages with 1 tool call each,
and the tool messages that answer them. Files come from `str_replace_editor` calls
and from `execute_bash` commands. README.md has the rules.
"""

from __future__ import annotations

import json
import posixpath
import re

from . import bash
from .trace import Access, TraceBuilder, tokens

CAT_N = "Here's the result of running `cat -n` on "
NUMBERED = re.compile(r"^\s*(\d+)\t(.*)$")
FOOTER = re.compile(
    r"^\[(?:The command completed with exit code -?\d+\.|Current working directory: .*|"
    r"Python interpreter: .*|Command finished with exit code -?\d+)\]$"
)
CWD = re.compile(r"^\[Current working directory: (.*)\]$", re.M)
EXIT = re.compile(r"^\[The command completed with exit code (-?\d+)\.\]$", re.M)
MAX_ITER = re.compile(r"maximum iteration\. Current iteration: (\d+)")
# OpenHands sends this as a user message when the agent replies without a tool call.
AUTO_CONTINUE = "Please continue working on the task"


def numbered(text: str):
    """(body, (first, last) line, text) of the `cat -n` lines in a tool output. Other
    lines, such as the editor's header and footer, are dropped."""
    body, nums, plain = [], [], []
    for line in text.split("\n"):
        m = NUMBERED.match(line)
        if m:
            body.append(line)
            nums.append(int(m.group(1)))
            plain.append(m.group(2))
    return "\n".join(body), ((min(nums), max(nums)) if nums else None), "\n".join(plain)


def condensed(row: dict) -> bool:
    """Signs that history was condensed: a second user message that is not the
    auto-continue prompt, a tool result with no call, or a run that hit the iteration
    cap with fewer assistant messages than iterations."""
    msgs = row["trajectory"]
    users = sum(1 for m in msgs if m["role"] == "user" and not (m.get("content") or "").startswith(AUTO_CONTINUE))
    called = {c["id"] for m in msgs if m["role"] == "assistant" for c in (m.get("tool_calls") or [])}
    orphans = any(m["role"] == "tool" and m.get("tool_call_id") not in called for m in msgs)
    cap = MAX_ITER.search(row.get("exit_status") or "")
    short = cap is not None and sum(1 for m in msgs if m["role"] == "assistant") < int(cap.group(1))
    return users > 1 or orphans or short


def trajectory(row: dict, traj: str, salt: str = "ctxcache-v1", keep_text: bool = False) -> tuple[list[Access], dict]:
    b = TraceBuilder(traj, salt, keep_text)
    msgs = row["trajectory"]
    results = {m.get("tool_call_id"): m for m in msgs if m["role"] == "tool"}
    cwd: str | None = None
    for m in msgs:
        if m["role"] == "user":
            b.stats["message_tokens"] += tokens(m.get("content") or "")
        if m["role"] != "assistant":
            continue
        b.stats["message_tokens"] += tokens(m.get("content") or "")
        for call in m.get("tool_calls") or []:
            fn = call.get("function") or {}
            raw = fn.get("arguments") or "{}"
            b.action = f"{fn.get('name')}: {raw}"
            b.stats["message_tokens"] += tokens(raw)
            try:
                args = json.loads(raw)
            except ValueError:
                args = {}
            res = results.get(call.get("id"))
            if res is None:
                b.stats["actions_without_output"] += 1
                if fn.get("name") == "str_replace_editor" and isinstance(args, dict):
                    _edit_reference(b, args)  # the edit still used its view. With no result, nothing was written.
                continue
            content = res.get("content") or ""
            b.stats["output_tokens"] += tokens(content)
            if fn.get("name") == "str_replace_editor" and isinstance(args, dict):
                _editor(b, args, content)
            elif fn.get("name") == "execute_bash" and isinstance(args, dict):
                cwd = _shell(b, args, content, cwd)
            else:
                b.output(fn.get("name") or "", raw, content)
    info = {"cluster": row.get("instance_id"), "condensed": int(condensed(row))}
    if keep_text:
        info["notes"] = b.notes
    return b.accesses, {**info, **b.stats}


def _edit_reference(b: TraceBuilder, args: dict) -> None:
    """str_replace names its view by old text, and insert by line. Other commands name none."""
    cmd = args.get("command")
    path = posixpath.normpath(args.get("path") or "")
    if cmd == "insert":
        line = args.get("insert_line")
        n = int(line) if str(line or "").isdigit() else None
        b.use(path, "line", line=n, end=n)
    elif cmd == "str_replace":
        b.use(path, "edit", text=args.get("old_str") if isinstance(args.get("old_str"), str) else None)


def _editor(b: TraceBuilder, args: dict, content: str) -> None:
    cmd = args.get("command")
    path = posixpath.normpath(args.get("path") or "")
    request = json.dumps(args, sort_keys=True)
    if cmd == "view" and content.startswith(CAT_N):
        body, lines, plain = numbered(content)
        b.read_file(path, body, lines, plain)
        return
    if cmd == "create" and content.startswith("File created successfully"):
        b.write(path)
    elif cmd in ("str_replace", "insert"):
        _edit_reference(b, args)
        if "has been edited" in content.split("\n", 1)[0]:
            b.write(path)
            body, lines, plain = numbered(content)
            b.read_file(path, body, lines, plain)
            return
    elif cmd == "undo_edit" and "undone successfully" in content.split("\n", 1)[0]:
        b.write(path)
        body, lines, plain = numbered(content)
        b.read_file(path, body, lines, plain)
        return
    b.output(f"editor:{cmd}", request, content)


def _shell(b: TraceBuilder, args: dict, content: str, cwd: str | None) -> str | None:
    command = str(args.get("command") or "")
    body = "\n".join(line for line in content.split("\n") if not FOOTER.match(line)).strip("\n")
    after = CWD.findall(content)
    exit_code = EXIT.search(content)
    if str(args.get("is_input")).lower() == "true":
        b.output("bash-input", command, body)
        return after[-1] if after else cwd
    eff = bash.effects(command, cwd, b.known())
    code = int(exit_code.group(1)) if exit_code else None
    # A failed simple command changed nothing. With no exit code, its writes stand.
    for kind, path in eff.applied(not eff.succeeded(code)):
        b.use(path, eff.why.get(path, "path")) if kind == "use" else b.write(path)
    for url in eff.urls:
        b.use_url(url)
    ok = eff.succeeded(code)
    if eff.read and ok:
        # `cat -n` output is the editor's view of the same lines, but only if every line
        # is a `cat -n` line. A data file with a line such as "1<TAB>alpha" stays whole.
        nbody, lines, plain = numbered(body)
        if lines and nbody == body:
            b.read_file(eff.read, body, lines, plain)
        else:
            b.read_file(eff.read, body, None, body)
    elif eff.url and ok:
        b.read_url(eff.url, command, body)
    else:
        b.output("bash", f"{cwd}\0{command}", body)
    return after[-1] if after else eff.cwd
