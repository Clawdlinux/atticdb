"""End to end with the real zerodb binary and stub agents. Build it first: cargo build -p zerodb,
or set ZERODB_BIN to a built binary."""

import csv
import json
import os
import shutil
import subprocess
import tempfile

import pytest

from research.setup_eval import cli

from .helpers import FIXTURES, ZERODB, fake_home, stub_calls

pytestmark = pytest.mark.skipif(not os.access(ZERODB, os.X_OK), reason="build target/debug/zerodb or set ZERODB_BIN")


@pytest.fixture
def short_tmp(monkeypatch):
    """A short temp dir, as the zerodb socket path must fit in 104 bytes."""
    parent = tempfile.mkdtemp(prefix="zse-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    yield parent
    shutil.rmtree(parent, ignore_errors=True)


def servers_under(path: str) -> list[str]:
    ps = subprocess.run(["ps", "-A", "-o", "command="], capture_output=True, text=True, timeout=10).stdout
    return [line for line in ps.splitlines() if " serve " in line and path in line]


def rows(path) -> list[dict]:
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def test_full_batch_with_stub_agents(monkeypatch, tmp_path, short_tmp, capsys):
    paths = fake_home(monkeypatch, tmp_path)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "v")
    monkeypatch.setenv("OPENAI_API_KEY", "v")
    out = tmp_path / "results" / "batch.csv"
    code = cli.main(["run", "--harness", "both", "--scenarios", "S1,S2,S3,S4,B1,B2", "--runs", "1",
                     "--zerodb", ZERODB, "--out", str(out)])
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
        assert json.loads(by[h, "S2"]["zerodb_calls"]) == {"memory_search": 1}
        assert json.loads(by[h, "S3"]["zerodb_calls"]) == {"fact_set": 1}
        assert json.loads(by[h, "S4"]["zerodb_calls"]) == {"memory_append": 1}
        for r in (by[h, s] for s in ("S1", "S2", "S3", "S4", "B1", "B2")):
            assert r["setup_exit"] == "0" and r["parse_ok"] == "1" and r["date"]
            assert r["input_tokens"] == "1110" and r["cache_read_tokens"] == "1000" and r["output_tokens"] == "20"
            assert "reply=" in r["notes"] and "/tmp/zse-t-" not in r["notes"]
    # Nothing is left: no temp root, no server, and the eval config dirs hold no zerodb part.
    assert os.listdir(short_tmp) == []
    assert servers_under(short_tmp) == []
    with open(os.path.join(paths["claude_dir"], ".claude.json")) as f:
        assert "zerodb" not in json.load(f).get("mcpServers", {})
    for name in ("settings.json", "hooks.json"):
        for folder in (paths["claude_dir"], paths["codex_dir"]):
            p = os.path.join(folder, name)
            if os.path.exists(p):
                assert "zerodb" not in open(p).read()
    if os.path.exists(os.path.join(paths["codex_dir"], "config.toml")):
        assert "mcp_servers.zerodb" not in open(os.path.join(paths["codex_dir"], "config.toml")).read()
    # The agents ran with the exact argv, from the fixture repo, with ZERODB_HOME set.
    agent_calls = [c for c in stub_calls(paths["log"]) if c["argv"][:1] in (["-p"], ["exec"])]
    assert len(agent_calls) == 12
    assert all(c["zerodb_home"] and c["cwd"].endswith("/evalrepo") for c in agent_calls)
    first = agent_calls[0]["argv"]
    assert first[0] == "-p" and first[-3:] == ["--permission-mode", "acceptEdits",
                                               "--allowedTools=mcp__zerodb__fact_set"]
    assert all(c["zerodb_home"] and c["api_keys"] == [] for c in stub_calls(paths["log"]))
    assert "Every child had ZERODB_HOME set under its own run root" in text
    assert "real zerodb home's data dir did not change" in text


def test_garbage_stream_and_failed_setup(monkeypatch, tmp_path, short_tmp, capsys):
    paths = fake_home(monkeypatch, tmp_path)
    out = tmp_path / "o.csv"
    monkeypatch.setenv("STUB_REPLAY", os.path.join(FIXTURES, "codex-events.jsonl"))
    assert cli.main(["run", "--harness", "codex", "--scenarios", "S2", "--zerodb", ZERODB, "--out", str(out)]) == 0
    (r,) = rows(out)
    assert r["parse_ok"] == "0" and "unparsed=2" in r["notes"] and r["pass"] == "1"
    # Without claude on PATH, setup can't add the MCP entry: the agent is skipped.
    os.remove(os.path.join(paths["bin"], "claude"))
    assert cli.main(["run", "--harness", "claude-code", "--scenarios", "S1", "--zerodb", ZERODB,
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
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--zerodb", ZERODB, "--out", str(out)])
    cfg = cli.config(a.zerodb)
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
    a = cli.parse(["run", "--harness", "claude-code", "--scenarios", "S1", "--zerodb", ZERODB, "--out", str(out)])
    assert cli.batch(a, cli.config(a.zerodb), out=lambda *_: None) == 0
    (r,) = rows(out)
    assert r["pass"] == "0" and r["parse_ok"] == "0" and "agent_exit=1" in r["notes"]
    assert "why=Error: login expired for [redacted] in <claude-dir>" in r["notes"]
    assert secret not in r["notes"] and paths["home"] not in r["notes"]
