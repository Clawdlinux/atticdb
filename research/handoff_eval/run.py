"""One handoff run: writer sessions in harness A, 1 reader session in harness B, and 3 stages.

Each run gets a new temp root from `setup_eval.harness.make_root` with its own AtticDB home,
`<root>/zh`, and its own fixture repo, `<root>/evalrepo`.

Its harness config dirs sit in a second temp dir, `<harness-root>`, made apart from the root. It
is neither a parent nor a sibling of the repo, so a reader's `grep -r ..` can't reach them:

- `CODEX_HOME`, `<harness-root>/codex`, holding copies of the eval dir's `auth.json` and
  `config.toml` only. So Codex sessions, memories and history never carry over;
- `CLAUDE_CONFIG_DIR`, `<harness-root>/claude`, empty. Claude's claude.ai login sits in the macOS
  Keychain under a name made from the config dir. `CLAUDE_SECURESTORAGE_CONFIG_DIR` points that
  lookup at the eval Claude dir, so the login is found and nothing else is shared;
- `COPILOT_HOME`, `<harness-root>/copilot`, empty. Copilot CLI keeps its login in the OS keychain,
  not in COPILOT_HOME, so an empty dir is logged in. Its sessions, logs and session-store.db land
  there and go with the run.

After the capture scan, the writer harness's dir loses everything but its login file, as the
reader's env names it and its transcripts hold the writer's prompts. For Copilot that is
everything, session-store.db included: it is session history, not memory a reader loads.

A run uses the 2 harnesses of its direction. Only those are disconnected, set up and torn down.

Before any agent starts, the run checks that no harness config it will use mentions atticdb
(outside the setup the atticdb arm does itself), and that the store, the repo and both config dirs
hold none of the case's evidence. After the run it disconnects both harnesses, stops the AtticDB
server the run started, and deletes both temp dirs.

Stages, each None when not measured or not applicable:

- captured: after the writers, an evidence token is in the run's AtticDB store (`atticdb search`),
  in a file of the repo working tree, or in the writer harness's native memory for this repo.
- delivered: before the reader's final reply, an evidence token was in its context: the
  SessionStart hook block for the reader harness (atticdb arm), a tool result or file read in the
  reader's transcript, or the reader prompt (oracle arm only). None for H5.
- passed: `grade.grade` on the reader's final reply and the repo it leaves.

Tokens match through `EVIDENCE_PATTERNS`, never as bare substrings.

A session that times out, crashes, reports an error or has no final reply makes the run
`errored`, with the stage it died in. It is never a pass or a fail.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field

from ..setup_eval import agents, guard
from ..setup_eval.harness import (BASE_REF, Config, HarnessError, Step, agent_finished, hook_block, hook_input,
                                  make_root, paths, stop_server)
from ..setup_eval.parse import PARSERS, Outcome
from ..setup_eval.proc import Result, Runner
from . import arms
from .cases import Case
from .fixture import write_fixture
from .grade import grade

# H5's reader needs no fact. Its evidence is H1's, which is unrelated history there, so delivery
# is not a stage of H5. Capture still is.
NO_DELIVERY = frozenset({"H5"})
MAX_SCAN_BYTES = 2_000_000  # a bigger file is read up to this size

# ---- evidence matching ----


def _anchor(token: str) -> re.Pattern:
    """The token whole: no letter or digit right before or after it."""
    return re.compile(rf"(?<![A-Za-z0-9]){re.escape(token)}(?![A-Za-z0-9])", re.I)


# 3.9 that is not the start of 3.9.6, 3.95 or a path part such as python3.9/site-packages.
_PY39 = r"(?<![\d.])3\.9(?!\d|\.\d|/)"
# 1 pattern per evidence token in cases.CASES. Capture, delivery and the clean start all use it.
EVIDENCE_PATTERNS: dict[str, re.Pattern] = {
    "LEDG-7731": _anchor("LEDG-7731"),
    "INC-2209": _anchor("INC-2209"),
    "STOR-5150": _anchor("STOR-5150"),
    "bramble-ci": _anchor("bramble-ci"),
    "FIN-88": _anchor("FIN-88"),
    # A bare 412 is a file size, a line number or an HTTP status. Only the header size counts.
    "412": re.compile(r"\b412\s*(?:-\s*)?bytes?\b", re.I),
    # A bare 3.9 is any Python's version line, and python3.9 is an interpreter path. Only "Python 3.9" as
    # words, not inside a path, or 3.9 next to the runner, counts.
    "3.9": re.compile(rf"(?<!/)python\s+{_PY39}|bramble-ci[\s\S]{{0,40}}?{_PY39}|{_PY39}[\s\S]{{0,40}}?bramble-ci",
                      re.I),
    "plain files": re.compile(r"\bplain[- ]files?\b", re.I),
}


def has_token(text: str, token: str) -> bool:
    """Whether `text` holds `token` by its pattern. A token without one raises KeyError."""
    return bool(EVIDENCE_PATTERNS[token].search(text or ""))


def tokens_in(text: str, tokens) -> list[str]:
    return [t for t in tokens if has_token(text, t)]


# ---- budget, config and result ----


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
    """setup_eval's Config, plus the model per harness and the session budget. `claude_dir` and
    `codex_dir` are the shared eval dirs that each run's own dirs are made from."""
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
    check_timed_out: bool = False  # a code check of the grader timed out
    errored: bool = False
    error_stage: str = ""
    reasons: list[str] = field(default_factory=list)
    sessions: int = 0  # agent sessions started
    input_tokens: int | None = None
    output_tokens: int | None = None
    wall_s: float | None = None  # agent sessions only
    premium_requests: int | None = None  # Copilot CLI sessions only, from their `result` line
    writer_model: str = ""
    reader_model: str = ""

    def fail(self, stage: str, why: str) -> None:
        self.errored = True
        self.error_stage = self.error_stage or stage
        self.reasons.append(f"error in {stage}: {why}")


# ---- the run's own config dirs ----

SECURE_STORAGE_ENV = "CLAUDE_SECURESTORAGE_CONFIG_DIR"
CODEX_COPIES = ("auth.json", "config.toml")
CODEX_REQUIRED = "auth.json"
# Files that would wire AtticDB into a harness, by run config dir.
WIRING_FILES = {"claude": ("settings.json", ".claude.json"), "codex": ("config.toml", "hooks.json", "AGENTS.md"),
                "copilot": ("mcp-config.json", "settings.json", "config.json", os.path.join("hooks", "atticdb.json"),
                            os.path.join("instructions", "atticdb.instructions.md"))}


def make_harness_root(cfg: Config) -> str:
    """The temp dir for a run's own harness config dirs. It is made apart from the run root, so it
    is neither a parent nor a sibling of the repo, and `grep -r ..` in the repo can't reach it."""
    return tempfile.mkdtemp(prefix="zhh-", dir=cfg.tmp_parent)


def run_dirs(hroot: str) -> tuple[str, str]:
    """The run's own CLAUDE_CONFIG_DIR and CODEX_HOME, in its harness root."""
    return os.path.join(hroot, "claude"), os.path.join(hroot, "codex")


HARNESS_DIRS = {"claude-code": "claude", "codex": "codex", "copilot": "copilot"}


def harness_dir(hroot: str, harness: str) -> str:
    """The run's own config dir for `harness`: CLAUDE_CONFIG_DIR, CODEX_HOME or COPILOT_HOME."""
    return os.path.join(hroot, HARNESS_DIRS[harness])


def setup_home(hroot: str) -> str:
    """The HOME the copilot setup and disconnect steps get: `<harness-root>/home`, empty. `atticdb
    connect copilot` finds VS Code's user `mcp.json` through HOME (or XDG_CONFIG_HOME off macOS) and
    may drop an atticdb entry there, so it must never see the real one."""
    return os.path.join(hroot, "home")


def setup_env(hroot: str, harness: str) -> dict:
    """Extra env for `atticdb setup` and `disconnect` of `harness`: a per-run HOME and
    XDG_CONFIG_HOME for copilot, nothing for the others."""
    if harness != "copilot":
        return {}
    home = setup_home(hroot)
    return {"HOME": home, "XDG_CONFIG_HOME": os.path.join(home, ".config")}


def run_harnesses(writer: str, reader: str) -> tuple[str, ...]:
    """The harnesses a run disconnects, sets up and tears down: its direction's 2, in a fixed order."""
    return tuple(h for h in agents.ALL_HARNESSES if h in (writer, reader))


# What a harness dir keeps once its writer is done: the login, and nothing a reader could read.
# Copilot's login is in the OS keychain, so its dir keeps nothing.
LOGIN_FILES = {"claude-code": (".credentials.json",), "codex": ("auth.json",), "copilot": ()}


def scrub_harness_dir(hroot: str, harness: str) -> list[str]:
    """Deletes everything in `harness`'s run dir but its login files: transcripts, sessions,
    history, memory and config. Returns the top-level names it deleted."""
    folder = harness_dir(hroot, harness)
    gone = []
    for name in sorted(os.listdir(folder)) if os.path.isdir(folder) else []:
        if name in LOGIN_FILES[harness]:
            continue
        path = os.path.join(folder, name)
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path)
        else:
            os.remove(path)
        gone.append(name)
    return gone


def env_for(cfg: Config, root: str, step: Step, hroot: str) -> dict:
    """setup_eval's child env, with the run's own config dirs, and Claude's login looked up under
    the eval Claude dir. An inherited CLAUDE_SECURESTORAGE_CONFIG_DIR is always replaced."""
    zh, _ = paths(root)
    claude, codex = run_dirs(hroot)
    return agents.child_env(cfg.base_env, home=cfg.home, atticdb_home=zh, claude_dir=claude, codex_dir=codex,
                            copilot_dir=harness_dir(hroot, "copilot"),
                            extra={SECURE_STORAGE_ENV: cfg.claude_dir, **step.extra_env},
                            keep_api_keys=cfg.keep_api_keys)


def prepare_config_dirs(cfg: Config, hroot: str, harnesses=agents.ALL_HARNESSES) -> None:
    """Makes the run's config dirs, and copies Codex's login and config into its own when the run
    uses codex. Raises HarnessError without the eval Codex login, and guard.Refused for an unsafe
    dir, COPILOT_HOME included."""
    claude, codex = run_dirs(hroot)
    copilot = harness_dir(hroot, "copilot")
    # Checked before they exist, so a refused dir is never made, and again after, by file identity.
    guard.check_config_dirs(claude, codex, cfg.home, copilot_dir=copilot)
    for folder in (claude, codex, copilot, os.path.join(setup_home(hroot), ".config")):
        os.makedirs(folder, mode=0o700, exist_ok=True)
    guard.check_config_dirs(claude, codex, cfg.home, copilot_dir=copilot)
    if "codex" not in harnesses:
        return
    for name in CODEX_COPIES:
        src = os.path.join(cfg.codex_dir, name)
        if not os.path.isfile(src):
            if name == CODEX_REQUIRED:
                raise HarnessError(f"the eval Codex dir has no {name}. Log in once: "
                                   f"{agents.login_hint('codex', cfg.claude_dir, cfg.codex_dir)}")
            continue
        dst = os.path.join(codex, name)
        shutil.copyfile(src, dst)
        os.chmod(dst, 0o600)


def sync_codex_auth(cfg: Config, hroot: str) -> str | None:
    """Codex may refresh its login during a run and retire the old refresh token. Then the
    run's `auth.json` replaces the eval dir's, atomically. Returns a note, or None."""
    src = os.path.join(run_dirs(hroot)[1], CODEX_REQUIRED)
    dst = os.path.join(cfg.codex_dir, CODEX_REQUIRED)
    try:
        with open(src, "rb") as f:
            new = f.read()
        with open(dst, "rb") as f:
            old = f.read()
    except OSError:
        return None
    if new == old:
        return None
    try:
        if not isinstance(json.loads(new), dict):
            return "the run's codex auth.json changed but is not a JSON object, so it was not kept"
    except ValueError:
        return "the run's codex auth.json changed but is not JSON, so it was not kept"
    tmp = f"{dst}.handoff-{os.getpid()}"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(new)
    os.replace(tmp, dst)
    return "codex refreshed its login; the eval dir's auth.json was updated"


def wiring_hits(hroot: str) -> list[str]:
    """The run's config files that mention atticdb. Codex's `[projects."<path>"]` trust headers
    are skipped, as a path is not wiring."""
    hits = []
    for kind, folder in zip(("claude", "codex", "copilot"), (*run_dirs(hroot), harness_dir(hroot, "copilot"))):
        for name in WIRING_FILES[kind]:
            text = _read(os.path.join(folder, name))
            lines = [ln for ln in text.splitlines()
                     if not (name == "config.toml" and ln.lstrip().startswith("[projects."))]
            if any("atticdb" in ln.lower() for ln in lines):
                hits.append(f"{kind}-dir:{name}")
    return hits


# ---- the commands of 1 run ----

def search_step(cfg: Config, repo: str, phase: str, token: str) -> Step:
    return Step(phase, [cfg.atticdb, "search", "--query", token], repo, agents.ATTICDB_TIMEOUT)


def oracle_state(case: Case, arm: str) -> dict[str, str]:
    """The files the oracle reader's repo gets in place of the writer's work. Other arms get none."""
    return dict(case.oracle_state) if arm == "oracle" else {}


def write_files(repo: str, files: dict[str, str]) -> None:
    """Writes each relative path under `repo`. A path that leaves the repo is refused."""
    real = os.path.realpath(repo)
    for rel, body in files.items():
        path = os.path.realpath(os.path.join(repo, rel))
        if os.path.isabs(rel) or not path.startswith(real + os.sep):
            raise ValueError(f"oracle_state path leaves the repo: {rel!r}")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(body)


def plan(cfg: HandoffConfig, case: Case, arm: str, writer: str, reader: str, root: str,
         hroot: str = "<harness-root>") -> dict[str, list[Step]]:
    """Every child of 1 run, by phase. A dry run prints these; a real run runs them. The copilot
    setup, disconnect and teardown steps get `setup_env`: a HOME in `hroot`."""
    _, repo = paths(root)
    z, git = cfg.atticdb, agents.git_env()
    both = run_harnesses(writer, reader)
    fixture = []
    if arms.instruction_files(arm):
        fixture += [Step("fixture", ["git", "add", *arms.instruction_files(arm)], repo, agents.GIT_TIMEOUT, git),
                    Step("fixture", ["git", "commit", "-q", "--no-verify", "-m", "Handoff instructions"], repo,
                         agents.GIT_TIMEOUT, git)]
    state = oracle_state(case, arm)
    if state:
        fixture += [Step("fixture", ["git", "add", "--", *state], repo, agents.GIT_TIMEOUT, git),
                    Step("fixture", ["git", "commit", "-q", "--no-verify", "-m", "writer work"], repo,
                         agents.GIT_TIMEOUT, git)]
    fixture.append(Step("fixture", ["git", "update-ref", BASE_REF, "HEAD"], repo, agents.GIT_TIMEOUT, git))
    return {
        "fixture": fixture,
        "disconnect": [Step("disconnect", agents.disconnect_argv(z, h), repo, agents.SETUP_TIMEOUT,
                            setup_env(hroot, h)) for h in both],
        "start": [search_step(cfg, repo, "start", t) for t in case.evidence],
        "setup": [Step("setup", agents.setup_argv(z, h), repo, agents.SETUP_TIMEOUT, setup_env(hroot, h))
                  for h in both]
        if arms.uses_atticdb(arm) else [],
        "writer": [Step("writer", arms.session_argv(writer, p, cfg.models.get(writer, ""), arm), repo,
                        cfg.agent_timeout) for p in case.writer_prompts] if arms.has_writer(arm) else [],
        "capture": [search_step(cfg, repo, "capture", t) for t in case.evidence] if arms.has_writer(arm) else [],
        "hook": [Step("hook", agents.hook_argv(z, reader), repo, agents.HOOK_TIMEOUT, stdin=hook_input(repo))]
        if arms.uses_atticdb(arm) else [],
        "reader": [Step("reader", arms.session_argv(reader, arms.reader_prompt(case, arm),
                                                    cfg.models.get(reader, ""), arm), repo, cfg.agent_timeout)],
        "grade": [Step("grade", ["git", "diff", BASE_REF], repo, agents.GIT_TIMEOUT, git)],
        "teardown": [Step("teardown", agents.disconnect_argv(z, h), repo, agents.SETUP_TIMEOUT,
                          setup_env(hroot, h)) for h in both],
    }


def _exec(cfg: Config, runner: Runner, root: str, step: Step, hroot: str) -> Result:
    return runner.run(step.argv, env=env_for(cfg, root, step, hroot), root=root, cwd=step.cwd,
                      timeout=step.timeout, stdin=step.stdin)


def _code(r: Result) -> int:
    return 127 if r.missing else (-1 if r.code is None else r.code)


def _required(cfg: Config, runner: Runner, root: str, step: Step, hroot: str) -> Result:
    r = _exec(cfg, runner, root, step, hroot)
    if r.code != 0:
        what = "timed out" if r.timed_out else f"exited {_code(r)}"
        raise HarnessError(f"{os.path.basename(step.argv[0])} {step.argv[1]} {what}")
    return r


# ---- where tokens are found ----

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
            if has_token(text, token):
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


# Not what an agent wrote down: git data, virtualenvs, caches and vendored packages.
SKIP_DIRS = frozenset({".git", ".venv", "venv", "__pycache__", ".pytest_cache", "node_modules"})


def file_hits(folder: str, tokens, label: str, only: set[str] | None = None,
              skip_files: tuple[str, ...] = ()) -> list[tuple[str, str]]:
    """(label:relpath, token) for each file under `folder` holding a token. SKIP_DIRS and any dir
    holding pyvenv.cfg are skipped, as are links and `skip_files` at the top. With `only`, just
    those relative paths are read."""
    found = []
    if not os.path.isdir(folder):
        return found
    for dirpath, dirnames, filenames in os.walk(folder):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS
                             and not os.path.exists(os.path.join(dirpath, d, "pyvenv.cfg")))
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, folder)
            if (only is not None and rel not in only) or rel in skip_files or os.path.islink(path):
                continue
            found += [(f"{label}:{rel}", t) for t in tokens_in(_read(path), tokens)]
    return found


def claude_project_slug(path: str) -> str:
    """Claude Code names a project dir after the real path, every character that is not a
    letter or digit turned into `-`: `/private/var/x_y.z` gives `-private-var-x-y-z`."""
    return re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(path))


def native_memory_dir(hroot: str, harness: str, repo: str) -> str | None:
    """The harness's own memory for `repo`, in the run's config dirs. None for Copilot CLI: its
    memory is off in prompt mode unless --enable-memory, and `copilot memories` imports to a
    service, not a local store. Its session-store.db is session history, and is scrubbed."""
    claude, codex = run_dirs(hroot)
    if harness == "claude-code":
        return os.path.join(claude, "projects", claude_project_slug(repo), "memory")
    if harness == "codex":
        return os.path.join(codex, "memories")
    if harness == "copilot":
        return None
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


def _content(ev: dict) -> list:
    msg = ev.get("message")
    content = msg.get("content") if isinstance(msg, dict) else None
    return content if isinstance(content, list) else []


def _claude_final(ev: dict) -> bool:
    return ev.get("type") == "assistant" and any(isinstance(b, dict) and b.get("type") == "text"
                                                 for b in _content(ev))


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


# Their results echo what the agent itself wrote, which is not context it received.
CLAUDE_WRITE_TOOLS = frozenset({"Write", "Edit", "MultiEdit", "NotebookEdit"})
CODEX_OUTPUT_FIELDS = {"command_execution": ("aggregated_output", "output", "stdout", "stderr"),
                       "mcp_tool_call": ("result", "error")}
CODEX_LEGACY_OUTPUT = {"exec_command_end": ("aggregated_output", "formatted_output", "stdout", "stderr"),
                       "mcp_tool_call_end": ("result",)}


def _copilot_final(ev: dict) -> bool:
    data = ev.get("data")
    return ev.get("type") == "assistant.message" and isinstance(data, dict) and data.get("phase") == "final_answer"


# Copilot CLI tools whose result echoes what the agent wrote.
COPILOT_WRITE_TOOLS = frozenset({"edit", "create", "write", "str_replace_editor", "apply_patch"})


def copilot_tool_outputs(events: list[dict]) -> list[str]:
    """Copilot: the `result` of each `tool.execution_complete` (`content`, `detailedContent`,
    `contents`), except for write tools, whose names come from the tool requests. The streamed
    `tool.shell_output` and partial results repeat the complete one, so they are not read."""
    names: dict[str, str] = {}
    texts: list[str] = []
    for ev in events:
        data = ev.get("data") if isinstance(ev.get("data"), dict) else {}
        kind = ev.get("type")
        if kind == "assistant.message":
            for req in data.get("toolRequests") or []:
                if isinstance(req, dict) and isinstance(req.get("toolCallId"), str):
                    names[req["toolCallId"]] = str(req.get("name"))
        elif kind == "tool.execution_start" and isinstance(data.get("toolCallId"), str):
            names[data["toolCallId"]] = str(data.get("toolName"))
        elif kind == "tool.execution_complete":
            if names.get(data.get("toolCallId")) in COPILOT_WRITE_TOOLS:
                continue
            result = data.get("result")
            if isinstance(result, dict):
                for key in ("content", "detailedContent", "contents"):
                    texts += _strings(result.get(key))
            elif isinstance(result, str):
                texts.append(result)
    return texts


def tool_outputs(harness: str, stdout: bytes) -> list[str]:
    """The text the reader received from tools before its final reply.

    Claude: `tool_result` content, except for Write and Edit tools, plus a Read's
    `tool_use_result.file.content`. Write and Edit echoes (`content`, `newString`,
    `structuredPatch`) are left out. Codex: command output and MCP results. The commands the
    agent typed are not read. Copilot: see `copilot_tool_outputs`. The agent's own messages,
    the prompt and tool arguments are never read."""
    events = _events(stdout)
    is_final = {"claude-code": _claude_final, "codex": _codex_final, "copilot": _copilot_final}[harness]
    finals = [i for i, ev in enumerate(events) if is_final(ev)]
    texts: list[str] = []
    tool_names: dict[str, str] = {}
    events = events[:finals[-1] if finals else len(events)]
    if harness == "copilot":
        return copilot_tool_outputs(events)
    for ev in events:
        if harness == "claude-code":
            if ev.get("type") == "assistant":
                for block in _content(ev):
                    if isinstance(block, dict) and block.get("type") == "tool_use" and isinstance(block.get("id"), str):
                        tool_names[block["id"]] = str(block.get("name"))
                continue
            if ev.get("type") != "user":
                continue
            for block in _content(ev):
                if (isinstance(block, dict) and block.get("type") == "tool_result"
                        and tool_names.get(block.get("tool_use_id")) not in CLAUDE_WRITE_TOOLS):
                    texts += _strings(block.get("content"))
            result = ev.get("tool_use_result")
            file = result.get("file") if isinstance(result, dict) else None
            if isinstance(file, dict) and isinstance(file.get("content"), str):
                texts.append(file["content"])
        elif ev.get("type") == "item.completed":
            item = _codex_item(ev)
            for name in CODEX_OUTPUT_FIELDS.get(item.get("type") or item.get("item_type"), ()):
                texts += _strings(item.get(name))
        elif isinstance(ev.get("msg"), dict):
            for name in CODEX_LEGACY_OUTPUT.get(ev["msg"].get("type"), ()):
                texts += _strings(ev["msg"].get(name))
    return texts


# ---- Copilot CLI specifics ----

def vscode_mcp_file(cfg: Config) -> str:
    """VS Code's user `mcp.json`, where `atticdb connect copilot` looks: HOME-based on macOS, else
    `$XDG_CONFIG_HOME` or `~/.config`. It mirrors `Harness::file` in crates/atticdb/src/connect.rs."""
    if sys.platform == "darwin":
        return os.path.join(cfg.home, "Library", "Application Support", "Code", "User", "mcp.json")
    xdg = cfg.base_env.get("XDG_CONFIG_HOME", "")
    base = xdg if os.path.isabs(xdg) else os.path.join(cfg.home, ".config")
    return os.path.join(base, "Code", "User", "mcp.json")


def copilot_hook_blocks(hroot: str) -> list[str]:
    """The block of each sessionStart hook that ran and succeeded in the run's COPILOT_HOME, read from
    `session-state/<session>/events.jsonl`: its `hook.end` events. '' for a hook that printed `{}`.
    The JSON stream on stdout has no hook events, so this is where a run sees the hook fire."""
    base = os.path.join(harness_dir(hroot, "copilot"), "session-state")
    blocks = []
    for name in sorted(os.listdir(base)) if os.path.isdir(base) else []:
        path = os.path.join(base, name, "events.jsonl")
        if not os.path.isfile(path):
            continue
        for ev in _events(_read(path).encode()):
            data = ev.get("data")
            if (ev.get("type") == "hook.end" and isinstance(data, dict) and data.get("hookType") == "sessionStart"
                    and data.get("success") is True):
                output = data.get("output")
                text = output.get("additionalContext") if isinstance(output, dict) else None
                blocks.append(text if isinstance(text, str) else "")
    return blocks


_SKILL = re.compile(r"<skill>\s*<name>(.*?)</name>.*?<location>(.*?)</location>", re.S)


def copilot_skills(hroot: str) -> list[tuple[str, str]]:
    """(name, location) of each skill offered to the model in the run's Copilot sessions, from the
    `system.message` events in `COPILOT_HOME/session-state/*/events.jsonl`. Copilot sessions keep
    the real HOME, so personal skills in `~/.copilot/skills` or `~/.agents/skills` could show here."""
    base = os.path.join(harness_dir(hroot, "copilot"), "session-state")
    found: list[tuple[str, str]] = []
    for name in sorted(os.listdir(base)) if os.path.isdir(base) else []:
        for ev in _events(_read(os.path.join(base, name, "events.jsonl")).encode()):
            if ev.get("type") == "system.message":
                found += [(n.strip(), loc.strip()) for n, loc in _SKILL.findall("\n".join(_strings(ev.get("data"))))]
    return list(dict.fromkeys(found))


def copilot_skill_note(hroot: str, who: str) -> str | None:
    """A note naming any skill that is not builtin, or None. Builtin skills ship with the CLI, so
    every Copilot session has them; any other skill would differ from the user's own setup."""
    other = sorted({n for n, loc in copilot_skills(hroot) if loc != "builtin"})
    if not other:
        return None
    shown = ", ".join(other[:5]) + (", ..." if len(other) > 5 else "")
    return f"copilot {who}: {len(other)} non-builtin skills offered ({shown})"


def copilot_hook_note(hroot: str, who: str, sessions: int, tokens) -> str:
    """A note on whether Copilot ran AtticDB's sessionStart hook, and which tokens its block held."""
    blocks = copilot_hook_blocks(hroot)
    found = tokens_in("\n".join(blocks), tokens)
    if not blocks:
        return f"copilot {who}: sessionStart hook did not run (no hook.end in session-state)"
    return (f"copilot {who}: sessionStart hook ran in {len(blocks)} of {sessions} sessions"
            + (f", block has [{','.join(found)}]" if found else ", block has no evidence token"))


# ---- 1 run ----

def _message(value) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        for key in ("message", "error", "detail"):
            if isinstance(value.get(key), str):
                return value[key]
    return None


def stream_errors(harness: str, stdout: bytes) -> list[str]:
    """The errors an agent reported in its JSON stream, first 2, redacted and cut. Codex:
    `turn.failed`, `error`, error items and legacy error messages. Claude: a failed `result`,
    and an `error` on an assistant message. Copilot: `error`, `session.error`, and a `result`
    with a non-zero `exitCode`."""
    found = []
    for ev in _events(stdout):
        kind = ev.get("type")
        text = None
        if harness == "copilot":
            if kind in ("error", "session.error"):
                data = ev.get("data")
                text = _message(data) or _message(ev) or kind
            elif kind == "result" and isinstance(ev.get("exitCode"), int) and ev["exitCode"] != 0:
                text = f"exitCode {ev['exitCode']}"
        elif harness == "codex":
            if kind == "turn.failed":
                text = _message(ev.get("error")) or "turn failed"
            elif kind == "error":
                text = _message(ev) or "error"
            elif kind in ("item.completed", "item.started") and _codex_item(ev).get("type") == "error":
                text = _message(_codex_item(ev))
            elif isinstance(ev.get("msg"), dict) and ev["msg"].get("type") in ("error", "stream_error"):
                text = _message(ev["msg"])
        elif kind == "result" and (ev.get("is_error") or ev.get("subtype") not in (None, "success")):
            text = _message(ev.get("result")) or _message(ev.get("error")) or str(ev.get("subtype"))
        elif kind == "assistant" and ev.get("error"):
            text = _message(ev.get("error")) or str(ev.get("error"))
        if text:
            found.append(guard.excerpt(text, None, 160))
    return list(dict.fromkeys(found))[:2]


def _unfinished(r: Result, out: Outcome, step: Step, harness: str) -> str | None:
    """Why a session did not end cleanly, or None. Errors from the JSON stream come first, as
    stderr often holds only a banner such as "Reading additional input from stdin..."."""
    if agent_finished(r, out):
        return None
    if r.missing:
        return f"{step.argv[0]} is not on PATH"
    errors = [f"stream error: {e}" for e in stream_errors(harness, r.stdout)]
    if r.timed_out:
        why = f"timed out after {step.timeout:g} s"
    elif r.code != 0:
        why = f"exited {r.code}"
        tail = [ln for ln in r.stderr.decode("utf-8", "replace").splitlines() if ln.strip()][-1:]
        errors += [f"stderr: {guard.excerpt(tail[0], None, 160)}"] if tail else []
    elif out.is_error:
        why = "the agent reported an error"
    else:
        why = "no final reply"
    return why + (": " + "; ".join(errors) if errors else "")


def _add(total: int | None, n: int | None) -> int | None:
    return total if n is None else (total or 0) + n


def _session(cfg: HandoffConfig, runner: Runner, root: str, hroot: str, step: Step, harness: str,
             res: HandoffResult, label: str) -> tuple[Result, Outcome]:
    if cfg.budget is not None:
        cfg.budget.take()
    res.sessions += 1
    r = _exec(cfg, runner, root, step, hroot)
    out = PARSERS[harness](r.stdout)
    res.wall_s = (res.wall_s or 0.0) + r.wall_s
    res.input_tokens = _add(res.input_tokens, out.input_tokens)
    res.output_tokens = _add(res.output_tokens, out.output_tokens)
    if out.unparsed:
        res.reasons.append(f"{label} unparsed={out.unparsed}")
    if out.premium_requests is not None:
        res.premium_requests = (res.premium_requests or 0) + out.premium_requests
        res.reasons.append(f"{label} premium_requests={out.premium_requests}")
    why = _unfinished(r, out, step, harness)
    if why:
        raise HarnessError(f"{label} {why}")
    return r, out


def _searches(cfg: Config, runner: Runner, root: str, hroot: str, steps: list[Step]) -> list[tuple[str, str]]:
    found = []
    for step in steps:
        r = _required(cfg, runner, root, step, hroot)
        token = step.argv[step.argv.index("--query") + 1]
        found += [(place, token) for place in store_hits(r.stdout, token)]
    return found


def check_models(cfg: HandoffConfig, *harnesses: str) -> None:
    for h in harnesses:
        if not cfg.models.get(h):
            raise ValueError(f"no model id for {h}. Pass {arms.MODEL_FLAGS[h]}")


def grader_timed_out(verdict) -> bool:
    """The grader's own flag when it has one. grade.py has none yet, so its reason text is read."""
    flag = getattr(verdict, "check_timed_out", None)
    if isinstance(flag, bool):
        return flag
    return any("timed out" in why for why in verdict.reasons)


def run_handoff(cfg: HandoffConfig, runner: Runner, case: Case, arm: str, writer: str, reader: str,
                index: int) -> HandoffResult:
    arms.check_arm(arm)
    check_models(cfg, *((writer, reader) if arms.has_writer(arm) else (reader,)))
    res = HandoffResult(case.id, arm, writer, reader, index, writer_model=cfg.models.get(writer, ""),
                        reader_model=cfg.models[reader])
    root = make_root(cfg)
    try:
        hroot = make_harness_root(cfg)
    except BaseException:
        shutil.rmtree(root, ignore_errors=True)
        raise
    _, repo = paths(root)
    claude_run, codex_run = run_dirs(hroot)
    steps = plan(cfg, case, arm, writer, reader, root, hroot)
    redact_paths = {root: "<root>", os.path.realpath(root): "<root>", hroot: "<harness-root>",
                    os.path.realpath(hroot): "<harness-root>", cfg.claude_dir: "<claude-dir>",
                    cfg.codex_dir: "<codex-dir>", cfg.atticdb: "<atticdb>", cfg.home: "~"}
    stage, touched = "fixture", False
    try:
        try:
            write_fixture(repo)
            write_files(repo, arms.instruction_files(arm))
            write_files(repo, oracle_state(case, arm))
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError) as e:
            raise HarnessError(f"the fixture could not be written: {type(e).__name__}: {e}") from e
        for step in steps["fixture"]:
            _required(cfg, runner, root, step, hroot)

        stage = "setup"
        prepare_config_dirs(cfg, hroot, run_harnesses(writer, reader))
        # Any AtticDB wiring left in the copied config goes first. Outside the atticdb arm, a
        # disconnect that fails may have left it in place, so the run stops.
        touched = True
        for step in steps["disconnect"]:
            r = _exec(cfg, runner, root, step, hroot)
            if r.code != 0:
                if not arms.uses_atticdb(arm):
                    raise HarnessError(f"pre-disconnect {step.argv[-1]} exited {_code(r)}")
                res.reasons.append(f"pre-disconnect {step.argv[-1]} exited {_code(r)}")
        wired = wiring_hits(hroot)
        if wired:
            raise HarnessError(f"harness config mentions atticdb before the run: {', '.join(wired)}")

        stage = "start"
        dirty = _searches(cfg, runner, root, hroot, steps["start"])
        dirty += file_hits(repo, case.evidence, "repo")
        dirty += file_hits(claude_run, case.evidence, "claude-dir")
        dirty += file_hits(codex_run, case.evidence, "codex-dir", skip_files=(CODEX_REQUIRED,))
        dirty += file_hits(harness_dir(hroot, "copilot"), case.evidence, "copilot-dir")
        if dirty:
            raise HarnessError(f"the run did not start clean: {_where(dirty)}")

        stage = "setup"
        if steps["setup"] and "copilot" in run_harnesses(writer, reader):
            # `atticdb connect copilot` drops an atticdb entry it finds in VS Code's mcp.json, which
            # sits under the real HOME. That is the user's own config, so the run stops first.
            vscode = vscode_mcp_file(cfg)
            if "atticdb" in _read(vscode).lower():
                raise HarnessError(f"{vscode} has an atticdb entry, and setting up copilot would remove it. "
                                   "Disconnect VS Code from atticdb before a copilot run")
        for step in steps["setup"]:
            _required(cfg, runner, root, step, hroot)

        if arms.has_writer(arm):
            stage = "writer"
            mem = native_memory_dir(hroot, writer, repo)
            mem_before = guard.snapshot(mem) if mem else {}
            for i, step in enumerate(steps["writer"], 1):
                _session(cfg, runner, root, hroot, step, writer, res, f"writer {i}")
            if writer == "copilot":
                if arms.uses_atticdb(arm):
                    res.reasons.append(copilot_hook_note(hroot, "writer", len(steps["writer"]), case.evidence))
                res.reasons += filter(None, [copilot_skill_note(hroot, "writer")])

            stage = "capture"
            found = _searches(cfg, runner, root, hroot, steps["capture"])
            found += file_hits(repo, case.evidence, "repo")
            if mem:
                label = "claude-memory" if writer == "claude-code" else "codex-memory"
                found += file_hits(mem, case.evidence, label, only=changed_files(mem_before, guard.snapshot(mem)))
            res.captured, res.captured_where = bool(found), _where(found)
            # The writer's transcripts and sessions hold its prompts. The reader gets the writer's
            # config dir in its env, so all but the login goes before the reader starts.
            try:
                scrub_harness_dir(hroot, writer)
            except OSError as e:
                raise HarnessError(f"could not clear the {writer} dir before the reader: {e}") from e

        delivered: list[tuple[str, str]] = []
        if arms.uses_atticdb(arm):
            stage = "hook"
            (hook,) = steps["hook"]
            r = _exec(cfg, runner, root, hook, hroot)
            if r.code != 0:
                res.reasons.append(f"hook exited {_code(r)}")
            delivered += [("hook", t) for t in tokens_in(hook_block(r.stdout), case.evidence)]
        if arm == "oracle":
            delivered += [("prompt", t) for t in tokens_in(arms.reader_prompt(case, arm), case.evidence)]

        stage = "reader"
        (step,) = steps["reader"]
        r, out = _session(cfg, runner, root, hroot, step, reader, res, "reader")
        if reader == "copilot":
            if arms.uses_atticdb(arm):
                res.reasons.append(copilot_hook_note(hroot, "reader", 1, case.evidence))
            res.reasons += filter(None, [copilot_skill_note(hroot, "reader")])
        outputs = "\n".join(tool_outputs(reader, r.stdout))
        delivered += [("transcript", t) for t in tokens_in(outputs, case.evidence)]
        if case.id not in NO_DELIVERY:
            res.delivered, res.delivered_where = bool(delivered), _where(delivered)

        stage = "grade"
        (diff_step,) = steps["grade"]
        d = _exec(cfg, runner, root, diff_step, hroot)
        if d.code != 0:
            res.reasons.append(f"git diff exited {_code(d)}")
        try:
            verdict = grade(case, out.reply, d.stdout.decode("utf-8", "replace"), repo)
        except (OSError, ValueError) as e:
            raise HarnessError(f"the grader failed: {e}") from e
        res.passed = verdict.passed
        res.reasons.extend(verdict.reasons)
        res.check_timed_out = grader_timed_out(verdict)
        if res.check_timed_out and not verdict.passed:
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
                r = _exec(cfg, runner, root, step, hroot)
                if r.code != 0:
                    res.reasons.append(f"disconnect {step.argv[-1]} exited {_code(r)}")
        note = sync_codex_auth(cfg, hroot)
        if note:
            res.reasons.append(note)
        shutil.rmtree(hroot, ignore_errors=True)
        problem = stop_server(cfg, runner, root)
        if problem:
            res.fail("teardown", problem)
        else:
            shutil.rmtree(root, ignore_errors=True)


def describe(cfg: HandoffConfig, case: Case, arm: str, writer: str, reader: str, root: str = "<root>",
             hroot: str = "<harness-root>") -> list[str]:
    """The dry-run lines of 1 run: each command, its cwd and timeout, and its env by name only."""
    zh, repo = paths(root)
    claude, codex = run_dirs(hroot)
    copilot = harness_dir(hroot, "copilot")
    used = run_harnesses(writer, reader)
    extra = ", ".join(arms.instruction_files(arm))
    lines = [f"  write the ledger fixture into {repo} and commit it" + (f", then write {extra}" if extra else "")]
    state = oracle_state(case, arm)
    if state:
        lines.append(f"  write the case's oracle_state ({', '.join(state)}) as the writer's work")
    for phase, steps in plan(cfg, case, arm, writer, reader, root, hroot).items():
        if phase == "disconnect":
            codex_note = (f"auth.json and config.toml copied from {cfg.codex_dir}" if "codex" in used
                          else "empty, as codex is not in this run")
            lines.append(f"  [setup] make {claude} (empty), {codex} ({codex_note}) and {copilot} "
                         "(empty: Copilot logs in from the OS keychain)")
        if phase == "start":
            stop = "any failure stops the run" if not arms.uses_atticdb(arm) else "a failure is noted"
            lines.append(f"  [setup] the disconnects above: {stop}. Then any atticdb mention in "
                         f"{', '.join(WIRING_FILES['claude'] + WIRING_FILES['codex'] + WIRING_FILES['copilot'])} "
                         "stops the run")
            lines.append(f"  [start] scan {repo}, {claude}, {codex} and {copilot} for {', '.join(case.evidence)}; "
                         "any hit stops the run")
        if phase == "setup" and steps and "copilot" in used:
            lines.append("  [setup] first, an atticdb entry in the real VS Code user mcp.json stops the run, as a "
                         f"backstop. Copilot's setup and disconnects run with HOME={setup_home(hroot)} and "
                         f"XDG_CONFIG_HOME={setup_home(hroot)}/.config, so they only see a per-run VS Code mcp.json")
        if phase == "capture" and steps:
            memory = (f"the files {writer} memory changed during the run" if writer != "copilot"
                      else "no copilot memory (off in prompt mode)")
            lines.append(f"  [capture] also scan {repo} and {memory}")
            keep = ", ".join(LOGIN_FILES[writer])
            lines.append(f"  [capture] then delete {'all but ' + keep if keep else 'everything'} in the {writer} dir")
        for step in steps:
            env = f" + {', '.join(sorted(step.extra_env))}" if step.extra_env else ""
            stdin = " <stdin: hook JSON>" if step.stdin is not None else ""
            session = " (agent session)" if phase in ("writer", "reader") else ""
            lines.append(f"  [{phase}]{session} cd {step.cwd} && {shlex.join(step.argv)}{stdin}")
            lines.append(f"      timeout {step.timeout:g} s; env: base{env}")
    refresh = ", copy a refreshed codex auth.json back" if "codex" in used else ""
    lines.append(f"  grade with grade.grade{refresh}, delete {hroot}, stop the atticdb "
                 f"server named in {zh}/data/LOCK, then delete {root}")
    return lines
