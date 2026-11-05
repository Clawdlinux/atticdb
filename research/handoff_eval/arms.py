"""The 6 arms, and the agent commands each one starts.

- native: repo files only.
- files: AGENTS.md and CLAUDE.md in the fixture ask the agent to keep HANDOFF.md. Nothing is pre-filled.
- atticdb: `atticdb setup --yes --harness <h>` for both harnesses before the writer, disconnect after.
- oracle: no writer session. The reader prompt starts with the case's gold note.
- atticdb-only: the atticdb arm with each harness's own memory off. It separates AtticDB from
  that memory. Any native memory write in it is flagged.
- blank: no writer session and no notes. It's a control. Its pass rate is the chance floor.
  Any evidence token the reader sees or says is a leak.
"""

from __future__ import annotations

from ..setup_eval import agents
from .cases import Case

ARMS = ("native", "files", "atticdb", "oracle", "atticdb-only", "blank")
# What a batch runs without --arms. The blank control runs only when asked for.
DEFAULT_ARMS = ("native", "files", "atticdb", "oracle")

HANDOFF_RULE = ("Keep HANDOFF.md at the repo root. After each task, record decisions, their reasons, rejected "
                "approaches and constraints. Read it before you start.")
# Written into the fixture for the files arm, and committed with it, so they are not part of any diff.
INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md")
ORACLE_PREFIX = "Notes from the last session: "
# The CLI flag that names each harness's model. Each is required only when a direction uses it.
MODEL_FLAGS = {"claude-code": "--claude-model", "codex": "--codex-model", "copilot": "--copilot-model"}
# The memory-off switches, checked 2026-11-05. Copilot needs none: its memory is off in prompt
# mode unless --enable-memory, which no arm passes.
CLAUDE_MEMORY_OFF_ENV = {"CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1"}
CODEX_MEMORY_OFF_ARGS = ["--disable", "memories"]


def check_arm(arm: str) -> None:
    if arm not in ARMS:
        raise ValueError(f"unknown arm {arm!r}; use {','.join(ARMS)}")


def has_writer(arm: str) -> bool:
    check_arm(arm)
    return arm not in ("oracle", "blank")


def uses_atticdb(arm: str) -> bool:
    check_arm(arm)
    return arm in ("atticdb", "atticdb-only")


def native_memory_off(arm: str) -> bool:
    """Whether each harness's own memory is switched off for the arm's agent sessions."""
    check_arm(arm)
    return arm == "atticdb-only"


def session_env(harness: str, arm: str) -> dict[str, str]:
    """The extra env of 1 agent session. Claude Code's memory goes off by env."""
    if native_memory_off(arm) and harness == "claude-code":
        return dict(CLAUDE_MEMORY_OFF_ENV)
    return {}


def instruction_files(arm: str) -> dict[str, str]:
    """The extra fixture files for `arm`, by name."""
    check_arm(arm)
    return {name: HANDOFF_RULE + "\n" for name in INSTRUCTION_FILES} if arm == "files" else {}


def reader_prompt(case: Case, arm: str) -> str:
    check_arm(arm)
    if arm == "oracle":
        return f"{ORACLE_PREFIX}{case.gold_note}\n\n{case.reader_prompt}"
    return case.reader_prompt


def writer_sessions(case: Case, arm: str) -> int:
    return len(case.writer_prompts) if has_writer(arm) else 0


def planned_sessions(case: Case, arm: str) -> int:
    """Agent sessions 1 run starts: its writers, then 1 reader."""
    return writer_sessions(case, arm) + 1


def session_argv(harness: str, prompt: str, model: str, arm: str) -> list[str]:
    """`agents.agent_argv` with the model flag. Claude and Copilot get `--model <id>` appended. Codex
    gets `-m <id>` just before the prompt, which `codex exec` takes as its last positional argument.

    Outside the arms that use AtticDB, Codex loses the `-c mcp_servers.atticdb...approval_mode` override.
    With no atticdb server configured, that override alone makes a partial server table, and
    Codex 0.147 refuses to load its config ("invalid transport in mcp_servers.atticdb").

    In the atticdb-only arm, Codex gets `--disable memories` just before `-m <id>`."""
    if not model:
        raise ValueError(f"no model id for {harness}")
    argv = agents.agent_argv(harness, prompt)
    if harness in ("claude-code", "copilot"):
        return argv + ["--model", model]
    if not uses_atticdb(arm):
        i = argv.index(agents.CODEX_APPROVE)
        if argv[i - 1] != "-c":
            raise ValueError("unexpected codex argv shape")
        del argv[i - 1:i + 1]
    if argv[-1] != prompt:
        raise ValueError("unexpected codex argv shape: the prompt is not last")
    off = CODEX_MEMORY_OFF_ARGS if native_memory_off(arm) else []
    return argv[:-1] + off + ["-m", model, prompt]
