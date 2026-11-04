"""A stand-in for `claude` and `codex` in handoff tests. It never calls a model.

A copy of research/setup_eval/tests/stub_agent.py, with the agent part changed for handoffs.
Installed under both names. It appends each call's argv to $STUB_LOG, then:

- `claude auth status`, `codex login status`: exit 0, or 1 with STUB_LOGGED_OUT=1.
- `claude mcp add|remove -s user atticdb ...`: edits `$CLAUDE_CONFIG_DIR/.claude.json`.
- `codex mcp get atticdb --json`: prints the `mcp_servers.atticdb` table of `$CODEX_HOME/config.toml`.
- `claude -p <prompt> ...`, `codex exec ... <prompt>`: an agent, with or without atticdb wired in.
  A prompt holding an evidence token is a writer. It saves the prompt with `fact set` when
  atticdb is wired in, else appends it to HANDOFF.md when the repo's CLAUDE.md or AGENTS.md asks
  for that, else writes nothing. STUB_WRITER=silent makes it write nothing anyway.
  STUB_NATIVE_MEMORY=1 also writes the prompt to the harness's native memory for the repo.
  Every writer session also leaves a transcript of its prompt in its config dir.
  STUB_READER_GREP=1 makes a reader grep the repo's parent and both harness dirs for the tokens.
  Any other prompt is a reader. It runs the SessionStart hook, reads HANDOFF.md if there is one
  (a tool result in its stream), and answers H1 right only if LEDG-7731 reached it.
"""

import hashlib
import json
import os
import re
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
                                "atticdb_home": bool(os.environ.get("ATTICDB_HOME")),
                                "api_keys": sorted(k for k in os.environ
                                                   if k in ACCOUNT_ENV or k.startswith("ANTHROPIC_FOUNDRY_")),
                                "git_env": sorted(k for k in os.environ if k.startswith("GIT_")),
                                "outside_home": sorted(
                                    k for k in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "COPILOT_HOME")
                                    if not os.environ.get(k, os.environ["HOME"]).startswith(os.environ["HOME"])),
                                "dirs": {k: os.environ.get(k) for k in ("CLAUDE_CONFIG_DIR", "CODEX_HOME",
                                                                         "CLAUDE_SECURESTORAGE_CONFIG_DIR")},
                                "codex_home_files": sorted(os.listdir(os.environ["CODEX_HOME"]))
                                if os.path.isdir(os.environ.get("CODEX_HOME", "")) else None})
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
    if ARGS[:5] == ["mcp", "add", "-s", "user", "atticdb"]:
        rest, env = ARGS[5:], {}
        while rest and rest[0] == "-e":
            key, _, value = rest[1].partition("=")
            env[key] = value
            rest = rest[2:]
        assert rest[0] == "--", rest
        entry = {"type": "stdio", "command": rest[1], "args": rest[2:], "env": env}
        state.setdefault("mcpServers", {})["atticdb"] = entry
    elif ARGS[:5] == ["mcp", "remove", "-s", "user", "atticdb"]:
        state.get("mcpServers", {}).pop("atticdb", None)
    else:
        sys.exit(2)
    write_json(claude_state(), state)


def codex_server():
    try:
        with open(os.path.join(os.environ["CODEX_HOME"], "config.toml"), "rb") as f:
            return tomllib.load(f).get("mcp_servers", {}).get("atticdb")
    except FileNotFoundError:
        return None


def hook_context(settings):
    for group in read_json(settings, {}).get("hooks", {}).get("SessionStart", []):
        for hook in group.get("hooks", []):
            if "atticdb" in hook.get("command", ""):
                stdin = json.dumps({"cwd": os.getcwd(), "hook_event_name": "SessionStart"})
                out = subprocess.run(["sh", "-c", hook["command"]], input=stdin, capture_output=True,
                                     text=True, timeout=15).stdout
                return json.loads(out or "{}").get("hookSpecificOutput", {}).get("additionalContext", "")
    return ""


def atticdb(server, *args):
    """Runs an atticdb shell verb as the MCP entry would: its binary, agent and env."""
    exe, agent = server["command"], server["args"][server["args"].index("--agent") + 1]
    env = dict(os.environ, **server.get("env", {}))
    out = subprocess.run([exe, *args, "--agent", agent], env=env, capture_output=True, text=True,
                         timeout=30)
    return out.stdout


WRITER_MARKS = ("LEDG-7731", "INC-2209", "STOR-5150", "bramble-ci", "FIN-88")
H1_RIGHT = "JSON headers were 412 bytes and struct cut them to 24 bytes, per ticket LEDG-7731."


def native_memory():
    if PROG == "claude":
        slug = re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(os.getcwd()))
        return os.path.join(os.environ["CLAUDE_CONFIG_DIR"], "projects", slug, "memory", "MEMORY.md")
    return os.path.join(os.environ["CODEX_HOME"], "memories", "stub.md")


def transcript():
    if PROG == "claude":
        slug = re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(os.getcwd()))
        return os.path.join(os.environ["CLAUDE_CONFIG_DIR"], "projects", slug, "s1.jsonl")
    return os.path.join(os.environ["CODEX_HOME"], "sessions", "2026", "11", "04", "rollout-s1.jsonl")


def asks_for_handoff():
    for name in ("CLAUDE.md", "AGENTS.md"):
        try:
            with open(name) as f:
                if "HANDOFF.md" in f.read():
                    return True
        except FileNotFoundError:
            pass
    return False


def write(prompt, server):
    """(tool calls as (kind, name, args, result), reply). kind is mcp or file."""
    calls = []
    if os.environ.get("STUB_WRITER") != "silent":
        if server is not None:
            key = "repo/evalrepo/decision-" + hashlib.sha256(prompt.encode()).hexdigest()[:8]
            args = {"key": key, "content": prompt}
            atticdb(server, "fact", "set", "--key", args["key"], "--content", args["content"])
            calls.append(("mcp", "fact_set", args, "ok"))
        elif asks_for_handoff():
            with open("HANDOFF.md", "a") as f:
                f.write(f"- {prompt}\n")
            calls.append(("file", "Write", {"file_path": "HANDOFF.md"}, "ok"))
    # Every session leaves a transcript in its config dir, as the real agents do.
    path = transcript()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a") as f:
        f.write(json.dumps({"type": "user", "prompt": prompt}) + "\n")
    if os.environ.get("STUB_NATIVE_MEMORY"):
        path = native_memory()
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a") as f:
            f.write(f"- {prompt}\n")
    return calls, "Done."


def read(prompt, context):
    calls, known = [], context + "\n" + prompt
    if os.environ.get("STUB_READER_GREP"):
        # Look everywhere a reader could: the repo's parent, and both harness dirs from the env.
        where = ["..", os.environ["CLAUDE_CONFIG_DIR"], os.environ["CODEX_HOME"]]
        marks = [x for m in WRITER_MARKS for x in ("-e", m)]
        out = subprocess.run(["grep", "-r", "-I", "-h", *marks, *where], capture_output=True, text=True,
                             timeout=30).stdout
        calls.append(("file", "Bash", {"file_path": f"grep -r {' '.join(where)}"}, out or "(no matches)"))
        known += "\n" + out
    if os.path.exists("HANDOFF.md"):
        with open("HANDOFF.md") as f:
            text = f.read()
        calls.append(("file", "Read", {"file_path": "HANDOFF.md"}, text))
        known += "\n" + text
    reply = H1_RIGHT if "LEDG-7731" in known else "I have no record of that."
    return calls, reply


def act(prompt, server, context):
    if any(m in prompt for m in WRITER_MARKS) and not prompt.startswith("Notes from the last session"):
        return write(prompt, server)
    return read(prompt, context)


def claude_events(calls, reply):
    yield {"type": "system", "subtype": "init", "session_id": "s1", "tools": []}
    for i, (kind, name, args, result) in enumerate(calls):
        full = f"mcp__atticdb__{name}" if kind == "mcp" else name
        yield {"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": f"toolu_{i}", "name": full, "input": args}]}}
        yield {"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": f"toolu_{i}", "content": [{"type": "text", "text": result}]}]}}
    yield {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}}
    yield {"type": "result", "subtype": "success", "is_error": False, "duration_ms": 1200,
           "num_turns": len(calls) + 1, "result": reply,
           "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000,
                     "output_tokens": 20}}


def codex_events(calls, reply):
    yield {"type": "thread.started", "thread_id": "t1"}
    yield {"type": "turn.started"}
    for i, (kind, name, args, result) in enumerate(calls):
        if kind == "mcp":
            item = {"id": f"item_{i}", "type": "mcp_tool_call", "server": "atticdb", "tool": name,
                    "arguments": args, "status": "in_progress"}
            done = dict(item, status="completed", result={"content": [{"type": "text", "text": result}]})
        else:
            item = {"id": f"item_{i}", "type": "command_execution", "command": f"cat {args['file_path']}",
                    "aggregated_output": "", "status": "in_progress"}
            done = dict(item, status="completed", aggregated_output=result, exit_code=0)
        yield {"type": "item.started", "item": item}
        yield {"type": "item.completed", "item": done}
    yield {"type": "item.completed", "item": {"id": "item_9", "type": "agent_message", "text": reply}}
    yield {"type": "turn.completed", "usage": {"input_tokens": 1110, "cached_input_tokens": 1000,
                                               "output_tokens": 20}}


def agent(prompt):
    if os.environ.get("STUB_EXIT"):
        sys.stderr.write(os.environ.get("STUB_STDERR", "") + "\n")
        sys.exit(int(os.environ["STUB_EXIT"]))
    if os.environ.get("STUB_SLEEP"):
        time.sleep(float(os.environ["STUB_SLEEP"]))
    if PROG == "codex" and os.environ.get("STUB_REFRESH_AUTH"):  # as a token refresh would
        with open(os.path.join(os.environ["CODEX_HOME"], "auth.json"), "w") as f:
            json.dump({"refreshed": os.environ["STUB_REFRESH_AUTH"]}, f)
    replay = os.environ.get("STUB_REPLAY")
    if replay:
        with open(replay) as f:
            sys.stdout.write(f.read())
        return
    if PROG == "claude":
        server = read_json(claude_state(), {}).get("mcpServers", {}).get("atticdb")
        context = hook_context(os.path.join(os.environ["CLAUDE_CONFIG_DIR"], "settings.json"))
    else:
        server = codex_server()
        context = hook_context(os.path.join(os.environ["CODEX_HOME"], "hooks.json"))
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
    if PROG == "codex" and ARGS == ["mcp", "get", "atticdb", "--json"]:
        server = codex_server()
        if server is None:
            sys.exit(1)
        transport = {"type": "stdio", "command": server["command"], "args": server.get("args", []),
                     "env": server.get("env")}
        print(json.dumps({"name": "atticdb", "enabled": True, "transport": transport}))
        return
    if PROG == "claude" and ARGS[:1] == ["-p"]:
        return agent(ARGS[1])
    if PROG == "codex" and ARGS[:1] == ["exec"]:
        return agent(ARGS[-1])
    sys.exit(2)


main()
