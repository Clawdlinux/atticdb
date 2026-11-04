"""The 4 arms, and the agent commands each one starts.

- native: repo files only.
- files: AGENTS.md and CLAUDE.md in the fixture ask the agent to keep HANDOFF.md. Nothing is pre-filled.
- atticdb: `atticdb setup --yes --harness <h>` for both harnesses before the writer, disconnect after.
- oracle: no writer session. The reader prompt starts with the case's gold note.
"""

from __future__ import annotations

from ..setup_eval import agents
from .cases import Case

ARMS = ("native", "files", "atticdb", "oracle")

HANDOFF_RULE = ("Keep HANDOFF.md at the repo root. After each task, record decisions, their reasons, rejected "
                "approaches and constraints. Read it before you start.")
# Written into the fixture for the files arm, and committed with it, so they are not part of any diff.
INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md")
ORACLE_PREFIX = "Notes from the last session: "
# The CLI flag that names each harness's model. Each is required only when a direction uses it.
MODEL_FLAGS = {"claude-code": "--claude-model", "codex": "--codex-model", "copilot": "--copilot-model"}


def check_arm(arm: str) -> None:
    if arm not in ARMS:
        raise ValueError(f"unknown arm {arm!r}; use {','.join(ARMS)}")


def has_writer(arm: str) -> bool:
    check_arm(arm)
    return arm != "oracle"


def uses_atticdb(arm: str) -> bool:
    check_arm(arm)
    return arm == "atticdb"


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

    Outside the atticdb arm, Codex loses the `-c mcp_servers.atticdb...approval_mode` override.
    With no atticdb server configured, that override alone makes a partial server table, and
    Codex 0.147 refuses to load its config ("invalid transport in mcp_servers.atticdb")."""
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
    return argv[:-1] + ["-m", model, prompt]
