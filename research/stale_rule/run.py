"""Runs the pilot. Each run builds a fresh project and Claude config dir, runs `claude -p` on the
task, then grades the lines it added. Raw transcripts go to ~/.cache/atticdb-stale-rule, outside
the repo. Results go to a CSV.

    python3 -m research.stale_rule run --batch pilot --jobs 4
    python3 -m research.stale_rule summary research/stale_rule/results/pilot.csv
"""

from __future__ import annotations

import argparse
import csv
import itertools
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor

from . import fixtures, grade

CACHE = os.path.expanduser("~/.cache/atticdb-stale-rule")
# The eval login: Claude keeps it in the Keychain under a name from this dir (see handoff_eval).
LOGIN_DIR = os.path.expanduser("~/.cache/atticdb-setup-eval/claude")
TIMEOUT = 600.0
ALLOWED = "Bash(git:*),Bash(python3:*),Bash(python:*),Bash(pytest:*)"
CONFLICT = re.compile(r"conflict|disagree|stale|inconsisten|contradict|mismatch|outdated|goes against|against the|differ|n.t match", re.I)
GIT_LOOK = re.compile(r"\bgit\s+(log|blame|show|diff)\b")
# Never passed on: another account, provider or config would change the run.
DROP = ("ANTHROPIC_", "CLAUDE_", "GIT_", "ATTICDB_", "OPENAI_", "AWS_")
FIELDS = ["channel", "neutral", "convention", "history", "arm", "rep", "verdict", "correct", "tests", "ran_git", "mentioned_conflict",
          "model", "turns", "cost_usd", "seconds", "error", "transcript"]


def child_env(claude_dir: str) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith(DROP)}
    env.update(fixtures.git_env())
    env.update({"CLAUDE_CONFIG_DIR": claude_dir, "CLAUDE_SECURESTORAGE_CONFIG_DIR": LOGIN_DIR,
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1", "PYTEST_DISABLE_PLUGIN_AUTOLOAD": "1"})
    return env


def argv(task: str, model: str) -> list[str]:
    # --allowedTools is variadic, so it goes last in --flag=value form.
    return ["claude", "-p", task, "--model", model, "--output-format", "stream-json", "--verbose",
            "--permission-mode", "acceptEdits", "--strict-mcp-config", f"--allowedTools={ALLOWED}"]


def parse(stream: str) -> dict:
    """The model, bash commands, final text, turns and cost from a stream-json transcript."""
    out = {"model": "", "bash": [], "text": "", "turns": "", "cost_usd": ""}
    for line in stream.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get("type") == "system" and event.get("subtype") == "init":
            out["model"] = event.get("model", "")
        if event.get("type") == "assistant":
            for block in event.get("message", {}).get("content", []):
                if block.get("type") == "tool_use" and block.get("name") == "Bash":
                    out["bash"].append(block.get("input", {}).get("command", ""))
        if event.get("type") == "result":
            out["text"] = event.get("result", "") or ""
            out["turns"] = event.get("num_turns", "")
            out["cost_usd"] = event.get("total_cost_usd", "")
    return out


def hook_settings(block_path: str) -> dict:
    """A SessionStart hook that prints the block, in the shape AtticDB's hook prints for Claude Code."""
    return {"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": f"cat '{block_path}'"}]}]}}


def run_one(conv_name: str, history: str, arm: str, rep: int, model: str, batch: str, channel: str,
            neutral: bool = False) -> dict:
    conv = fixtures.CONVENTIONS[conv_name]
    row = {"channel": channel, "neutral": int(neutral), "convention": conv_name, "history": history, "arm": arm, "rep": rep, "error": ""}
    root = tempfile.mkdtemp(prefix="sr-")
    claude_root = tempfile.mkdtemp(prefix="sr-cfg-")
    try:
        repo = os.path.join(root, "project")
        base = fixtures.build(conv, history, repo, channel, neutral)
        claude_dir = os.path.join(claude_root, "claude")
        os.makedirs(claude_dir)
        if channel == "memory":
            block = fixtures.memory_block(arm, conv, history, repo)
            block_path = os.path.join(claude_root, "block.json")
            with open(block_path, "w") as f:
                json.dump({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": block}}, f)
            with open(os.path.join(claude_dir, "settings.json"), "w") as f:
                json.dump(hook_settings(block_path), f)
            user = fixtures.memory_user_file(arm)
        else:
            user = fixtures.user_file(arm, conv, repo)
        if user:
            with open(os.path.join(claude_dir, "CLAUDE.md"), "w") as f:
                f.write(user)
        start = time.monotonic()
        try:
            proc = subprocess.run(argv(conv.task, model), cwd=repo, env=child_env(claude_dir),
                                  stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=TIMEOUT)
            stream, code = proc.stdout, proc.returncode
            if code != 0:
                row["error"] = f"exit {code}: {proc.stderr[-200:].strip()}"
        except subprocess.TimeoutExpired as timed_out:
            stream = timed_out.stdout.decode() if isinstance(timed_out.stdout, bytes) else (timed_out.stdout or "")
            row["error"] = "timeout"
        row["seconds"] = round(time.monotonic() - start)
        os.makedirs(os.path.join(CACHE, batch), exist_ok=True)
        name = f"{channel}-{conv_name}-{history}-{arm}-{rep}.jsonl"
        with open(os.path.join(CACHE, batch, name), "w") as f:
            f.write(stream)
        parsed = parse(stream)
        lines = grade.added_lines(repo, base)
        found = grade.verdict(conv_name, lines)
        row.update({"verdict": found, "correct": int(grade.correct(history, found)),
                    "tests": int(grade.tests_pass(repo)),
                    "ran_git": int(any(GIT_LOOK.search(c) for c in parsed["bash"])),
                    "mentioned_conflict": int(bool(CONFLICT.search(parsed["text"]))),
                    "model": parsed["model"], "turns": parsed["turns"], "cost_usd": parsed["cost_usd"],
                    "transcript": name})
    except Exception as error:  # a broken run is a row, not a crash of the batch
        row["error"] = row["error"] or f"{type(error).__name__}: {error}"
    finally:
        shutil.rmtree(root, ignore_errors=True)
        shutil.rmtree(claude_root, ignore_errors=True)
    return row


def batch(a: argparse.Namespace) -> int:
    if not os.path.isdir(LOGIN_DIR):
        print(f"no eval Claude login at {LOGIN_DIR}", file=sys.stderr)
        return 2
    arms = a.arms or list(fixtures.MEMORY_ARMS if a.channel == "memory" else fixtures.ARMS)
    cells = list(itertools.product(a.conventions, fixtures.HISTORIES, arms, range(1, a.reps + 1)))
    if a.resume:
        # Reruns only the cells that errored or are missing in an earlier CSV of the same batch.
        with open(a.resume) as f:
            done = {(r["convention"], r["history"], r["arm"], int(r["rep"])) for r in csv.DictReader(f)
                    if not r["error"]}
        cells = [c for c in cells if c not in done]
    out = a.out or os.path.join(os.path.dirname(__file__), "results", f"{a.batch}.csv")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    print(f"{len(cells)} runs, {a.jobs} at a time, model {a.model}, to {out}", flush=True)
    rows = []
    with ThreadPoolExecutor(a.jobs) as pool, open(out, "w", newline="") as f:
        writer = csv.DictWriter(f, FIELDS)
        writer.writeheader()
        for row in pool.map(lambda c: run_one(*c, a.model, a.batch, a.channel, a.neutral), cells):
            writer.writerow(row)
            f.flush()
            rows.append(row)
            print(f"{row['convention']:10} {row['history']:9} {row['arm']:9} {row.get('verdict', ''):6} "
                  f"correct={row.get('correct', '')} git={row.get('ran_git', '')} {row['error']}", flush=True)
    print(summary_text(rows))
    return 0


def summary_text(rows: list[dict]) -> str:
    """Correct runs per arm and history, errored runs left out."""
    lines = ["| Arm | stale correct | migration correct | errored |", "|---|---|---|---|"]
    for arm in dict.fromkeys(r["arm"] for r in rows):
        mine = [r for r in rows if r["arm"] == arm]
        if not mine:
            continue
        cells = []
        for history in fixtures.HISTORIES:
            ok = [r for r in mine if r["history"] == history and not r["error"]]
            cells.append(f"{sum(int(r['correct']) for r in ok)}/{len(ok)}")
        cells.append(str(sum(1 for r in mine if r["error"])))
        lines.append(f"| {arm} | " + " | ".join(cells) + " |")
    return "\n".join(lines)


def main(argv_=None) -> int:
    p = argparse.ArgumentParser(prog="stale_rule")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--batch", required=True)
    r.add_argument("--reps", type=int, default=1)
    r.add_argument("--jobs", type=int, default=4)
    r.add_argument("--model", default="sonnet")
    r.add_argument("--conventions", nargs="+", default=sorted(fixtures.CONVENTIONS))
    r.add_argument("--arms", nargs="+", default=[])
    r.add_argument("--channel", choices=fixtures.CHANNELS, default="file")
    r.add_argument("--neutral", action="store_true", help="the stale refactor commit names only its files")
    r.add_argument("--out", default="")
    r.add_argument("--resume", default="", help="an earlier CSV: skip its cells that didn't error")
    s = sub.add_parser("summary")
    s.add_argument("csv")
    a = p.parse_args(argv_)
    if a.cmd == "summary":
        with open(a.csv) as f:
            print(summary_text(list(csv.DictReader(f))))
        return 0
    return batch(a)
