#!/usr/bin/env python3
"""A 60-second demo: a correction from 1 agent reaches another, and a stale write is refused.

Runs 2 MCP clients, `atticdb mcp --agent agent-a` and `--agent agent-b`, against a fresh
temp home. Each MCP session tracks the last revision it read of each key (D107). A
`fact_set` to a key that changed since this session read it writes nothing and returns
the current value.

Usage: python3 scripts/demo_correction.py [--fast]

The binary is ATTICDB_BIN, else `atticdb` on PATH. Exits 0 when every step matches the
story, 1 when one doesn't. It stops the server it started and removes the temp home,
also on failure. Python 3 stdlib only.
"""

import argparse
import json
import os
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time

KEY = "repo/shop/package-manager"
FIRST = "Use npm."
CORRECTION = "Use pnpm, not npm. npm broke the lockfile on 2026-11-02."
STALE = "Use npm. Faster installs."
PAUSE = 1.5
REPLY_TIMEOUT = 30.0
WIDTH = 78


class Deviation(Exception):
    """A step did not give the expected outcome."""


class Agent:
    """1 `atticdb mcp` process, spoken to as JSON-RPC 2.0 over newline-delimited stdio."""

    def __init__(self, binary, name, env, cwd):
        self.name = name
        self.next_id = 0
        self.stderr = []
        self.proc = subprocess.Popen(
            [binary, "mcp", "--agent", name],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=env, cwd=cwd, text=True, encoding="utf-8")
        self.lines = queue.Queue()
        threading.Thread(target=self._pump, args=(self.proc.stdout, self.lines.put),
                         daemon=True).start()
        threading.Thread(target=self._pump, args=(self.proc.stderr, self.stderr.append),
                         daemon=True).start()

    @staticmethod
    def _pump(stream, sink):
        for line in stream:
            sink(line)
        sink(None)

    def send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params):
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        deadline = time.monotonic() + REPLY_TIMEOUT
        while True:
            try:
                line = self.lines.get(timeout=max(0.0, deadline - time.monotonic()))
            except queue.Empty:
                raise Deviation(f"{self.name}: no reply to {method} in {REPLY_TIMEOUT:.0f} s")
            if line is None:
                err = "".join(l for l in self.stderr if l).strip()
                raise Deviation(f"{self.name}: atticdb mcp exited. stderr: {err}")
            reply = json.loads(line)
            if reply.get("id") != self.next_id:
                continue  # a notification or a stray message
            if "error" in reply:
                raise Deviation(f"{self.name}: {method} failed: {reply['error']}")
            return reply["result"]

    def start(self):
        self.request("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "atticdb-demo", "version": "1"},
        })
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool(self, name, arguments):
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self):
        """Closes stdin so the adapter exits. Kills this child if it doesn't."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def text_of(result):
    return "\n".join(c.get("text", "") for c in result.get("content", []))


def check(ok, message):
    if not ok:
        raise Deviation(message)


class Demo:
    def __init__(self, fast):
        self.fast = fast

    def pause(self):
        if not self.fast:
            time.sleep(PAUSE)

    @staticmethod
    def say(line=""):
        print(line, flush=True)

    @staticmethod
    def detail(text):
        for line in text.splitlines():
            for part in textwrap.wrap(line, WIDTH - 6) or [""]:
                print("    | " + part, flush=True)

    def run(self, a, b):
        self.say("AtticDB demo: a correction reaches another agent")
        self.say(f"2 agents share 1 store. Key: {KEY}")
        self.say()
        self.pause()

        self.say(f'[A] fact_set "{FIRST}"')
        r = a.tool("fact_set", {"key": KEY, "content": FIRST})
        check(r.get("structuredContent", {}).get("committed") is True,
              f"A's first fact_set did not commit: {text_of(r)}")
        self.say(f"    {text_of(r)}")
        self.pause()

        self.say("[B] fact_get")
        r = b.tool("fact_get", {"key": KEY})
        fact = r.get("structuredContent", {}).get("fact") or {}
        check(fact.get("content") == FIRST, f"B read {fact.get('content')!r}, not {FIRST!r}")
        self.say(f'    "{fact["content"]}" (by {fact["agent"]}, lsn {fact["lsn"]})')
        self.pause()

        self.say("[A] fact_get, then fact_set the correction")
        r = a.tool("fact_get", {"key": KEY})
        check(text_of(r) == FIRST, f"A read {text_of(r)!r}, not {FIRST!r}")
        r = a.tool("fact_set", {"key": KEY, "content": CORRECTION})
        check(r.get("structuredContent", {}).get("committed") is True,
              f"A's correction did not commit: {text_of(r)}")
        self.say(f'    "{CORRECTION}"')
        self.say(f"    {text_of(r)}")
        self.pause()

        self.say(f'[B] fact_set "{STALE}" (from its old read)')
        r = b.tool("fact_set", {"key": KEY, "content": STALE})
        reply = text_of(r)
        check(r.get("isError") is True and reply.startswith("conflict:"),
              f"B's stale fact_set was not refused: {reply}")
        check(CORRECTION in reply, f"the refusal does not carry the current value: {reply}")
        self.say("    Refused. Nothing was written. The server says:")
        self.detail(reply)
        self.pause()

        self.say("[B] fact_history, newest first")
        r = b.tool("fact_history", {"key": KEY})
        revisions = r.get("structuredContent", {}).get("revisions")
        check(isinstance(revisions, list), f"fact_history gave no revisions: {text_of(r)}")
        for rev in revisions:
            self.say(f'    lsn {rev["lsn"]} by {rev["agent"]}: {rev["content"]}')
        got = [(rev["agent"], rev["content"]) for rev in revisions]
        want = [("agent-a", CORRECTION), ("agent-a", FIRST)]
        check(got == want, f"history is {got}, expected {want}")
        self.pause()

        self.say()
        self.say(f'Current value: "{CORRECTION}"')
        self.say("B's stale write never landed.")
        self.say("B saw the correction before any write of its own could land.")


def server_pid(data_dir):
    """The pid in `<data_dir>/LOCK`: `pid=<pid> instance=<id> unix=<path> tcp=<addr>`."""
    try:
        with open(os.path.join(data_dir, "LOCK"), encoding="utf-8") as f:
            first = f.read().split(" ", 1)[0]
    except OSError:
        return None
    if not first.startswith("pid="):
        return None
    try:
        pid = int(first[4:])
    except ValueError:
        return None
    return pid if pid > 1 and pid != os.getpid() else None


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def stop_server(data_dir):
    """Stops the server named in LOCK with SIGTERM, then SIGKILL after 5 s. Exact pid only."""
    pid = server_pid(data_dir)
    if pid is None or not alive(pid):
        return
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while alive(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    if alive(pid):
        os.kill(pid, signal.SIGKILL)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fast", action="store_true", help="skip the pauses")
    args = parser.parse_args()

    binary = os.environ.get("ATTICDB_BIN") or shutil.which("atticdb")
    if not binary:
        print("atticdb not found. Set ATTICDB_BIN or put atticdb on PATH.", file=sys.stderr)
        return 2
    binary = os.path.abspath(binary) if os.sep in binary else binary

    # SIGTERM runs the cleanup below, like Ctrl-C does.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))

    # macOS caps socket paths at 104 bytes, so the temp home stays short.
    tmp = tempfile.mkdtemp(prefix="ad", dir="/tmp")
    home = os.path.join(tmp, "h")
    data_dir = os.path.join(home, "data")
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("ATTICDB_") and not k.startswith("XDG_")}
    env.update({
        "HOME": tmp,
        "ATTICDB_HOME": home,
        "ATTICDB_NO_UPDATE_CHECK": "1",
        "XDG_DATA_HOME": os.path.join(tmp, ".local", "share"),
        "XDG_CONFIG_HOME": os.path.join(tmp, ".config"),
        "XDG_CACHE_HOME": os.path.join(tmp, ".cache"),
        "XDG_STATE_HOME": os.path.join(tmp, ".local", "state"),
    })

    agents = []
    try:
        a = Agent(binary, "agent-a", env, tmp)
        agents.append(a)
        b = Agent(binary, "agent-b", env, tmp)
        agents.append(b)
        a.start()
        b.start()
        Demo(args.fast).run(a, b)
        return 0
    except Deviation as e:
        print(f"FAIL: {e}", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError) as e:
        print(f"FAIL: {e!r}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    finally:
        for agent in agents:
            agent.close()
        stop_server(data_dir)
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
