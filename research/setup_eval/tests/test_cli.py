import os
import subprocess
import tempfile

import pytest

from research.setup_eval import agents, cli, proc
from research.setup_eval import harness as harness_mod
from research.setup_eval.scenarios import SCENARIOS

from .helpers import ATTICDB, fake_home, stub_calls


def test_claude_argv_puts_the_prompt_before_the_variadic_flag():
    prompt = SCENARIOS["S1"].prompt
    argv = agents.agent_argv("claude-code", prompt)
    assert argv == ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
                    "--permission-mode", "acceptEdits", "--allowedTools=mcp__atticdb__fact_set"]
    allowed = [i for i, a in enumerate(argv) if a.startswith("--allowedTools") or a.startswith("--allowed-tools")]
    assert allowed == [len(argv) - 1], "1 allow flag, last, so it can swallow nothing"
    assert argv.index(prompt) == 2 < allowed[0]
    assert "=" in argv[-1], "the --flag=value form takes exactly 1 value"
    mode = argv.index("--permission-mode")
    assert argv[mode + 1] == "acceptEdits" and argv.index(prompt) < mode < allowed[0], \
        "before the variadic flag, so acceptEdits is its value and not a tool name"


def test_codex_argv():
    prompt = SCENARIOS["S3"].prompt
    assert agents.agent_argv("codex", prompt) == [
        "codex", "exec", "--json", "--dangerously-bypass-hook-trust", "-c",
        'mcp_servers.atticdb.tools.fact_set.approval_mode="approve"', "--sandbox", "workspace-write",
        "--skip-git-repo-check", prompt]
    argv = agents.agent_argv("codex", prompt)
    # Explicit, as a fresh config without a trust entry would get read-only. Never a wider one.
    assert argv[argv.index("--sandbox") + 1] == "workspace-write"
    assert not any(a == "-s" or "danger-full-access" in a or "bypass-approvals-and-sandbox" in a for a in argv)
    assert argv[-1] == prompt


def test_setup_names_1_harness_only():
    for h in agents.HARNESSES:
        assert agents.setup_argv("/z", h) == ["/z", "setup", "--yes", "--harness", h]
    for bad in ("vscode", "copilot", "claude-code,codex", "both"):
        with pytest.raises(ValueError):
            agents.setup_argv("/z", bad)
        with pytest.raises(ValueError):
            agents.disconnect_argv("/z", bad)


def test_child_env_drops_the_users_atticdb_and_config_vars():
    base = {"PATH": "/bin", "ATTICDB_TOKEN": "t", "ATTICDB_HOME": "/real", "CLAUDE_CONFIG_DIR": "/c",
            "CODEX_HOME": "/x", "ATTICDB_SETUP_INTERACTIVE": "1"}
    env = agents.child_env(base, home="/h", atticdb_home="/r/zh", claude_dir="/e/claude", codex_dir="/e/codex")
    assert env == {"PATH": "/bin", "HOME": "/h", "ATTICDB_HOME": "/r/zh", "CLAUDE_CONFIG_DIR": "/e/claude",
                   "CODEX_HOME": "/e/codex"}


def test_credential_and_provider_vars_are_stripped_unless_kept():
    exact = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK",
             "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY", "ANTHROPIC_BASE_URL", "OPENAI_API_KEY",
             "CODEX_API_KEY", "OPENAI_BASE_URL", "OPENAI_ORG_ID"]
    assert sorted(agents.ACCOUNT_ENV) == sorted(exact)
    assert agents.ACCOUNT_PREFIXES == ("ANTHROPIC_FOUNDRY_",)
    named = exact + ["ANTHROPIC_FOUNDRY_API_KEY", "ANTHROPIC_FOUNDRY_RESOURCE", "ANTHROPIC_FOUNDRY_BASE_URL"]
    base = {"PATH": "/bin", "OTHER": "e", "ANTHROPIC_FOUNDRYX": "not the prefix", **{k: "v" for k in named}}
    kw = dict(home="/h", atticdb_home="/r/zh", claude_dir="/e/claude", codex_dir="/e/codex")
    env = agents.child_env(base, **kw)
    assert not any(k in env for k in named) and env["OTHER"] == "e" and env["ANTHROPIC_FOUNDRYX"]
    assert agents.stripped_keys(base) == sorted(named)
    kept = agents.child_env(base, keep_api_keys=True, **kw)
    assert all(kept[k] == "v" for k in named)
    assert agents.stripped_keys(base, keep_api_keys=True) == []


def test_dry_run_names_stripped_keys_never_values(monkeypatch, tmp_path, capsys):
    fake_home(monkeypatch, tmp_path)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-value-1")
    monkeypatch.setenv("CODEX_API_KEY", "codex-value-2")
    monkeypatch.setenv("ANTHROPIC_FOUNDRY_API_KEY", "foundry-value-3")
    base = ["run", "--atticdb", "/z", "--out", str(tmp_path / "o.csv"), "--dry-run"]
    assert cli.main(base) == 0
    text = capsys.readouterr().out
    assert ("Credential and provider vars stripped from every child: ANTHROPIC_API_KEY, ANTHROPIC_FOUNDRY_API_KEY, "
            "CODEX_API_KEY.") in text
    assert "CLAUDE_CODE_USE_FOUNDRY, ANTHROPIC_FOUNDRY_*." in text, "the checked names, prefixes as NAME_*"
    assert "sk-ant-value-1" not in text and "codex-value-2" not in text and "foundry-value-3" not in text
    assert cli.main(base + ["--keep-api-keys"]) == 0
    text = capsys.readouterr().out
    assert "Credential and provider vars: kept (--keep-api-keys)" in text and "sk-ant-value-1" not in text


def test_children_get_no_api_keys(monkeypatch, tmp_path):
    paths = fake_home(monkeypatch, tmp_path)
    for name in agents.ACCOUNT_ENV + ("ANTHROPIC_FOUNDRY_API_KEY",):
        monkeypatch.setenv(name, "v")
    assert cli.main(["check"]) == 0
    assert all(c["api_keys"] == [] for c in stub_calls(paths["log"]))


def test_budget_guard(monkeypatch, tmp_path, capsys):
    fake_home(monkeypatch, tmp_path)
    base = ["run", "--atticdb", "/z", "--out", str(tmp_path / "o.csv"), "--dry-run"]
    assert cli.main(base) == 0, "both harnesses x 6 scenarios = 12, the default cap"
    with pytest.raises(SystemExit) as e:
        cli.main(base + ["--runs", "2"])
    assert e.value.code == 2
    assert "24 runs, over --max-runs 12" in capsys.readouterr().err
    assert cli.main(base + ["--runs", "2", "--max-runs", "24"]) == 0
    with pytest.raises(SystemExit):
        cli.main(base + ["--runs", "3", "--max-runs", "37"])
    assert "at most 36" in capsys.readouterr().err


def test_dry_run_runs_nothing(monkeypatch, tmp_path, capsys):
    paths = fake_home(monkeypatch, tmp_path)
    tmp_parent = tmp_path / "t"
    tmp_parent.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_parent))
    started = []
    monkeypatch.setattr(proc.Runner, "run", lambda *a, **k: started.append(a))
    monkeypatch.setattr(subprocess, "Popen", lambda *a, **k: started.append(a))
    out = tmp_path / "o.csv"
    assert cli.main(["run", "--atticdb", ATTICDB, "--out", str(out), "--dry-run"]) == 0
    text = capsys.readouterr().out
    assert started == [] and stub_calls(paths["log"]) == []
    assert not out.exists() and os.listdir(tmp_parent) == []
    assert os.listdir(paths["claude_dir"]) == [] and os.listdir(paths["codex_dir"]) == []
    assert text.count("\nrun 1 ") == 12
    assert "claude -p 'How do I run this repo'\"'\"'s tests? 1 line.' --output-format stream-json" in text
    assert "setup --yes --harness codex" in text and "setup --yes --harness claude-code" in text
    assert "--harness vscode" not in text and "--harness copilot" not in text
    for secret_place in (paths["home"], paths["claude_dir"]):
        assert secret_place not in text, "env values are not printed"


def test_run_refuses_without_eval_dirs(monkeypatch, tmp_path, capsys):
    paths = fake_home(monkeypatch, tmp_path, make_eval_dirs=False)
    assert cli.main(["run", "--atticdb", "/bin/sh", "--out", str(tmp_path / "o.csv")]) == 2
    assert "claude auth login" in capsys.readouterr().err
    assert stub_calls(paths["log"]) == []


def test_run_refuses_a_missing_atticdb(monkeypatch, tmp_path, capsys):
    fake_home(monkeypatch, tmp_path)
    assert cli.main(["run", "--atticdb", str(tmp_path / "nope"), "--out", str(tmp_path / "o.csv")]) == 2
    assert "cargo build -p atticdb" in capsys.readouterr().err


def test_check(monkeypatch, tmp_path, capsys):
    paths = fake_home(monkeypatch, tmp_path)
    assert cli.main(["check"]) == 0
    text = capsys.readouterr().out
    assert "claude-code: logged in" in text and "codex: logged in" in text
    calls = stub_calls(paths["log"])
    assert [(c["prog"], c["argv"]) for c in calls] == [("claude", ["auth", "status"]), ("codex", ["login", "status"])]
    assert all(c["atticdb_home"] for c in calls)
    monkeypatch.setenv("STUB_LOGGED_OUT", "1")
    assert cli.main(["check"]) == 1
    assert "not logged in (exit 1)" in capsys.readouterr().out


def test_check_reports_a_missing_dir(monkeypatch, tmp_path, capsys):
    paths = fake_home(monkeypatch, tmp_path, make_eval_dirs=False)
    assert cli.main(["check"]) == 1
    text = capsys.readouterr().out
    assert "CLAUDE_CONFIG_DIR=" in text and "codex login" in text
    assert stub_calls(paths["log"]) == []


def test_batch_warns_on_changes_to_user_config_and_the_parent_atticdb_home(monkeypatch, tmp_path):
    paths = fake_home(monkeypatch, tmp_path)
    home = paths["home"]
    os.makedirs(os.path.join(home, ".claude"))
    parent_home = tmp_path / "parent-zh"
    (parent_home / "data").mkdir(parents=True)
    (parent_home / "data" / "seg").write_text("1")
    monkeypatch.setenv("ATTICDB_HOME", str(parent_home))
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--atticdb", "/bin/sh",
                   "--out", str(tmp_path / "o.csv")])

    def fake_run(cfg, runner, harness, scenario, n):
        with open(os.path.join(home, ".claude", "settings.json"), "w") as f:
            f.write("{}")
        (parent_home / "data" / "seg").write_text("22")
        return harness_mod.RunResult(harness, scenario.id, n)

    monkeypatch.setattr(cli, "run_one", fake_run)
    lines = []
    assert cli.batch(a, cli.config(a.atticdb), out=lines.append) == 0
    text = "\n".join(lines)
    assert "WARNING: 1 files in $ATTICDB_HOME/data of the parent env changed during the batch (seg)" in text
    # The new file, and the mtime of the dir that holds it.
    assert ("WARNING: 2 entries at the top level of ~/.claude*, ~/.codex or ~/.copilot changed during the "
            "batch (~/.claude, ~/.claude/settings.json)") in text
    assert "The real atticdb home's data dir did not change" in text
    assert str(parent_home) not in text

    lines.clear()
    monkeypatch.setattr(cli, "run_one", lambda cfg, runner, h, s, n: harness_mod.RunResult(h, s.id, n))
    assert cli.batch(a, cli.config(a.atticdb), out=lines.append) == 0
    assert "WARNING" not in "\n".join(lines)
    assert "The top level of ~/.claude*, ~/.codex and ~/.copilot did not change" in "\n".join(lines)


def test_inherited_git_vars_are_dropped_before_extra():
    base = {"PATH": "/bin", "GIT_DIR": "/real/.git", "GIT_WORK_TREE": "/real", "GIT_INDEX_FILE": "/real/.git/index",
            "GIT_SSH_COMMAND": "ssh -i k", "GIT_CONFIG_GLOBAL": "/real/gitconfig", "GITHUB_TOKEN": "kept-by-name"}
    kw = dict(home="/h", atticdb_home="/r/zh", claude_dir="/e/claude", codex_dir="/e/codex")
    env = agents.child_env(base, **kw)
    assert not any(k.startswith("GIT_") for k in env), "no inherited GIT_* reaches any child"
    assert env["GITHUB_TOKEN"] == "kept-by-name", "only the GIT_ prefix is dropped"
    env = agents.child_env(base, extra=agents.git_env(), **kw)
    assert env["GIT_CONFIG_GLOBAL"] == os.devnull and "GIT_DIR" not in env and "GIT_WORK_TREE" not in env


def test_fake_home_overrides_the_users_config_locations(monkeypatch, tmp_path):
    for name in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "COPILOT_HOME"):
        monkeypatch.setenv(name, "/Users/someone/real")
    paths = fake_home(monkeypatch, tmp_path)
    for name in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "COPILOT_HOME"):
        assert os.environ[name].startswith(paths["home"] + os.sep), name
