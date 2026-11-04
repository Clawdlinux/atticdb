import os

from research.setup_eval import parse

from .helpers import FIXTURES


def read(name: str) -> bytes:
    with open(os.path.join(FIXTURES, name), "rb") as f:
        return f.read()


def test_claude_stream_fixture():
    out = parse.parse_claude(read("claude-stream.jsonl"))
    # The repeated toolu_02 counts once. Read is not an AtticDB tool.
    assert [c.name for c in out.calls] == ["memory_search", "fact_set"]
    assert out.calls[1].args == {"key": "repo/evalrepo/node-version", "content": "Always pin Node 22."}
    assert out.counts() == {"fact_set": 1, "memory_search": 1}
    assert out.reply == "Saved: pin Node 22."
    assert (out.input_tokens, out.output_tokens, out.cache_read_tokens) == (12 + 3400 + 15000, 210, 15000)
    assert out.turns == 4
    assert out.final and not out.is_error
    assert (out.unparsed, out.unknown) == (2, 1), "a text line and a JSON array; 1 future event type"
    assert not out.parse_ok, "garbage lines count against parse_ok"


def test_codex_events_fixture():
    out = parse.parse_codex(read("codex-events.jsonl"))
    # Started and completed item_1 count once; the other server's call is left out; string
    # arguments are decoded.
    assert [c.name for c in out.calls] == ["memory_search", "memory_append"]
    assert out.calls[1].args == {"content": "Added add(a, b) to calc.py."}
    assert out.reply.startswith("We dropped Redis")
    assert (out.input_tokens, out.output_tokens, out.cache_read_tokens) == (24000, 350, 20000)
    assert out.turns == 1
    assert (out.unparsed, out.unknown) == (2, 1)
    assert out.final and not out.parse_ok


def test_clean_streams_parse_ok():
    claude = b'{"type":"result","subtype":"success","is_error":false,"result":"hi","num_turns":1,"usage":{}}\n'
    assert parse.parse_claude(claude).parse_ok
    codex = b'{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"hi"}}\n'
    out = parse.parse_codex(codex)
    assert out.parse_ok and out.turns is None and out.input_tokens is None


def test_garbage_never_crashes():
    junk = [b"", b"\x00\xff\xfe", b"null\n42\n\"s\"\n", b'{"type": "assistant", "message": "x"}\n',
            b'{"type": "assistant", "message": {"content": [1, {"type": "tool_use", "name": 3}]}}\n',
            b'{"type": "result", "usage": [1], "result": 5}\n',
            b'{"type": "item.completed", "item": {"type": "mcp_tool_call", "server": 1}}\n',
            b'{"type": "turn.completed", "usage": "lots"}\n', b'{"msg": {"type": "mcp_tool_call_end"}}\n',
            b'{"msg": {"type": "token_count", "info": null}}\n']
    for blob in junk:
        for parser in (parse.parse_claude, parse.parse_codex):
            out = parser(blob)
            assert not out.parse_ok
            assert isinstance(out.calls, list)


def test_claude_without_result_keeps_last_text():
    stream = b'{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}\n'
    out = parse.parse_claude(stream)
    assert out.reply == "partial" and not out.final and not out.parse_ok


def test_claude_error_result():
    stream = b'{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":9}\n'
    out = parse.parse_claude(stream)
    assert out.is_error and out.turns == 9 and not out.final


def test_codex_legacy_events():
    lines = [
        '{"id":"0","msg":{"type":"mcp_tool_call_end","call_id":"c1","invocation":{"server":"atticdb",'
        '"tool":"fact_set","arguments":{"key":"repo/evalrepo/k","content":"x"}}}}',
        '{"id":"0","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5,'
        '"cached_input_tokens":2,"output_tokens":1}}}}',
        '{"id":"0","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":9,'
        '"cached_input_tokens":4,"output_tokens":3}}}}',
        '{"id":"0","msg":{"type":"task_complete","last_agent_message":"done"}}',
    ]
    out = parse.parse_codex("\n".join(lines))
    assert out.counts() == {"fact_set": 1}
    assert (out.input_tokens, out.cache_read_tokens, out.output_tokens) == (9, 4, 3), "the last running total"
    assert out.reply == "done" and out.parse_ok and out.turns == 1


def test_non_string_types_are_unparsed_not_raised():
    for kind in ("[1]", "{}", "3", "true"):
        line = '{"type": %s}' % kind
        for parser in (parse.parse_claude, parse.parse_codex):
            out = parser(line + "\n" + line)
            assert out.unparsed == 2 and out.unknown == 0, (parser.__name__, kind)
        out = parse.parse_codex('{"msg": {"type": %s}}' % kind)
        assert out.unparsed == 1, kind
    # A bad line beside a good stream costs parse_ok only.
    good = '{"type":"result","subtype":"success","is_error":false,"result":"hi","num_turns":1}'
    out = parse.parse_claude('{"type": ["x"]}\n' + good)
    assert out.final and out.reply == "hi" and out.unparsed == 1 and not out.parse_ok


def test_copilot_events_fixture():
    """Captured from a real `copilot -p ... --output-format json` 1.0.105 run, trimmed: the
    streamed deltas cut to 1 each, reasoning and opaque ids dropped, paths made neutral."""
    out = parse.parse_copilot(read("copilot-events.jsonl"))
    # The atticdb call shows in the tool request and its execution, and counts once. bash is not AtticDB's.
    assert [c.name for c in out.calls] == ["fact_set"]
    assert out.calls[0].args == {"key": "repo/evalrepo/probe", "content": "probe ok"}
    assert out.reply == "ZEBRA-4417\n\nSaved `repo/evalrepo/probe` as `probe ok`. `calc.py` defines `add`."
    assert out.final and not out.is_error and out.parse_ok
    assert (out.input_tokens, out.output_tokens, out.cache_read_tokens) == (353 + 15438 + 31043, 269, 31043)
    assert out.turns == 3 and out.premium_requests == 1
    assert (out.unparsed, out.unknown) == (0, 0)


def test_copilot_other_servers_bad_lines_and_errors():
    import json
    lines = [
        {"type": "assistant.message", "data": {"content": "", "toolRequests": [
            {"toolCallId": "a", "name": "github-mcp-server-search", "mcpServerName": "github-mcp-server",
             "mcpToolName": "search", "arguments": {}},
            {"toolCallId": "b", "name": "atticdb-memory_search", "arguments": '{"query": "why"}'}]}},
        {"type": "tool.execution_start", "data": {"toolCallId": "b", "toolName": "atticdb-memory_search",
                                                  "mcpServerName": "atticdb", "mcpToolName": "memory_search"}},
        {"type": "assistant.message", "data": {"content": "partial answer", "toolRequests": []}},
        {"type": "assistant.message", "data": {"content": 3}},
        {"type": "future.kind", "data": {}},
        {"type": "session.error", "data": {"message": "rate limited"}},
        {"type": "result", "exitCode": 1, "usage": {"premiumRequests": 0}},
    ]
    text = "\n".join(json.dumps(x) for x in lines) + "\nnot json\n"
    out = parse.parse_copilot(text)
    assert [c.name for c in out.calls] == ["memory_search"] and out.calls[0].args == {"query": "why"}
    assert out.is_error and not out.final and out.reply == "partial answer"
    assert (out.unparsed, out.unknown) == (2, 1) and out.premium_requests == 0
    assert out.input_tokens is None and out.turns is None
    # A non-zero exit alone is an error, too.
    assert parse.parse_copilot('{"type":"result","exitCode":2}\n').is_error
    assert parse.PARSERS["copilot"] is parse.parse_copilot
