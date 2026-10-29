"""The commands each run starts, and the env they get. Dry runs print these same lists."""

from __future__ import annotations

import os

HARNESSES = ("claude-code", "codex")

# Eval-only overrides, listed in the results.
CLAUDE_ALLOWED = "mcp__zerodb__fact_set"
CODEX_APPROVE = 'mcp_servers.zerodb.tools.fact_set.approval_mode="approve"'
OVERRIDES = {
    "claude-code": [f"--allowedTools={CLAUDE_ALLOWED}: fact_set runs without asking, as nobody can answer a prompt",
                    "--permission-mode acceptEdits: file edits run without asking, so S4 tests memory, "
                    "not the permission prompt"],
    "codex": ["--dangerously-bypass-hook-trust: the zerodb hook runs without a stored approval",
              f"-c '{CODEX_APPROVE}': fact_set runs without asking",
              "the default workspace-write sandbox is kept"],
}

AGENT_TIMEOUT = 300.0
SETUP_TIMEOUT = 120.0
ZERODB_TIMEOUT = 30.0
GIT_TIMEOUT = 30.0
HOOK_TIMEOUT = 15.0
CHECK_TIMEOUT = 20.0

# Never passed on: a token or another home would point the children at the user's store.
DROPPED_PREFIXES = ("ZERODB_",)
DROPPED = {"CLAUDE_CONFIG_DIR", "CODEX_HOME"}
# Dropped unless --keep-api-keys, so the agents bill the logins in the eval dirs.
API_KEYS = ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY", "CODEX_API_KEY")


def agent_argv(harness: str, prompt: str) -> list[str]:
    """The headless agent command. Claude's `--allowedTools` is variadic and would swallow
    a prompt or flag value after it, so the prompt comes right after `-p`, and the flag uses
    the `--flag=value` form, last."""
    if harness == "claude-code":
        return ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
                "--permission-mode", "acceptEdits", f"--allowedTools={CLAUDE_ALLOWED}"]
    if harness == "codex":
        return ["codex", "exec", "--json", "--dangerously-bypass-hook-trust", "-c", CODEX_APPROVE,
                "--skip-git-repo-check", prompt]
    raise ValueError(f"unknown harness {harness!r}")


def check_argv(harness: str) -> list[str]:
    return ["claude", "auth", "status"] if harness == "claude-code" else ["codex", "login", "status"]


def login_hint(harness: str, claude_dir: str, codex_dir: str) -> str:
    if harness == "claude-code":
        return f"CLAUDE_CONFIG_DIR={claude_dir} claude auth login"
    return f"CODEX_HOME={codex_dir} codex login"


def setup_argv(zerodb: str, harness: str) -> list[str]:
    # 1 harness at a time: connect vscode and copilot must never run.
    if harness not in HARNESSES:
        raise ValueError(f"unknown harness {harness!r}")
    return [zerodb, "setup", "--yes", "--harness", harness]


def disconnect_argv(zerodb: str, harness: str) -> list[str]:
    if harness not in HARNESSES:
        raise ValueError(f"unknown harness {harness!r}")
    return [zerodb, "disconnect", harness]


def hook_argv(zerodb: str, harness: str) -> list[str]:
    return [zerodb, "hook", "session-start", "--harness", harness]


def stripped_keys(base: dict, keep_api_keys: bool = False) -> list[str]:
    """The names of the API keys in `base` that the children won't get."""
    return [] if keep_api_keys else [k for k in API_KEYS if k in base]


def child_env(base: dict, *, home: str, zerodb_home: str, claude_dir: str, codex_dir: str,
              extra: dict | None = None, keep_api_keys: bool = False) -> dict:
    """`base` without any `ZERODB_*`, `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, and without the API
    keys unless `keep_api_keys`, then the run's own."""
    dropped = DROPPED | set(stripped_keys(base, keep_api_keys))
    env = {k: v for k, v in base.items() if k not in dropped and not k.startswith(DROPPED_PREFIXES)}
    env.update({"HOME": home, "ZERODB_HOME": zerodb_home, "CLAUDE_CONFIG_DIR": claude_dir,
                "CODEX_HOME": codex_dir})
    env.update(extra or {})
    return env


def git_env() -> dict:
    """For the harness's own git calls: no user or system config, so no hooks or signing."""
    return {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "eval",
            "GIT_AUTHOR_EMAIL": "eval@example.invalid", "GIT_COMMITTER_NAME": "eval",
            "GIT_COMMITTER_EMAIL": "eval@example.invalid", "GIT_TERMINAL_PROMPT": "0"}
