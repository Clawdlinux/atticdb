"""The commands each run starts, and the env they get. Dry runs print these same lists."""

from __future__ import annotations

import os

HARNESSES = ("claude-code", "codex")

# Eval-only overrides, listed in the results.
CLAUDE_ALLOWED = "mcp__atticdb__fact_set"
CODEX_APPROVE = 'mcp_servers.atticdb.tools.fact_set.approval_mode="approve"'
OVERRIDES = {
    "claude-code": [f"--allowedTools={CLAUDE_ALLOWED}: fact_set runs without asking, as nobody can answer a prompt",
                    "--permission-mode acceptEdits: file edits run without asking, so S4 tests memory, "
                    "not the permission prompt"],
    "codex": ["--dangerously-bypass-hook-trust: the atticdb hook runs without a stored approval",
              f"-c '{CODEX_APPROVE}': fact_set runs without asking",
              "--sandbox workspace-write: a fresh Codex config has no trust entry for the fixture, so "
              "Codex 0.147 would pick read-only. This keeps the plan's default workspace-write sandbox, which "
              "still tests that the MCP server reaches the home socket"],
}

AGENT_TIMEOUT = 300.0
SETUP_TIMEOUT = 120.0
ATTICDB_TIMEOUT = 30.0
GIT_TIMEOUT = 30.0
HOOK_TIMEOUT = 15.0
CHECK_TIMEOUT = 20.0

# Never passed on. A token or another home would point the children at the user's store.
# An inherited GIT_DIR, GIT_WORK_TREE or GIT_INDEX_FILE would point git at a real repo.
# The harness's own git steps get their GIT_* vars back through `extra`.
DROPPED_PREFIXES = ("ATTICDB_", "GIT_")
DROPPED = {"CLAUDE_CONFIG_DIR", "CODEX_HOME"}
# Credentials, and the switches that pick another provider or endpoint. Dropped unless
# --keep-api-keys, so runs use the logins in the eval dirs and nothing else.
ACCOUNT_ENV = ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_BASE_URL",
               "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "OPENAI_API_KEY", "CODEX_API_KEY",
               "OPENAI_BASE_URL", "OPENAI_ORG_ID", "CLAUDE_CODE_USE_FOUNDRY")
# Every var with these prefixes too, such as ANTHROPIC_FOUNDRY_API_KEY or _RESOURCE.
ACCOUNT_PREFIXES = ("ANTHROPIC_FOUNDRY_",)


def agent_argv(harness: str, prompt: str) -> list[str]:
    """The headless agent command. Claude's `--allowedTools` is variadic and would swallow
    a prompt or flag value after it, so the prompt comes right after `-p`, and the flag uses
    the `--flag=value` form, last."""
    if harness == "claude-code":
        return ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
                "--permission-mode", "acceptEdits", f"--allowedTools={CLAUDE_ALLOWED}"]
    if harness == "codex":
        return ["codex", "exec", "--json", "--dangerously-bypass-hook-trust", "-c", CODEX_APPROVE,
                "--sandbox", "workspace-write", "--skip-git-repo-check", prompt]
    raise ValueError(f"unknown harness {harness!r}")


def check_argv(harness: str) -> list[str]:
    return ["claude", "auth", "status"] if harness == "claude-code" else ["codex", "login", "status"]


def login_hint(harness: str, claude_dir: str, codex_dir: str) -> str:
    if harness == "claude-code":
        return f"CLAUDE_CONFIG_DIR={claude_dir} claude auth login"
    return f"CODEX_HOME={codex_dir} codex login"


def setup_argv(atticdb: str, harness: str) -> list[str]:
    # 1 harness at a time: connect vscode and copilot must never run.
    if harness not in HARNESSES:
        raise ValueError(f"unknown harness {harness!r}")
    return [atticdb, "setup", "--yes", "--harness", harness]


def disconnect_argv(atticdb: str, harness: str) -> list[str]:
    if harness not in HARNESSES:
        raise ValueError(f"unknown harness {harness!r}")
    return [atticdb, "disconnect", harness]


def hook_argv(atticdb: str, harness: str) -> list[str]:
    return [atticdb, "hook", "session-start", "--harness", harness]


def stripped_keys(base: dict, keep_api_keys: bool = False) -> list[str]:
    """The names of the credential and provider vars in `base` that the children won't get."""
    if keep_api_keys:
        return []
    return sorted(k for k in base if k in ACCOUNT_ENV or k.startswith(ACCOUNT_PREFIXES))


def account_names() -> str:
    """The stripped names as text, prefixes as `NAME_*`."""
    return ", ".join(list(ACCOUNT_ENV) + [f"{p}*" for p in ACCOUNT_PREFIXES])


def child_env(base: dict, *, home: str, atticdb_home: str, claude_dir: str, codex_dir: str,
              extra: dict | None = None, keep_api_keys: bool = False) -> dict:
    """`base` without any `ATTICDB_*`, `GIT_*`, `CLAUDE_CONFIG_DIR` or `CODEX_HOME`.
    The credential and provider vars go too, unless `keep_api_keys`.
    Then it adds the run's own vars, then `extra`."""
    dropped = DROPPED | set(stripped_keys(base, keep_api_keys))
    env = {k: v for k, v in base.items() if k not in dropped and not k.startswith(DROPPED_PREFIXES)}
    env.update({"HOME": home, "ATTICDB_HOME": atticdb_home, "CLAUDE_CONFIG_DIR": claude_dir,
                "CODEX_HOME": codex_dir})
    env.update(extra or {})
    return env


def git_env() -> dict:
    """For the harness's own git calls: no user or system config, so no hooks or signing."""
    return {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "eval",
            "GIT_AUTHOR_EMAIL": "eval@example.invalid", "GIT_COMMITTER_NAME": "eval",
            "GIT_COMMITTER_EMAIL": "eval@example.invalid", "GIT_TERMINAL_PROMPT": "0"}
