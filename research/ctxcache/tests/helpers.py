"""Shared builders: traces from compact specs, and rows shaped like the real data."""

from __future__ import annotations

import json
import os
import random

from research.ctxcache.trace import Access


def trace(*spec) -> list[Access]:
    """Accesses from (name, tokens) or (name, tokens, "ref") tuples. First flags follow."""
    seen, out = set(), []
    for item in spec:
        name, size, ref = item[0], item[1], len(item) > 2 and item[2] == "ref"
        out.append(Access(name, size, "file", name not in seen, ref))
        seen.add(name)
    return out


def random_trace(rng: random.Random) -> tuple[list[Access], int]:
    """At most 12 accesses over at most 6 objects, with a budget that some objects exceed."""
    sizes = {f"o{k}": rng.randint(1, 9) for k in range(rng.randint(1, 6))}
    seen, acc = set(), []
    for _ in range(rng.randint(1, 12)):
        name = rng.choice(sorted(sizes))
        first = name not in seen
        seen.add(name)
        acc.append(Access(name, sizes[name], "file", first, not first and rng.random() < 0.3))
    return acc, rng.randint(1, 18)


def shape(acc: list[Access]) -> list[str]:
    """Each access as letter + "*" if first + "r" if a reference + ":" + kind, in first-seen order."""
    letters: dict[str, str] = {}
    out = []
    for a in acc:
        letter = letters.setdefault(a.obj, "ABCDEFGHIJKLMNOPQRSTUVWXYZ"[len(letters)])
        out.append(f"{letter}{'*' if a.first else ''}{'r' if a.ref else ''}:{a.kind}")
    return out


# SWE-agent rows: an ai step holds the action in a fenced block; the next user step is
# the observation, with the footer SWE-agent prints.
def swe_obs(body: str, open_file: str = "n/a", cwd: str = "/repo") -> str:
    return f"{body}\n\n(Open file: {open_file})\n(Current directory: {cwd})\nbash-$"


def swe_window(path: str, total: int, first: int, last: int, tag: str = "", edited: bool = False) -> str:
    lines = [f"[File: {path} ({total} lines total)]"]
    if first > 1:
        lines.append(f"({first - 1} more lines above)")
    lines += [f"{i}:    value_{i} = {i}{tag}" for i in range(first, last + 1)]
    if last < total:
        lines.append(f"({total - last} more lines below)")
    if edited:
        lines.append("File updated. Please review the changes and make sure they are correct "
                     "(correct indentation, no duplicate lines, etc). Edit the file again if necessary.")
    return "\n".join(lines)


def swe_row(*steps) -> dict:
    """steps: (action, observation or None)."""
    traj = [{"role": "system", "system_prompt": "SETTING: ...", "text": None},
            {"role": "user", "text": "We're currently solving the following issue..."}]
    for act, obs in steps:
        traj.append({"role": "ai", "text": f"DISCUSSION\nNext step.\n\n```\n{act}\n```\n"})
        if obs is not None:
            traj.append({"role": "user", "text": obs})
    return {"instance_id": "owner__repo-1", "model_name": "swe-agent-llama-70b", "trajectory": traj}


# OpenHands rows: assistant messages with 1 tool call each, answered by tool messages.
def oh_cat(path: str, lines: list[str], first: int = 1, edited: bool = False) -> str:
    head = (f"The file {path} has been edited. Here's the result of running `cat -n` on a snippet of {path}:"
            if edited else f"Here's the result of running `cat -n` on {path}:")
    body = "\n".join(f"{i:6}\t{text}" for i, text in enumerate(lines, first))
    tail = "\nReview the changes and make sure they are as expected. Edit the file again if necessary." if edited else ""
    return f"{head}\n{body}{tail}"


def oh_bash(output: str, cwd: str = "/repo", code: int = 0) -> str:
    return (f"{output}\n[The command completed with exit code {code}.]\n[Current working directory: {cwd}]\n"
            f"[Python interpreter: /opt/conda/envs/testbed/bin/python]\n[Command finished with exit code {code}]")


def oh_row(*calls, extra_user: str | None = None, exit_status: str = "submit") -> dict:
    """calls: (tool name, arguments dict, result content or None). `extra_user` is a user
    message after the first call."""
    msgs = [{"role": "system", "content": "You are OpenHands agent..."},
            {"role": "user", "content": "<uploaded_files>/repo</uploaded_files> Fix the issue."}]
    for k, (name, args, content) in enumerate(calls):
        cid = f"toolu_{k:02d}"
        msgs.append({"role": "assistant", "content": "", "tool_calls": [
            {"id": cid, "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]})
        if content is not None:
            msgs.append({"role": "tool", "tool_call_id": cid, "name": name, "content": content})
        if extra_user and k == 0:
            msgs.append({"role": "user", "content": extra_user})
    return {"instance_id": "owner__repo-2", "exit_status": exit_status, "trajectory": msgs}


# Copilot CLI sessions: events.jsonl lines.
def copilot_call(cid: str, name: str, args, content, parent: str | None = None, edits=None) -> list[dict]:
    msg = {"type": "assistant.message",
           "data": {"content": "", "toolRequests": [{"toolCallId": cid, "name": name, "arguments": args}]}}
    done = {"type": "tool.execution_complete",
            "data": {"toolCallId": cid, "success": True, "result": {"content": content}}}
    if parent:
        msg["data"]["parentToolCallId"] = done["data"]["parentToolCallId"] = parent
    if edits:
        done["data"]["fileEdits"] = [{"path": p, "kind": "edit"} for p in edits]
    return [msg, done]


def copilot_session(root: str, name: str, events: list[dict]) -> None:
    os.makedirs(os.path.join(root, name))
    with open(os.path.join(root, name, "events.jsonl"), "w") as f:
        for e in events:
            f.write(json.dumps(e) + "\n")
