"""End to end with the real atticdb binary and stub agents. Build it first: cargo build -p atticdb,
or set ATTICDB_BIN to a built binary."""

import csv
import json
import os
import shutil
import subprocess
import tempfile

import pytest

from research.setup_eval import cli, harness
from research.setup_eval.proc import Runner

from .helpers import FIXTURES, ATTICDB, fake_home, stub_calls

pytestmark = pytest.mark.skipif(not os.access(ATTICDB, os.X_OK), reason="build target/debug/atticdb or set ATTICDB_BIN")


@pytest.fixture
def short_tmp(monkeypatch):
    """A short temp dir, as the atticdb socket path must fit in 104 bytes."""
    parent = tempfile.mkdtemp(prefix="zse-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    yield parent
    shutil.rmtree(parent, ignore_errors=True)


def servers_under(path: str) -> list[str]:
    ps = subprocess.run(["ps", "-A", "-ww", "-o", "command="], capture_output=True, text=True, timeout=10).stdout
    return [line for line in ps.splitlines() if " serve " in line and path in line]


def rows(path) -> list[dict]:
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def test_full_batch_with_stub_agents(monkeypatch, tmp_path, short_tmp, capsys):
    monkeypatch.setenv("ZERODB_HOME", "/old")
    monkeypatch.setenv("ZERODB_TOKEN", "old")
    paths = fake_home(monkeypatch, tmp_path)
    assert "ZERODB_HOME" not in os.environ and "ZERODB_TOKEN" not in os.environ
    # An old token in the user's shell never reaches a child, so no atticdb child stops at its guard.
    monkeypatch.setenv("ZERODB_TOKEN", "old")
    # A decoy repo that inherited git vars point at. No step may touch it.
    decoy = tmp_path / "decoy"
    subprocess.run(["git", "init", "-q", str(decoy)], check=True, timeout=30)
    monkeypatch.setenv("GIT_DIR", str(decoy / ".git"))
    monkeypatch.setenv("GIT_WORK_TREE", str(decoy))
    monkeypatch.setenv("GIT_INDEX_FILE", str(decoy / ".git" / "index"))
    decoy_before = sorted(p.relative_to(decoy) for p in decoy.rglob("*"))
    for name in ("ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "OPENAI_API_KEY",
                 "OPENAI_BASE_URL"):
        monkeypatch.setenv(name, "v")
    out = tmp_path / "results" / "batch.csv"
    code = cli.main(["run", "--harness", "both", "--scenarios", "S1,S2,S3,S4,B1,B2", "--runs", "1",
                     "--atticdb", ATTICDB, "--out", str(out)])
    text = capsys.readouterr().out
    assert code == 0, text
    got = rows(out)
    assert [r["harness"] for r in got] == ["claude-code"] * 6 + ["codex"] * 6
    by = {(r["harness"], r["scenario"]): r for r in got}
    for h in ("claude-code", "codex"):
        for sid in ("S1", "S2", "S3", "S4"):
            assert by[h, sid]["pass"] == "1", (h, sid, by[h, sid]["notes"])
        assert by[h, "B1"]["pass"] == "0" and "reply lacks nextest" in by[h, "B1"]["notes"]
        assert by[h, "B2"]["pass"] == "0" and "reply lacks the reason" in by[h, "B2"]["notes"]
        assert json.loads(by[h, "S2"]["atticdb_calls"]) == {"memory_search": 1}
        assert json.loads(by[h, "S3"]["atticdb_calls"]) == {"fact_set": 1}
        assert json.loads(by[h, "S4"]["atticdb_calls"]) == {"memory_append": 1}
        for r in (by[h, s] for s in ("S1", "S2", "S3", "S4", "B1", "B2")):
            assert r["setup_exit"] == "0" and r["parse_ok"] == "1" and r["date"]
            assert r["input_tokens"] == "1110" and r["cache_read_tokens"] == "1000" and r["output_tokens"] == "20"
            assert "reply=" in r["notes"] and "/tmp/zse-t-" not in r["notes"]
    assert sorted(p.relative_to(decoy) for p in decoy.rglob("*")) == decoy_before, "the decoy repo is untouched"
    # Nothing is left: no temp root, no server, and the eval config dirs hold no AtticDB part.
    assert os.listdir(short_tmp) == []
    assert servers_under(short_tmp) == []
    with open(os.path.join(paths["claude_dir"], ".claude.json")) as f:
        assert "atticdb" not in json.load(f).get("mcpServers", {})
    for name in ("settings.json", "hooks.json"):
        for folder in (paths["claude_dir"], paths["codex_dir"]):
            p = os.path.join(folder, name)
            if os.path.exists(p):
                assert "atticdb" not in open(p).read()
    if os.path.exists(os.path.join(paths["codex_dir"], "config.toml")):
        assert "mcp_servers.atticdb" not in open(os.path.join(paths["codex_dir"], "config.toml")).read()
    # The agents ran with the exact argv, from the fixture repo, with ATTICDB_HOME set.
    agent_calls = [c for c in stub_calls(paths["log"]) if c["argv"][:1] in (["-p"], ["exec"])]
    assert len(agent_calls) == 12
    assert all(c["atticdb_home"] and c["cwd"].endswith("/evalrepo") for c in agent_calls)
    first = agent_calls[0]["argv"]
    assert first[0] == "-p" and first[-3:] == ["--permission-mode", "acceptEdits",
                                               "--allowedTools=mcp__atticdb__fact_set"]
    assert all(c["atticdb_home"] and c["api_keys"] == [] and c["git_env"] == [] and c["outside_home"] == []
               for c in stub_calls(paths["log"]))
    assert "Every child had ATTICDB_HOME set under its own run root" in text
    assert "real atticdb home's data dir did not change" in text


def test_garbage_stream_and_failed_setup(monkeypatch, tmp_path, short_tmp, capsys):
    paths = fake_home(monkeypatch, tmp_path)
    out = tmp_path / "o.csv"
    monkeypatch.setenv("STUB_REPLAY", os.path.join(FIXTURES, "codex-events.jsonl"))
    assert cli.main(["run", "--harness", "codex", "--scenarios", "S2", "--atticdb", ATTICDB, "--out", str(out)]) == 0
    (r,) = rows(out)
    assert r["parse_ok"] == "0" and "unparsed=2" in r["notes"] and r["pass"] == "1"
    # Without claude on PATH, setup can't add the MCP entry: the agent is skipped.
    os.remove(os.path.join(paths["bin"], "claude"))
    assert cli.main(["run", "--harness", "claude-code", "--scenarios", "S1", "--atticdb", ATTICDB,
                     "--out", str(out)]) == 0
    r = rows(out)[-1]
    assert r["setup_exit"] != "0" and r["pass"] == "0" and "agent skipped: setup failed" in r["notes"]
    assert "why=" in r["notes"] and "claude is not on PATH" in r["notes"]
    for value in (short_tmp, paths["home"], paths["claude_dir"]):
        assert value not in r["notes"], "no env value reaches the results"
    assert os.listdir(short_tmp) == [] and servers_under(short_tmp) == []
    capsys.readouterr()


def test_agent_timeout_is_recorded(monkeypatch, tmp_path, short_tmp):
    fake_home(monkeypatch, tmp_path)
    monkeypatch.setenv("STUB_SLEEP", "60")
    out = tmp_path / "o.csv"
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--atticdb", ATTICDB, "--out", str(out)])
    cfg = cli.config(a.atticdb)
    cfg.agent_timeout = 2.0
    assert cli.batch(a, cfg, out=lambda *_: None) == 0
    (r,) = rows(out)
    assert r["pass"] == "0" and "agent timed out after 2 s" in r["notes"] and float(r["wall_s"]) < 30
    assert os.listdir(short_tmp) == []


def test_agent_failure_notes_are_redacted(monkeypatch, tmp_path, short_tmp):
    paths = fake_home(monkeypatch, tmp_path)
    secret = "sk-ant-api03-" + "q" * 30
    monkeypatch.setenv("STUB_EXIT", "1")
    monkeypatch.setenv("STUB_STDERR", f"Error: login expired for {secret} in {paths['claude_dir']}")
    out = tmp_path / "o.csv"
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--atticdb", ATTICDB, "--out", str(out)])
    assert cli.batch(a, cli.config(a.atticdb), out=lambda *_: None) == 0
    (r,) = rows(out)
    assert r["pass"] == "0" and r["parse_ok"] == "0" and "agent_exit=1" in r["notes"]
    assert "why=Error: login expired for [redacted] in <claude-dir>" in r["notes"]
    assert secret not in r["notes"] and paths["home"] not in r["notes"]


def test_a_right_answer_without_a_clean_finish_fails(monkeypatch, tmp_path, short_tmp):
    fake_home(monkeypatch, tmp_path)
    out = tmp_path / "o.csv"
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--atticdb", ATTICDB, "--out", str(out)])
    monkeypatch.setenv("STUB_EXIT_AFTER", "1")
    assert cli.batch(a, cli.config(a.atticdb), out=lambda *_: None) == 0
    monkeypatch.delenv("STUB_EXIT_AFTER")
    errored = tmp_path / "error.jsonl"
    errored.write_text('{"type":"assistant","message":{"content":[{"type":"text","text":"cargo nextest run"}]}}\n'
                       '{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":2}\n')
    monkeypatch.setenv("STUB_REPLAY", str(errored))
    assert cli.batch(a, cli.config(a.atticdb), out=lambda *_: None) == 0
    first, second = rows(out)
    assert "nextest" in first["notes"] and "agent_exit=1" in first["notes"] and first["pass"] == "0"
    assert "nextest" in second["notes"] and "no final reply" in second["notes"] and second["pass"] == "0"
    assert "the agent reported an error" in second["notes"]


def test_server_stop_ignores_an_inherited_columns(monkeypatch, tmp_path, short_tmp):
    """With COLUMNS=40, a ps without -ww cuts the command line before --data-dir on Linux, and
    the server would be left running."""
    fake_home(monkeypatch, tmp_path)
    monkeypatch.setenv("COLUMNS", "40")
    cfg = cli.config(ATTICDB)
    assert cfg.base_env["COLUMNS"] == "40"
    root = harness.make_root(cfg)
    zh, repo = harness.paths(root)
    os.makedirs(repo)
    env = dict(cfg.base_env, ATTICDB_HOME=zh)
    seeded = subprocess.run([ATTICDB, "fact", "set", "--key", "k", "--content", "v"], cwd=repo, env=env,
                            capture_output=True, timeout=30)
    assert seeded.returncode == 0, seeded.stderr
    assert servers_under(root), "the shell verb started a server"
    runner = Runner()
    argvs = []
    real_run = runner.run
    monkeypatch.setattr(runner, "run", lambda argv, **kw: argvs.append(argv) or real_run(argv, **kw))
    assert harness.stop_server(cfg, runner, root) is None
    assert argvs and argvs[0][:2] == ["ps", "-ww"]
    assert servers_under(root) == []
    shutil.rmtree(root)
