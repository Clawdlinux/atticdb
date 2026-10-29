"""A stand-in for `claude` and `codex` in tests. It never calls a model.

Installed under both names. It appends each call's argv to $STUB_LOG, then:

- `claude auth status`, `codex login status`: exit 0, or 1 with STUB_LOGGED_OUT=1.
- `claude mcp add|remove -s user zerodb ...`: edits `$CLAUDE_CONFIG_DIR/.claude.json`.
- `codex mcp get zerodb --json`: prints the `mcp_servers.zerodb` table of `$CODEX_HOME/config.toml`.
- `claude -p <prompt> ...`, `codex exec ... <prompt>`: an agent. With STUB_REPLAY=<file> it
  prints the file. Otherwise it acts on the prompt for real through the zerodb that setup
  wired in: it runs the SessionStart hook from the config, calls the zerodb CLI as the MCP
  entry would, edits calc.py, and prints a stream in the agent's own JSONL shape.
"""

import json
import os
import subprocess
import sys
import time
import tomllib

PROG = os.path.basename(sys.argv[0])
ACCOUNT_ENV = ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_BASE_URL",
               "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "OPENAI_API_KEY", "CODEX_API_KEY",
               "OPENAI_BASE_URL", "OPENAI_ORG_ID", "CLAUDE_CODE_USE_FOUNDRY")
ARGS = sys.argv[1:]


def log():
    path = os.environ.get("STUB_LOG")
    if path:
        with open(path, "a") as f:
            f.write(json.dumps({"prog": PROG, "argv": ARGS, "cwd": os.getcwd(),
                                "zerodb_home": bool(os.environ.get("ZERODB_HOME")),
                                "api_keys": sorted(k for k in os.environ
                                                   if k in ACCOUNT_ENV or k.startswith("ANTHROPIC_FOUNDRY_")),
                                "git_env": sorted(k for k in os.environ if k.startswith("GIT_")),
                                "outside_home": sorted(
                                    k for k in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "COPILOT_HOME")
                                    if not os.environ.get(k, os.environ["HOME"]).startswith(os.environ["HOME"]))})
                    + "\n")


def read_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def write_json(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(value, f, indent=2)


def claude_state():
    return os.path.join(os.environ["CLAUDE_CONFIG_DIR"], ".claude.json")


def claude_mcp():
    state = read_json(claude_state(), {})
    if ARGS[:5] == ["mcp", "add", "-s", "user", "zerodb"]:
        rest, env = ARGS[5:], {}
        while rest and rest[0] == "-e":
            key, _, value = rest[1].partition("=")
            env[key] = value
            rest = rest[2:]
        assert rest[0] == "--", rest
        entry = {"type": "stdio", "command": rest[1], "args": rest[2:], "env": env}
        state.setdefault("mcpServers", {})["zerodb"] = entry
    elif ARGS[:5] == ["mcp", "remove", "-s", "user", "zerodb"]:
        state.get("mcpServers", {}).pop("zerodb", None)
    else:
        sys.exit(2)
    write_json(claude_state(), state)


def codex_server():
    try:
        with open(os.path.join(os.environ["CODEX_HOME"], "config.toml"), "rb") as f:
            return tomllib.load(f).get("mcp_servers", {}).get("zerodb")
    except FileNotFoundError:
        return None


def hook_context(settings):
    for group in read_json(settings, {}).get("hooks", {}).get("SessionStart", []):
        for hook in group.get("hooks", []):
            if "zerodb" in hook.get("command", ""):
                stdin = json.dumps({"cwd": os.getcwd(), "hook_event_name": "SessionStart"})
                out = subprocess.run(["sh", "-c", hook["command"]], input=stdin, capture_output=True,
                                     text=True, timeout=15).stdout
                return json.loads(out or "{}").get("hookSpecificOutput", {}).get("additionalContext", "")
    return ""


def zerodb(server, *args):
    """Runs a zerodb shell verb as the MCP entry would: its binary, agent and env."""
    exe, agent = server["command"], server["args"][server["args"].index("--agent") + 1]
    env = dict(os.environ, **server.get("env", {}))
    out = subprocess.run([exe, *args, "--agent", agent], env=env, capture_output=True, text=True,
                         timeout=30)
    return out.stdout


def act(prompt, server, context):
    """(tool calls as (name, args), reply)."""
    if "run this repo's tests" in prompt:
        return [], ("Run `cargo nextest run`." if "nextest" in context else "Run `python -m pytest`.")
    if "drop Redis" in prompt:
        args = {"query": "Redis"}
        hits = zerodb(server, "search", "--query", "Redis")
        if "round trips" in hits:
            return [("memory_search", args)], "Its 2 extra round trips per append broke the 1 ms p99 budget."
        return [("memory_search", args)], "I found no record of why Redis was dropped."
    if prompt.startswith("Remember"):
        args = {"key": "repo/evalrepo/node-version", "content": "Always pin Node 22."}
        zerodb(server, "fact", "set", "--key", args["key"], "--content", args["content"])
        return [("fact_set", args)], "Saved: always pin Node 22 in this repo."
    if "add(a, b)" in prompt:
        with open("calc.py", "a") as f:
            f.write('\n\ndef add(a, b):\n    """a plus b."""\n    return a + b\n')
        args = {"content": "Added add(a, b) to calc.py; it returns a + b."}
        zerodb(server, "append", "--content", args["content"])
        return [("memory_append", args)], "Added `add(a, b)` to calc.py."
    return [], "I am a stub."


def claude_events(calls, reply):
    yield {"type": "system", "subtype": "init", "session_id": "s1", "tools": []}
    for i, (name, args) in enumerate(calls):
        yield {"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": f"toolu_{i}", "name": f"mcp__zerodb__{name}", "input": args}]}}
        yield {"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": f"toolu_{i}", "content": "ok"}]}}
    yield {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}}
    yield {"type": "result", "subtype": "success", "is_error": False, "duration_ms": 1200,
           "num_turns": len(calls) + 1, "result": reply,
           "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000,
                     "output_tokens": 20}}


def codex_events(calls, reply):
    yield {"type": "thread.started", "thread_id": "t1"}
    yield {"type": "turn.started"}
    for i, (name, args) in enumerate(calls):
        item = {"id": f"item_{i}", "type": "mcp_tool_call", "server": "zerodb", "tool": name,
                "arguments": args, "status": "in_progress"}
        yield {"type": "item.started", "item": item}
        yield {"type": "item.completed", "item": dict(item, status="completed", result={"content": []})}
    yield {"type": "item.completed", "item": {"id": "item_9", "type": "agent_message", "text": reply}}
    yield {"type": "turn.completed", "usage": {"input_tokens": 1110, "cached_input_tokens": 1000,
                                               "output_tokens": 20}}


def agent(prompt):
    if os.environ.get("STUB_EXIT"):
        sys.stderr.write(os.environ.get("STUB_STDERR", "") + "\n")
        sys.exit(int(os.environ["STUB_EXIT"]))
    if os.environ.get("STUB_SLEEP"):
        time.sleep(float(os.environ["STUB_SLEEP"]))
    replay = os.environ.get("STUB_REPLAY")
    if replay:
        with open(replay) as f:
            sys.stdout.write(f.read())
        return
    if PROG == "claude":
        server = read_json(claude_state(), {}).get("mcpServers", {}).get("zerodb")
        context = hook_context(os.path.join(os.environ["CLAUDE_CONFIG_DIR"], "settings.json"))
    else:
        server = codex_server()
        context = hook_context(os.path.join(os.environ["CODEX_HOME"], "hooks.json"))
    if server is None:
        sys.exit(3)
    calls, reply = act(prompt, server, context)
    events = claude_events if PROG == "claude" else codex_events
    for ev in events(calls, reply):
        print(json.dumps(ev))
    sys.stdout.flush()
    if os.environ.get("STUB_EXIT_AFTER"):  # a full stream, then a failing exit
        sys.exit(int(os.environ["STUB_EXIT_AFTER"]))


def main():
    log()
    if ARGS[:2] in (["auth", "status"], ["login", "status"]):
        sys.exit(1 if os.environ.get("STUB_LOGGED_OUT") else 0)
    if PROG == "claude" and ARGS[:1] == ["mcp"]:
        return claude_mcp()
    if PROG == "codex" and ARGS == ["mcp", "get", "zerodb", "--json"]:
        server = codex_server()
        if server is None:
            sys.exit(1)
        transport = {"type": "stdio", "command": server["command"], "args": server.get("args", []),
                     "env": server.get("env")}
        print(json.dumps({"name": "zerodb", "enabled": True, "transport": transport}))
        return
    if PROG == "claude" and ARGS[:1] == ["-p"]:
        return agent(ARGS[1])
    if PROG == "codex" and ARGS[:1] == ["exec"]:
        return agent(ARGS[-1])
    sys.exit(2)


main()
