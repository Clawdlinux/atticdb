"""One eval run, and a batch of them.

Each run gets a temp root with its own zerodb home (`<root>/zh`) and fixture repo
(`<root>/evalrepo`). It seeds the store, runs `zerodb setup` for 1 harness against the eval
config dirs, runs the agent headless, judges the result, then disconnects the harness, stops
the zerodb server the run started, and deletes the root.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import signal
import tempfile
import time
from dataclasses import dataclass, field

from . import agents, guard
from .parse import PARSERS, Outcome
from .proc import Result, Runner
from .scenarios import CALC_PY, NEEDS_BLOCK, README_MD, REPO, Scenario, judge, seed_commands

SUN_PATH_BYTES = 104  # macOS; Linux allows 108
BASE_REF = "refs/eval/base"


class HarnessError(Exception):
    """A run that could not be prepared, so its agent did not run."""


@dataclass
class Config:
    zerodb: str
    home: str  # the real HOME
    claude_dir: str
    codex_dir: str
    tmp_parent: str | None = None
    agent_timeout: float = agents.AGENT_TIMEOUT
    base_env: dict = field(default_factory=lambda: dict(os.environ))
    keep_api_keys: bool = False


@dataclass
class Step:
    phase: str  # fixture, seed, setup, agent, check, teardown
    argv: list[str]
    cwd: str
    timeout: float
    extra_env: dict = field(default_factory=dict)
    stdin: bytes | None = None


def paths(root: str) -> tuple[str, str]:
    return os.path.join(root, "zh"), os.path.join(root, REPO)


def hook_input(repo: str) -> bytes:
    return json.dumps({"cwd": repo, "hook_event_name": "SessionStart"}).encode()


def plan(cfg: Config, harness: str, scenario: Scenario, root: str) -> dict[str, list[Step]]:
    """Every command of 1 run, by phase. A dry run prints these; a real run runs them."""
    zh, repo = paths(root)
    z, git = cfg.zerodb, agents.git_env()
    steps = {
        "fixture": [
            Step("fixture", ["git", "init", "-q"], repo, agents.GIT_TIMEOUT, git),
            Step("fixture", ["git", "add", "calc.py", "README.md"], repo, agents.GIT_TIMEOUT, git),
            Step("fixture", ["git", "commit", "-q", "--no-verify", "-m", "Initial commit"], repo,
                 agents.GIT_TIMEOUT, git),
            Step("fixture", ["git", "update-ref", BASE_REF, "HEAD"], repo, agents.GIT_TIMEOUT, git),
        ],
        "seed": [Step("seed", [z] + args, repo, agents.ZERODB_TIMEOUT) for args in seed_commands(scenario)],
        "setup": [
            Step("setup", agents.disconnect_argv(z, harness), repo, agents.SETUP_TIMEOUT),
            Step("setup", agents.setup_argv(z, harness), repo, agents.SETUP_TIMEOUT),
        ],
        "agent": [Step("agent", agents.agent_argv(harness, scenario.prompt), repo, cfg.agent_timeout)],
        "check": [],
        "teardown": [Step("teardown", agents.disconnect_argv(z, harness), repo, agents.SETUP_TIMEOUT)],
    }
    if scenario.rule in NEEDS_BLOCK:
        steps["check"].append(Step("check", agents.hook_argv(z, harness), repo, agents.HOOK_TIMEOUT,
                                   stdin=hook_input(repo)))
    if scenario.rule == "summary":
        steps["check"].append(Step("check", ["git", "diff", BASE_REF, "--", "calc.py"], repo,
                                   agents.GIT_TIMEOUT, git))
    return steps


def env_for(cfg: Config, root: str, step: Step) -> dict:
    zh, _ = paths(root)
    return agents.child_env(cfg.base_env, home=cfg.home, zerodb_home=zh, claude_dir=cfg.claude_dir,
                            codex_dir=cfg.codex_dir, extra=step.extra_env, keep_api_keys=cfg.keep_api_keys)


def describe(cfg: Config, harness: str, scenario: Scenario, root: str = "<root>") -> list[str]:
    """The dry-run lines of 1 run: each command, its cwd and timeout, and its env by name only."""
    zh, repo = paths(root)
    lines = [f"  write {repo}/calc.py and {repo}/README.md"]
    for phase, steps in plan(cfg, harness, scenario, root).items():
        for step in steps:
            extra = f" + {', '.join(sorted(step.extra_env))}" if step.extra_env else ""
            stdin = " <stdin: hook JSON>" if step.stdin is not None else ""
            lines.append(f"  [{phase}] cd {step.cwd} && {shlex.join(step.argv)}{stdin}")
            lines.append(f"      timeout {step.timeout:g} s; env: base{extra}")
    lines.append(f"  stop the zerodb server named in {zh}/data/LOCK, then delete {root}")
    return lines


def _exec(cfg: Config, runner: Runner, root: str, step: Step) -> Result:
    return runner.run(step.argv, env=env_for(cfg, root, step), root=root, cwd=step.cwd,
                      timeout=step.timeout, stdin=step.stdin)


def _code(r: Result) -> int:
    """The exit code for the CSV: -1 for a timeout, 127 for a missing program."""
    return 127 if r.missing else (-1 if r.code is None else r.code)


def _required(cfg: Config, runner: Runner, root: str, step: Step) -> Result:
    r = _exec(cfg, runner, root, step)
    if r.code != 0:
        what = "timed out" if r.timed_out else f"exited {_code(r)}"
        raise HarnessError(f"{step.phase} step {os.path.basename(step.argv[0])} {step.argv[1]} {what}")
    return r


def agent_finished(r: Result, out: Outcome) -> bool:
    """Whether the agent ended cleanly: exit 0, no timeout, a final reply, and no error it
    reported. A pass needs this as well as the scenario rule. Unparsed lines that don't stop
    the final reply from being found count only toward `parse_ok`."""
    return r.code == 0 and not r.timed_out and not r.missing and out.final and not out.is_error


def _why(r: Result, words: tuple[str, ...], paths: dict[str, str], stream: str = "both",
         last: bool = False) -> list[str]:
    """Up to 2 output lines that hold 1 of `words`, the first or the `last` ones, redacted and
    cut, for the notes. Paths from the env, and anything shaped like a token, are replaced first."""
    blobs = {"both": r.stdout + b"\n" + r.stderr, "stderr": r.stderr}[stream]
    lines = [line.strip() for line in blobs.decode("utf-8", "replace").splitlines() if line.strip()]
    hits = list(dict.fromkeys(line for line in lines if any(w in line.lower() for w in words)))
    hits = hits[-2:] if last else hits[:2]
    return [f"why={guard.excerpt(line, paths, 160)}" for line in hits]


def hook_block(stdout: bytes) -> str:
    """The block in the hook's JSON line, or '' for `{}` or anything else."""
    try:
        obj = json.loads(stdout.decode("utf-8", "replace").strip() or "{}")
    except ValueError:
        return ""
    if not isinstance(obj, dict):
        return ""
    inner = obj.get("hookSpecificOutput")
    text = inner.get("additionalContext") if isinstance(inner, dict) else obj.get("additionalContext")
    return text if isinstance(text, str) else ""


def stop_server(cfg: Config, runner: Runner, root: str) -> str | None:
    """Stops the zerodb server named in the run's `LOCK`, if it is this run's server. Returns a
    problem for the notes, or None."""
    data = os.path.join(paths(root)[0], "data")
    try:
        with open(os.path.join(data, "LOCK")) as f:  # in the run's own temp home
            text = f.read(4096)
    except OSError:
        return None
    m = re.match(r"pid=(\d+)\b", text)
    if not m or not _alive(int(m.group(1))):
        return None
    pid = int(m.group(1))
    # -ww: no width limit, so an inherited COLUMNS can't cut the line before --data-dir.
    step = Step("teardown", ["ps", "-ww", "-o", "command=", "-p", str(pid)], "/", 10.0)
    r = runner.run(step.argv, env=env_for(cfg, root, step), root=root, cwd=root, timeout=step.timeout)
    command = r.stdout.decode("utf-8", "replace")
    if " serve " not in f" {command} " or f"--data-dir {data}" not in command:
        return None if not _alive(pid) else "the LOCK pid is not this run's server, so it was left alone"
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            return None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if not _alive(pid):
                return None
            time.sleep(0.05)
    return "the zerodb server did not stop"


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def make_root(cfg: Config) -> str:
    root = tempfile.mkdtemp(prefix="zse-", dir=cfg.tmp_parent)
    data = os.path.join(paths(root)[0], "data")
    longest = len(os.fsencode(os.path.join(data, ".zs.12345678", "s"))) + 1
    if longest > SUN_PATH_BYTES:
        shutil.rmtree(root, ignore_errors=True)
        raise guard.Refused(f"the temp dir {cfg.tmp_parent or tempfile.gettempdir()} is too long for "
                            "the zerodb socket path. Set TMPDIR to a shorter dir")
    return root


@dataclass
class RunResult:
    harness: str
    scenario: str
    run: int
    passed: bool = False
    outcome: Outcome = field(default_factory=Outcome)
    wall_s: float | None = None
    setup_exit: int | None = None
    notes: list[str] = field(default_factory=list)
    errored: bool = False


def run_one(cfg: Config, runner: Runner, harness: str, scenario: Scenario, index: int) -> RunResult:
    res = RunResult(harness, scenario.id, index)
    root = make_root(cfg)
    repo = paths(root)[1]
    steps = plan(cfg, harness, scenario, root)
    setup_tried = False
    redact_paths = {root: "<root>", os.path.realpath(root): "<root>", cfg.claude_dir: "<claude-dir>",
                    cfg.codex_dir: "<codex-dir>", cfg.zerodb: "<zerodb>", cfg.home: "~"}
    try:
        os.makedirs(repo)
        with open(os.path.join(repo, "calc.py"), "w") as f:
            f.write(CALC_PY)
        with open(os.path.join(repo, "README.md"), "w") as f:
            f.write(README_MD)
        for step in steps["fixture"] + steps["seed"]:
            _required(cfg, runner, root, step)
        pre, setup = steps["setup"]
        setup_tried = True
        r = _exec(cfg, runner, root, pre)
        if r.code != 0:
            res.notes.append(f"pre_disconnect_exit={_code(r)}")
        r = _exec(cfg, runner, root, setup)
        res.setup_exit = _code(r)
        if res.setup_exit != 0:
            res.notes.append("agent skipped: setup failed")
            res.notes.extend(_why(r, ("failed", "skipped", "refused", "error"), redact_paths))
            return res
        (agent,) = steps["agent"]
        r = _exec(cfg, runner, root, agent)
        res.wall_s = r.wall_s
        if r.missing:
            res.notes.append(f"{agent.argv[0]} is not on PATH")
        elif r.timed_out:
            res.notes.append(f"agent timed out after {agent.timeout:g} s")
        elif r.code != 0:
            res.notes.append(f"agent_exit={r.code}")
            res.notes.extend(_why(r, ("",), redact_paths, stream="stderr", last=True))
        res.outcome = PARSERS[harness](r.stdout)
        block, diff = "", ""
        for step in steps["check"]:
            c = _exec(cfg, runner, root, step)
            if c.code != 0:
                res.notes.append(f"check {step.argv[1]} exited {_code(c)}")
            if step.argv[0] == "git":
                diff = c.stdout.decode("utf-8", "replace")
            else:
                block = hook_block(c.stdout)
        verdict = judge(scenario, res.outcome, block, diff)
        clean = agent_finished(r, res.outcome)
        res.passed = verdict.passed and clean
        res.notes.extend(verdict.reasons)
        if not res.outcome.final:
            res.notes.append("no final reply")
        if res.outcome.unparsed:
            res.notes.append(f"unparsed={res.outcome.unparsed}")
        if res.outcome.unknown:
            res.notes.append(f"unknown_events={res.outcome.unknown}")
        if res.outcome.is_error:
            res.notes.append("the agent reported an error")
        res.notes.append("reply=" + guard.excerpt(res.outcome.reply, redact_paths))
        return res
    except HarnessError as e:
        res.errored = True
        res.notes.append(f"error: {e}")
        return res
    finally:
        if setup_tried:
            (down,) = steps["teardown"]
            r = _exec(cfg, runner, root, down)
            if r.code != 0:
                res.notes.append(f"disconnect_exit={_code(r)}")
        problem = stop_server(cfg, runner, root)
        if problem:
            res.notes.append(problem)
            res.errored = True
        else:
            shutil.rmtree(root, ignore_errors=True)
