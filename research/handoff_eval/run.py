"""One handoff run: writer sessions in harness A, 1 reader session in harness B, and 3 stages.

Each run gets a new temp root from `setup_eval.harness.make_root`: its own AtticDB home
(`<root>/zh`) and its own fixture repo (`<root>/evalrepo`). Before any agent starts, the run
checks that the store, the repo and the writer's native memory for this repo hold none of the
case's evidence tokens. After the run it disconnects both harnesses, stops the AtticDB server
the run started, and deletes the root, as setup_eval does.

Stages, each None when not measured or not applicable:

- captured: after the writers, an evidence token is in the run's AtticDB store (`atticdb search`),
  in any file of the repo working tree, or in the writer harness's native memory for this repo.
- delivered: before the reader's final reply, an evidence token was in its context: the
  SessionStart hook block for the reader harness (atticdb arm), a tool result in the reader's
  transcript, or the reader prompt (oracle arm only). None for H5.
- passed: `grade.grade` on the reader's final reply and the repo it leaves.

A session that times out, crashes, reports an error or has no final reply makes the run
`errored`, with the stage it died in. It is never a pass or a fail.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
from dataclasses import dataclass, field

from ..setup_eval import agents, guard
from ..setup_eval.harness import (BASE_REF, Config, HarnessError, Step, agent_finished, env_for, hook_block,
                                  hook_input, make_root, paths, stop_server)
from ..setup_eval.parse import PARSERS, Outcome
from ..setup_eval.proc import Result, Runner
from . import arms
from .cases import Case
from .fixture import write_fixture
from .grade import grade

# H5's reader needs no fact. Its evidence is H1's, which is unrelated history there, so delivery
# is not a stage of H5. Capture still is.
NO_DELIVERY = frozenset({"H5"})
MAX_SCAN_BYTES = 2_000_000  # a bigger file in the repo or a memory dir is read up to this size


class BudgetExceeded(Exception):
    """The next agent session would pass `--max-sessions`. It was not started."""


@dataclass
class Budget:
    """Agent sessions, writers and readers alike. `take` runs before a session starts."""
    limit: int
    used: int = 0

    def fits(self, n: int) -> bool:
        return self.used + n <= self.limit

    def take(self) -> None:
        if self.used >= self.limit:
            raise BudgetExceeded(f"session {self.used + 1} would pass --max-sessions {self.limit}")
        self.used += 1


@dataclass
class HandoffConfig(Config):
    """setup_eval's Config, plus the model per harness and the session budget."""
    models: dict[str, str] = field(default_factory=dict)
    budget: Budget | None = None


@dataclass
class HandoffResult:
    case: str
    arm: str
    writer: str
    reader: str
    run: int
    captured: bool | None = None
    captured_where: str = ""
    delivered: bool | None = None
    delivered_where: str = ""
    passed: bool | None = None
    errored: bool = False
    error_stage: str = ""
    reasons: list[str] = field(default_factory=list)
    sessions: int = 0  # agent sessions started
    input_tokens: int | None = None
    output_tokens: int | None = None
    wall_s: float | None = None  # agent sessions only
    writer_model: str = ""
    reader_model: str = ""

    def fail(self, stage: str, why: str) -> None:
        self.errored = True
        self.error_stage = self.error_stage or stage
        self.reasons.append(f"error in {stage}: {why}")


# ---- the commands of 1 run ----

def search_step(cfg: Config, repo: str, phase: str, token: str) -> Step:
    return Step(phase, [cfg.atticdb, "search", "--query", token], repo, agents.ATTICDB_TIMEOUT)


def plan(cfg: HandoffConfig, case: Case, arm: str, writer: str, reader: str, root: str) -> dict[str, list[Step]]:
    """Every child of 1 run, by phase. A dry run prints these; a real run runs them."""
    _, repo = paths(root)
    z, git = cfg.atticdb, agents.git_env()
    both = agents.HARNESSES
    fixture = []
    if arms.instruction_files(arm):
        fixture += [Step("fixture", ["git", "add", *arms.instruction_files(arm)], repo, agents.GIT_TIMEOUT, git),
                    Step("fixture", ["git", "commit", "-q", "--no-verify", "-m", "Handoff instructions"], repo,
                         agents.GIT_TIMEOUT, git)]
    fixture.append(Step("fixture", ["git", "update-ref", BASE_REF, "HEAD"], repo, agents.GIT_TIMEOUT, git))
    steps = {
        "fixture": fixture,
        "disconnect": [Step("disconnect", agents.disconnect_argv(z, h), repo, agents.SETUP_TIMEOUT) for h in both],
        "start": [search_step(cfg, repo, "start", t) for t in case.evidence],
        "setup": [Step("setup", agents.setup_argv(z, h), repo, agents.SETUP_TIMEOUT) for h in both]
        if arms.uses_atticdb(arm) else [],
        "writer": [Step("writer", arms.session_argv(writer, p, cfg.models.get(writer, ""), arm), repo,
                        cfg.agent_timeout) for p in case.writer_prompts] if arms.has_writer(arm) else [],
        "capture": [search_step(cfg, repo, "capture", t) for t in case.evidence] if arms.has_writer(arm) else [],
        "hook": [Step("hook", agents.hook_argv(z, reader), repo, agents.HOOK_TIMEOUT, stdin=hook_input(repo))]
        if arms.uses_atticdb(arm) else [],
        "reader": [Step("reader", arms.session_argv(reader, arms.reader_prompt(case, arm),
                                                    cfg.models.get(reader, ""), arm), repo, cfg.agent_timeout)],
        "grade": [Step("grade", ["git", "diff", BASE_REF], repo, agents.GIT_TIMEOUT, git)],
        "teardown": [Step("teardown", agents.disconnect_argv(z, h), repo, agents.SETUP_TIMEOUT) for h in both],
    }
    return steps


def _exec(cfg: Config, runner: Runner, root: str, step: Step) -> Result:
    return runner.run(step.argv, env=env_for(cfg, root, step), root=root, cwd=step.cwd, timeout=step.timeout,
                      stdin=step.stdin)


def _code(r: Result) -> int:
    return 127 if r.missing else (-1 if r.code is None else r.code)


def _required(cfg: Config, runner: Runner, root: str, step: Step) -> Result:
    r = _exec(cfg, runner, root, step)
    if r.code != 0:
        what = "timed out" if r.timed_out else f"exited {_code(r)}"
        raise HarnessError(f"{os.path.basename(step.argv[0])} {step.argv[1]} {what}")
    return r


# ---- where tokens are found ----

def _has(text: str, token: str) -> bool:
    return token.lower() in (text or "").lower()


def _where(found: list[tuple[str, str]]) -> str:
    """`place[tok,tok]; place[tok]`, places in the order found."""
    by: dict[str, list[str]] = {}
    for place, token in found:
        if token not in by.setdefault(place, []):
            by[place].append(token)
    return "; ".join(f"{place}[{','.join(toks)}]" for place, toks in by.items())


def store_hits(stdout: bytes, token: str) -> list[str]:
    """The places in an `atticdb search` JSON line whose content or key holds `token`."""
    places = []
    for line in stdout.decode("utf-8", "replace").splitlines():
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        hits = obj.get("hits") if isinstance(obj, dict) else None
        for hit in hits if isinstance(hits, list) else []:
            if not isinstance(hit, dict):
                continue
            text = f"{hit.get('content', '')}\n{hit.get('key', '')}"
            if _has(text, token):
                kind = hit.get("kind") if isinstance(hit.get("kind"), str) else "record"
                key = f" {hit['key']}" if isinstance(hit.get("key"), str) else ""
                places.append(f"atticdb:{kind}{key}")
    return places


def _read(path: str) -> str:
    try:
        with open(path, "rb") as f:
            return f.read(MAX_SCAN_BYTES).decode("utf-8", "replace")
    except OSError:
        return ""


def file_hits(folder: str, tokens: tuple[str, ...], label: str, only: set[str] | None = None) -> list[tuple[str, str]]:
    """(label:relpath, token) for each file under `folder` holding a token. `.git` is skipped.
    With `only`, just those relative paths are read."""
    found = []
    if not os.path.isdir(folder):
        return found
    for dirpath, dirnames, filenames in os.walk(folder):
        dirnames[:] = sorted(d for d in dirnames if d != ".git")
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, folder)
            if (only is not None and rel not in only) or os.path.islink(path):
                continue
            text = _read(path)
            found += [(f"{label}:{rel}", t) for t in tokens if _has(text, t)]
    return found


def claude_project_slug(path: str) -> str:
    """Claude Code names a project dir after the real path, every character that is not a
    letter or digit turned into `-`: `/private/var/x_y.z` gives `-private-var-x-y-z`."""
    return re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(path))


def native_memory_dir(cfg: Config, harness: str, repo: str) -> str:
    if harness == "claude-code":
        return os.path.join(cfg.claude_dir, "projects", claude_project_slug(repo), "memory")
    if harness == "codex":
        return os.path.join(cfg.codex_dir, "memories")
    raise ValueError(f"unknown harness {harness!r}")


def changed_files(before: dict, after: dict) -> set[str]:
    """Files added or changed in size or mtime between 2 `guard.snapshot`s."""
    return {p for p in guard.compare(before, after) if p in after}


def _strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for v in value.values():
            yield from _strings(v)
    elif isinstance(value, list):
        for v in value:
            yield from _strings(v)


def _events(stdout: bytes) -> list[dict]:
    out = []
    for line in stdout.decode("utf-8", "replace").splitlines():
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if isinstance(obj, dict):
            out.append(obj)
    return out


def _claude_final(ev: dict) -> bool:
    msg = ev.get("message")
    content = msg.get("content") if isinstance(msg, dict) else None
    return ev.get("type") == "assistant" and isinstance(content, list) and any(
        isinstance(b, dict) and b.get("type") == "text" for b in content)


def _codex_item(ev: dict) -> dict:
    item = ev.get("item")
    if not isinstance(item, dict):
        return {}
    fields = dict(item)
    if isinstance(item.get("details"), dict):
        fields.update(item["details"])
    return fields


def _codex_final(ev: dict) -> bool:
    if ev.get("type") == "item.completed":
        item = _codex_item(ev)
        return (item.get("type") or item.get("item_type")) in ("agent_message", "assistant_message")
    msg = ev.get("msg")
    return isinstance(msg, dict) and msg.get("type") in ("agent_message", "task_complete")


CODEX_OUTPUT_FIELDS = {"command_execution": ("aggregated_output", "output", "stdout", "stderr"),
                       "mcp_tool_call": ("result", "error")}
CODEX_LEGACY_OUTPUT = {"exec_command_end": ("aggregated_output", "formatted_output", "stdout", "stderr"),
                       "mcp_tool_call_end": ("result",)}


def tool_outputs(harness: str, stdout: bytes) -> list[str]:
    """The text of every tool result and file read in a transcript, before its final reply."""
    events = _events(stdout)
    is_final = _claude_final if harness == "claude-code" else _codex_final
    finals = [i for i, ev in enumerate(events) if is_final(ev)]
    texts: list[str] = []
    for ev in events[:finals[-1] if finals else len(events)]:
        if harness == "claude-code":
            if ev.get("type") != "user":
                continue
            msg = ev.get("message")
            content = msg.get("content") if isinstance(msg, dict) else None
            for block in content if isinstance(content, list) else []:
                if isinstance(block, dict) and block.get("type") == "tool_result":
                    texts += _strings(block.get("content"))
            texts += _strings(ev.get("tool_use_result"))
        elif ev.get("type") == "item.completed":
            item = _codex_item(ev)
            for name in CODEX_OUTPUT_FIELDS.get(item.get("type") or item.get("item_type"), ()):
                texts += _strings(item.get(name))
        elif isinstance(ev.get("msg"), dict):
            for name in CODEX_LEGACY_OUTPUT.get(ev["msg"].get("type"), ()):
                texts += _strings(ev["msg"].get(name))
    return texts


# ---- 1 run ----

def _unfinished(r: Result, out: Outcome, step: Step) -> str | None:
    """Why a session did not end cleanly, or None."""
    if agent_finished(r, out):
        return None
    if r.missing:
        return f"{step.argv[0]} is not on PATH"
    if r.timed_out:
        return f"timed out after {step.timeout:g} s"
    if r.code != 0:
        tail = [ln for ln in r.stderr.decode("utf-8", "replace").splitlines() if ln.strip()][-1:]
        return f"exited {r.code}" + (f" ({guard.excerpt(tail[0], None, 160)})" if tail else "")
    if out.is_error:
        return "the agent reported an error"
    return "no final reply"


def _add(total: int | None, n: int | None) -> int | None:
    return total if n is None else (total or 0) + n


def _session(cfg: HandoffConfig, runner: Runner, root: str, step: Step, harness: str, res: HandoffResult,
             label: str) -> tuple[Result, Outcome]:
    if cfg.budget is not None:
        cfg.budget.take()
    res.sessions += 1
    r = _exec(cfg, runner, root, step)
    out = PARSERS[harness](r.stdout)
    res.wall_s = (res.wall_s or 0.0) + r.wall_s
    res.input_tokens = _add(res.input_tokens, out.input_tokens)
    res.output_tokens = _add(res.output_tokens, out.output_tokens)
    if out.unparsed:
        res.reasons.append(f"{label} unparsed={out.unparsed}")
    why = _unfinished(r, out, step)
    if why:
        raise HarnessError(f"{label} {why}")
    return r, out


def _searches(cfg, runner, root, steps: list[Step]) -> list[tuple[str, str]]:
    found = []
    for step in steps:
        r = _required(cfg, runner, root, step)
        token = step.argv[step.argv.index("--query") + 1]
        found += [(place, token) for place in store_hits(r.stdout, token)]
    return found


def check_models(cfg: HandoffConfig, *harnesses: str) -> None:
    for h in harnesses:
        if not cfg.models.get(h):
            raise ValueError(f"no model id for {h}. Pass --claude-model and --codex-model")


def run_handoff(cfg: HandoffConfig, runner: Runner, case: Case, arm: str, writer: str, reader: str,
                index: int) -> HandoffResult:
    arms.check_arm(arm)
    check_models(cfg, *((writer, reader) if arms.has_writer(arm) else (reader,)))
    res = HandoffResult(case.id, arm, writer, reader, index,
                        writer_model=cfg.models.get(writer, ""),
                        reader_model=cfg.models[reader])
    root = make_root(cfg)
    _, repo = paths(root)
    steps = plan(cfg, case, arm, writer, reader, root)
    redact_paths = {root: "<root>", os.path.realpath(root): "<root>", cfg.claude_dir: "<claude-dir>",
                    cfg.codex_dir: "<codex-dir>", cfg.atticdb: "<atticdb>", cfg.home: "~"}
    stage, touched = "fixture", False
    try:
        write_fixture(repo)
        for name, body in arms.instruction_files(arm).items():
            with open(os.path.join(repo, name), "w") as f:
                f.write(body)
        for step in steps["fixture"]:
            _required(cfg, runner, root, step)

        # Any AtticDB wiring a crashed run left in the eval dirs goes first.
        stage, touched = "setup", True
        for step in steps["disconnect"]:
            r = _exec(cfg, runner, root, step)
            if r.code != 0:
                res.reasons.append(f"pre-disconnect {step.argv[-1]} exited {_code(r)}")

        stage = "start"
        dirty = _searches(cfg, runner, root, steps["start"])
        dirty += file_hits(repo, case.evidence, "repo")
        dirty += file_hits(native_memory_dir(cfg, "claude-code", repo), case.evidence, "claude-memory")
        if dirty:
            raise HarnessError(f"the run did not start clean: {_where(dirty)}")
        # The Codex memories dir is shared by every run. Only files a writer changes count as
        # capture, but an old one can still reach a Codex reader, so it is named here.
        stale = file_hits(native_memory_dir(cfg, "codex", repo), case.evidence, "codex-memory")
        if stale:
            res.reasons.append(f"stale codex memory from an earlier run: {_where(stale)}")

        stage = "setup"
        for step in steps["setup"]:
            _required(cfg, runner, root, step)

        if arms.has_writer(arm):
            stage = "writer"
            mem = native_memory_dir(cfg, writer, repo)
            mem_before = guard.snapshot(mem)
            for i, step in enumerate(steps["writer"], 1):
                _session(cfg, runner, root, step, writer, res, f"writer {i}")

            stage = "capture"
            found = _searches(cfg, runner, root, steps["capture"])
            found += file_hits(repo, case.evidence, "repo")
            label = "claude-memory" if writer == "claude-code" else "codex-memory"
            found += file_hits(mem, case.evidence, label, only=changed_files(mem_before, guard.snapshot(mem)))
            res.captured, res.captured_where = bool(found), _where(found)

        delivered: list[tuple[str, str]] = []
        if arms.uses_atticdb(arm):
            stage = "hook"
            (hook,) = steps["hook"]
            r = _exec(cfg, runner, root, hook)
            if r.code != 0:
                res.reasons.append(f"hook exited {_code(r)}")
            block = hook_block(r.stdout)
            delivered += [("hook", t) for t in case.evidence if _has(block, t)]
        if arm == "oracle":
            prompt = arms.reader_prompt(case, arm)
            delivered += [("prompt", t) for t in case.evidence if _has(prompt, t)]

        stage = "reader"
        (step,) = steps["reader"]
        r, out = _session(cfg, runner, root, step, reader, res, "reader")
        outputs = "\n".join(tool_outputs(reader, r.stdout))
        delivered += [("transcript", t) for t in case.evidence if _has(outputs, t)]
        if case.id not in NO_DELIVERY:
            res.delivered, res.delivered_where = bool(delivered), _where(delivered)

        stage = "grade"
        (diff_step,) = steps["grade"]
        d = _exec(cfg, runner, root, diff_step)
        if d.code != 0:
            res.reasons.append(f"git diff exited {_code(d)}")
        try:
            verdict = grade(case, out.reply, d.stdout.decode("utf-8", "replace"), repo)
        except (OSError, ValueError) as e:
            raise HarnessError(f"the grader failed: {e}") from e
        res.passed = verdict.passed
        res.reasons.extend(verdict.reasons)
        if not verdict.passed and any("timed out" in why for why in verdict.reasons):
            res.reasons.append("check timed out")
        res.reasons.append("reply=" + guard.excerpt(out.reply, redact_paths))
        return res
    except HarnessError as e:
        res.fail(stage, guard.redact(str(e), redact_paths))
        return res
    except BudgetExceeded as e:
        res.fail("budget", str(e))
        return res
    finally:
        if touched:
            for step in steps["teardown"]:
                r = _exec(cfg, runner, root, step)
                if r.code != 0:
                    res.reasons.append(f"disconnect {step.argv[-1]} exited {_code(r)}")
        problem = stop_server(cfg, runner, root)
        if problem:
            res.fail("teardown", problem)
        else:
            shutil.rmtree(root, ignore_errors=True)


def describe(cfg: HandoffConfig, case: Case, arm: str, writer: str, reader: str, root: str = "<root>") -> list[str]:
    """The dry-run lines of 1 run: each command, its cwd and timeout, and its env by name only."""
    zh, repo = paths(root)
    extra = ", ".join(arms.instruction_files(arm))
    lines = [f"  write the ledger fixture into {repo} and commit it" + (f", then write {extra}" if extra else "")]
    for phase, steps in plan(cfg, case, arm, writer, reader, root).items():
        if phase == "start":
            lines.append(f"  [start] scan {repo} and the claude-code memory dir of this repo for "
                         f"{', '.join(case.evidence)}; any hit errors the run")
        if phase == "capture" and steps:
            lines.append(f"  [capture] also scan {repo} and the files {writer} memory changed during the run")
        for step in steps:
            env = f" + {', '.join(sorted(step.extra_env))}" if step.extra_env else ""
            stdin = " <stdin: hook JSON>" if step.stdin is not None else ""
            session = " (agent session)" if phase in ("writer", "reader") else ""
            lines.append(f"  [{phase}]{session} cd {step.cwd} && {shlex.join(step.argv)}{stdin}")
            lines.append(f"      timeout {step.timeout:g} s; env: base{env}")
    lines.append(f"  grade with grade.grade, stop the atticdb server named in {zh}/data/LOCK, then delete {root}")
    return lines
