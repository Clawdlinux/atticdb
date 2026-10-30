"""Defensive parsers for the 2 agents' JSONL streams.

A line that is not a JSON object, an event whose `type` is not a string, or a known event
whose fields have the wrong shape, counts as unparsed. An event of a type the parser doesn't
know counts as unknown. Neither stops the parse. `parse_ok` holds when the final reply was
found and no line was unparsed.

Tool calls keep the bare tool name of the atticdb server: `mcp__atticdb__fact_set` in
Claude Code and `{"server": "atticdb", "tool": "fact_set"}` in Codex both give `fact_set`.

Token counts mean the same in both: `input_tokens` is every prompt token, cache reads and
writes included, and `cache_read_tokens` is the part read from the cache.
"""

from __future__ import annotations

import json
from collections import Counter
from dataclasses import dataclass, field

SERVER = "atticdb"
CLAUDE_PREFIX = f"mcp__{SERVER}__"


@dataclass
class Call:
    name: str  # the bare AtticDB tool name
    args: dict | None  # None when the arguments were not a JSON object


@dataclass
class Outcome:
    reply: str = ""
    calls: list[Call] = field(default_factory=list)
    input_tokens: int | None = None
    output_tokens: int | None = None
    cache_read_tokens: int | None = None
    turns: int | None = None
    final: bool = False  # the final reply was found
    is_error: bool = False  # the agent reported an error
    unparsed: int = 0
    unknown: int = 0

    @property
    def parse_ok(self) -> bool:
        return self.final and self.unparsed == 0

    def counts(self) -> dict[str, int]:
        return dict(sorted(Counter(c.name for c in self.calls).items()))


def _lines(stream: bytes | str):
    """Yields each non-empty line as an object, or None for one that is not a JSON object."""
    text = stream.decode("utf-8", "replace") if isinstance(stream, bytes) else stream
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            yield None
            continue
        yield obj if isinstance(obj, dict) else None


def _int(value) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def _sum(*values) -> int | None:
    known = [v for v in values if v is not None]
    return sum(known) if known else None


def _args(value) -> dict | None:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return None
        return value if isinstance(value, dict) else None
    return None


# Claude Code: `claude -p ... --output-format stream-json --verbose`.
CLAUDE_KNOWN = {"system", "user", "stream_event", "rate_limit_event"}


def parse_claude(stream: bytes | str) -> Outcome:
    out = Outcome()
    seen_ids: set[str] = set()
    last_text = ""
    for ev in _lines(stream):
        if ev is None:
            out.unparsed += 1
            continue
        kind = ev.get("type")
        if kind is not None and not isinstance(kind, str):
            out.unparsed += 1
            continue
        if kind == "assistant":
            msg = ev.get("message")
            content = msg.get("content") if isinstance(msg, dict) else None
            if not isinstance(content, list):
                out.unparsed += 1
                continue
            texts = []
            for block in content:
                if not isinstance(block, dict):
                    out.unparsed += 1
                    continue
                if block.get("type") == "text" and isinstance(block.get("text"), str):
                    texts.append(block["text"])
                elif block.get("type") == "tool_use":
                    name, bid = block.get("name"), block.get("id")
                    if not isinstance(name, str):
                        out.unparsed += 1
                        continue
                    if isinstance(bid, str):
                        if bid in seen_ids:
                            continue
                        seen_ids.add(bid)
                    if name.startswith(CLAUDE_PREFIX):
                        out.calls.append(Call(name[len(CLAUDE_PREFIX):], _args(block.get("input"))))
            if texts:
                last_text = "\n".join(texts)
        elif kind == "result":
            usage = ev.get("usage")
            if usage is not None and not isinstance(usage, dict):
                out.unparsed += 1
                usage = None
            usage = usage or {}
            read = _int(usage.get("cache_read_input_tokens"))
            out.input_tokens = _sum(_int(usage.get("input_tokens")),
                                    _int(usage.get("cache_creation_input_tokens")), read)
            out.output_tokens = _int(usage.get("output_tokens"))
            out.cache_read_tokens = read
            out.turns = _int(ev.get("num_turns"))
            out.is_error = bool(ev.get("is_error")) or ev.get("subtype") not in (None, "success")
            if isinstance(ev.get("result"), str):
                out.reply = ev["result"]
                out.final = True
            elif not out.is_error:
                out.unparsed += 1
        elif kind in CLAUDE_KNOWN:
            pass
        else:
            out.unknown += 1
    if not out.final:
        out.reply = last_text
    return out


# Codex: `codex exec --json`. The current events are `thread.started`, `turn.started`,
# `item.started` / `item.updated` / `item.completed` with an `item`, `turn.completed` with
# `usage`, `turn.failed` and `error`. Older builds printed `{"id", "msg": {"type": ...}}`
# lines; those are read too.
CODEX_KNOWN = {"thread.started", "turn.started"}
CODEX_ITEM_EVENTS = {"item.started", "item.updated", "item.completed"}
CODEX_LEGACY_KNOWN = {"task_started", "agent_reasoning", "agent_reasoning_delta", "agent_message_delta",
                      "exec_command_begin", "exec_command_end", "patch_apply_begin", "patch_apply_end",
                      "mcp_tool_call_begin", "background_event", "session_configured"}


def _codex_item(item) -> tuple[str | None, dict]:
    """The item's type and fields. Older items nest fields under `details` with `item_type`."""
    if not isinstance(item, dict):
        return None, {}
    fields = dict(item)
    if isinstance(item.get("details"), dict):
        fields.update(item["details"])
    kind = fields.get("type") or fields.get("item_type")
    return (kind if isinstance(kind, str) else None), fields


def parse_codex(stream: bytes | str) -> Outcome:
    out = Outcome()
    calls: dict[str, Call] = {}  # by item id, so a started and completed call counts once
    order: list[str] = []
    anon = 0
    usage = {"input": None, "cached": None, "output": None}
    turns = 0

    def add_usage(u: dict) -> None:
        for key, src in (("input", "input_tokens"), ("cached", "cached_input_tokens"),
                         ("output", "output_tokens")):
            v = _int(u.get(src))
            if v is not None:
                usage[key] = (usage[key] or 0) + v

    def add_call(cid, server, tool, args) -> bool:
        nonlocal anon
        if not isinstance(server, str) or not isinstance(tool, str):
            return False
        if server != SERVER:
            return True
        if not isinstance(cid, str):
            anon += 1
            cid = f"anon-{anon}"
        if cid not in calls:
            order.append(cid)
        calls[cid] = Call(tool, _args(args))
        return True

    for ev in _lines(stream):
        if ev is None:
            out.unparsed += 1
            continue
        kind = ev.get("type")
        if kind is not None and not isinstance(kind, str):
            out.unparsed += 1
            continue
        if kind in CODEX_ITEM_EVENTS:
            item_kind, item = _codex_item(ev.get("item"))
            if item_kind is None:
                out.unparsed += 1
            elif item_kind == "mcp_tool_call":
                if not add_call(item.get("id"), item.get("server"), item.get("tool"), item.get("arguments")):
                    out.unparsed += 1
            elif item_kind in ("agent_message", "assistant_message") and kind == "item.completed":
                if isinstance(item.get("text"), str):
                    out.reply, out.final = item["text"], True
                else:
                    out.unparsed += 1
            # reasoning, command_execution, file_change, web_search, todo_list: not needed
        elif kind == "turn.completed":
            turns += 1
            if isinstance(ev.get("usage"), dict):
                add_usage(ev["usage"])
            elif ev.get("usage") is not None:
                out.unparsed += 1
        elif kind in ("turn.failed", "error"):
            out.is_error = True
        elif kind in CODEX_KNOWN:
            pass
        elif kind is None and isinstance(ev.get("msg"), dict):
            msg = ev["msg"]
            mkind = msg.get("type")
            if not isinstance(mkind, str):
                out.unparsed += 1
            elif mkind == "agent_message" and isinstance(msg.get("message"), str):
                out.reply, out.final = msg["message"], True
            elif mkind == "task_complete":
                if isinstance(msg.get("last_agent_message"), str):
                    out.reply, out.final = msg["last_agent_message"], True
                turns += 1
            elif mkind == "mcp_tool_call_end":
                inv = msg.get("invocation")
                if not isinstance(inv, dict) or not add_call(msg.get("call_id"), inv.get("server"),
                                                             inv.get("tool"), inv.get("arguments")):
                    out.unparsed += 1
            elif mkind == "token_count":
                info = msg.get("info")
                total = info.get("total_token_usage") if isinstance(info, dict) else None
                if isinstance(total, dict):  # a running total: keep the last
                    usage.update({"input": None, "cached": None, "output": None})
                    add_usage(total)
            elif mkind == "error":
                out.is_error = True
            elif mkind in CODEX_LEGACY_KNOWN:
                pass
            else:
                out.unknown += 1
        else:
            out.unknown += 1
    out.calls = [calls[cid] for cid in order]
    out.input_tokens, out.output_tokens = usage["input"], usage["output"]
    out.cache_read_tokens = usage["cached"]
    out.turns = turns if turns else None
    return out


PARSERS = {"claude-code": parse_claude, "codex": parse_codex}
