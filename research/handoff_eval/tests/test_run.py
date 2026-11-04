"""Handoff runs with stub agents and the real atticdb binary. Build it first: cargo build -p atticdb,
or set ATTICDB_BIN. Tests that start no child run without it. The real claude and codex never run."""

from __future__ import annotations

import csv
import os
import shutil
import subprocess
import tempfile

import pytest

from research.handoff_eval import arms, cli, results, run
from research.handoff_eval.cases import CASES
from research.handoff_eval.run import Budget, HandoffConfig, run_handoff
from research.setup_eval import agents
from research.setup_eval.proc import Runner
from research.setup_eval.scenarios import Verdict

from .helpers import ATTICDB, agent_calls, fake_home, stub_calls

needs_atticdb = pytest.mark.skipif(not os.access(ATTICDB, os.X_OK),
                                   reason="build target/debug/atticdb or set ATTICDB_BIN")
MODELS = {"claude-code": "claude-x", "codex": "codex-y"}
DIRECTIONS = [("claude-code", "codex"), ("codex", "claude-code")]


@pytest.fixture
def short_tmp(monkeypatch):
    """A short temp dir, as the atticdb socket path must fit in 104 bytes."""
    parent = tempfile.mkdtemp(prefix="zhe-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    yield parent
    shutil.rmtree(parent, ignore_errors=True)


@pytest.fixture
def env(monkeypatch, tmp_path, short_tmp):
    paths = fake_home(monkeypatch, tmp_path)
    paths["tmp"] = short_tmp
    return paths


def make_cfg(env, **kw) -> HandoffConfig:
    kw.setdefault("models", dict(MODELS))
    kw.setdefault("agent_timeout", 60.0)
    return HandoffConfig(atticdb=ATTICDB, home=env["home"], claude_dir=env["claude_dir"],
                         codex_dir=env["codex_dir"], tmp_parent=env["tmp"], base_env=dict(os.environ), **kw)


def servers_under(path: str) -> list[str]:
    ps = subprocess.run(["ps", "-A", "-ww", "-o", "command="], capture_output=True, text=True, timeout=10).stdout
    return [line for line in ps.splitlines() if " serve " in line and path in line]


def assert_cleaned_up(env) -> None:
    assert os.listdir(env["tmp"]) == [], "the run root is deleted"
    assert servers_under(env["tmp"]) == [], "the run's server is stopped"


def rows(path) -> list[dict]:
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def pilot_args(*extra) -> list[str]:
    return ["--cases", "H1,H2,H3,H4,H5,H6", "--arms", "native,files,atticdb,oracle",
            "--directions", "claude-code:codex,codex:claude-code", "--reps", "1",
            "--claude-model", "X", "--codex-model", "Y", *extra]


# ---- the 4 arms ----

@needs_atticdb
@pytest.mark.parametrize("writer,reader", DIRECTIONS)
def test_atticdb_arm_captures_and_delivers(env, writer, reader):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb", writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is True
    assert "atticdb:fact repo/evalrepo/decision-" in res.captured_where
    assert "LEDG-7731" in res.captured_where and "412" in res.captured_where
    assert res.delivered is True and res.delivered_where.startswith("hook[LEDG-7731,412]")
    assert res.passed is True
    assert res.sessions == 2
    calls = agent_calls(env["log"])
    assert [c["prog"] for c in calls] == [writer.split("-")[0], reader.split("-")[0]]
    assert_cleaned_up(env)
    # Both harnesses were disconnected after the run.
    for name in ("settings.json", "hooks.json"):
        for folder in (env["claude_dir"], env["codex_dir"]):
            p = os.path.join(folder, name)
            assert not os.path.exists(p) or "atticdb" not in open(p).read()


@needs_atticdb
@pytest.mark.parametrize("writer,reader", DIRECTIONS)
def test_files_arm_reads_handoff(env, writer, reader):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is True and res.captured_where == "repo:HANDOFF.md[LEDG-7731,412]"
    assert res.delivered is True and res.delivered_where == "transcript[LEDG-7731,412]"
    assert res.passed is True
    # No atticdb setup ran in this arm, and Codex never got the atticdb approval override.
    assert not any(c["argv"][:1] == ["mcp"] and "add" in c["argv"] for c in stub_calls(env["log"]))
    for c in agent_calls(env["log"]):
        assert agents.CODEX_APPROVE not in c["argv"]
    assert_cleaned_up(env)


@needs_atticdb
@pytest.mark.parametrize("writer,reader", DIRECTIONS)
def test_native_arm_has_no_capture_path(env, writer, reader):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is False and res.captured_where == ""
    assert res.delivered is False and res.passed is False
    assert "reply cites neither LEDG-7731 nor both 412 and 24" in res.reasons
    assert_cleaned_up(env)


@needs_atticdb
@pytest.mark.parametrize("writer,label,name", [("claude-code", "claude-memory", "MEMORY.md"),
                                               ("codex", "codex-memory", "stub.md")])
def test_native_memory_counts_as_capture(env, monkeypatch, writer, label, name):
    monkeypatch.setenv("STUB_NATIVE_MEMORY", "1")
    reader = "codex" if writer == "claude-code" else "claude-code"
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is True and res.captured_where == f"{label}:{name}[LEDG-7731,412]"


@needs_atticdb
def test_old_codex_memory_is_not_capture(env, monkeypatch):
    """Only files a writer changed during the run count. An old one is named as stale."""
    memories = os.path.join(env["codex_dir"], "memories")
    os.makedirs(memories)
    with open(os.path.join(memories, "old.md"), "w") as f:
        f.write("ticket LEDG-7731\n")
    monkeypatch.setenv("STUB_WRITER", "silent")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.captured is False
    assert "stale codex memory from an earlier run: codex-memory:old.md[LEDG-7731]" in res.reasons


@needs_atticdb
@pytest.mark.parametrize("writer,reader", DIRECTIONS)
def test_oracle_skips_writer(env, writer, reader):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "oracle", writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.sessions == 1 and res.captured is None and res.captured_where == ""
    assert res.delivered is True and res.delivered_where.startswith("prompt[LEDG-7731,412]")
    assert res.passed is True
    (call,) = agent_calls(env["log"])
    assert call["prog"] == reader.split("-")[0]
    prompt = call["argv"][1] if reader == "claude-code" else call["argv"][-1]
    assert prompt == f"Notes from the last session: {CASES['H1'].gold_note}\n\n{CASES['H1'].reader_prompt}"
    assert_cleaned_up(env)


@needs_atticdb
def test_h5_has_no_delivery_stage(env):
    res = run_handoff(make_cfg(env), Runner(), CASES["H5"], "atticdb", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and "LEDG-7731" in res.captured_where  # H1's tokens
    assert res.delivered is None and res.delivered_where == ""
    # The stub reader recites H1's reason when it sees it, which is no abstention.
    assert res.passed is False and "reply doesn't say it found no record" in res.reasons


# ---- isolation, errors and budget ----

@needs_atticdb
def test_run_starts_clean(env, monkeypatch):
    cfg = make_cfg(env)
    # 1: a repo that already holds a token.
    real = run.write_fixture

    def leaky_fixture(repo):
        real(repo)
        with open(os.path.join(repo, "notes.txt"), "w") as f:
            f.write("see LEDG-7731\n")

    monkeypatch.setattr(run, "write_fixture", leaky_fixture)
    res = run_handoff(cfg, Runner(), CASES["H1"], "atticdb", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "start" and res.passed is None and res.captured is None
    assert any("did not start clean: repo:notes.txt[LEDG-7731]" in r for r in res.reasons)
    monkeypatch.setattr(run, "write_fixture", real)

    # 2: a store left over from an earlier run.
    real_root = run.make_root

    def used_root(c):
        root = real_root(c)
        zh, repo = run.paths(root)
        e = dict(os.environ, ATTICDB_HOME=zh)
        os.makedirs(repo)
        subprocess.run([ATTICDB, "fact", "set", "--key", "repo/evalrepo/x", "--content", "struct, 412 bytes"],
                       cwd=repo, env=e, check=True, capture_output=True, timeout=30)
        os.rmdir(os.path.join(repo))
        return root

    monkeypatch.setattr(run, "make_root", used_root)
    res = run_handoff(cfg, Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "start"
    assert any("did not start clean: atticdb:fact repo/evalrepo/x[412]" in r for r in res.reasons), res.reasons
    monkeypatch.setattr(run, "make_root", real_root)

    # 3: Claude memory for this repo path, left by an earlier run with the same temp name.
    def remembered_root(c):
        root = real_root(c)
        mem = run.native_memory_dir(c, "claude-code", run.paths(root)[1])
        os.makedirs(mem)
        with open(os.path.join(mem, "MEMORY.md"), "w") as f:
            f.write("bramble-ci\n")
        return root

    monkeypatch.setattr(run, "make_root", remembered_root)
    res = run_handoff(cfg, Runner(), CASES["H4"], "files", "codex", "claude-code", 1)
    assert res.errored and res.error_stage == "start"
    assert any("claude-memory:MEMORY.md[bramble-ci]" in r for r in res.reasons), res.reasons

    assert agent_calls(env["log"]) == [], "no agent starts in a run that is not clean"
    assert_cleaned_up(env)


@needs_atticdb
def test_writer_timeout_is_errored_not_failed(env, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("STUB_SLEEP", "5")
    res = run_handoff(make_cfg(env, agent_timeout=1.0), Runner(), CASES["H3"], "atticdb", "codex", "claude-code", 1)
    assert res.errored and res.error_stage == "writer"
    assert res.passed is None and res.captured is None and res.delivered is None
    assert "error in writer: writer 1 timed out after 1 s" in res.reasons
    assert res.sessions == 1, "neither the 2nd writer nor the reader started"
    assert_cleaned_up(env)

    # In a batch, the next run still runs, and both rows are kept as errored.
    out = tmp_path / "r.csv"
    code = cli.main(["--cases", "H1", "--arms", "native,oracle", "--directions", "claude-code:codex",
                     "--claude-model", "X", "--codex-model", "Y", "--max-sessions", "3", "--agent-timeout", "1",
                     "--out", str(out), "--atticdb", ATTICDB])
    assert code == 1
    got = rows(out)
    assert [(r["arm"], r["errored"], r["error_stage"], r["passed"]) for r in got] == [
        ("native", "1", "writer", ""), ("oracle", "1", "reader", "")]
    text = capsys.readouterr().out
    assert "error in writer" in text and "error in reader" in text


@needs_atticdb
def test_budget_counts_every_session(env, tmp_path, capsys):
    # The CLI refuses a batch over the budget, counting writers and readers: H1 is 2, H3 is 3.
    args = ["--cases", "H1,H3", "--arms", "atticdb", "--directions", "claude-code:codex",
            "--claude-model", "X", "--codex-model", "Y", "--atticdb", ATTICDB, "--out", str(tmp_path / "r.csv")]
    with pytest.raises(SystemExit) as e:
        cli.parse(args + ["--max-sessions", "4"])
    assert e.value.code == 2
    assert "is 5 agent sessions (3 writer, 2 reader), over --max-sessions 4" in capsys.readouterr().err

    # At run time, the batch stops before a run that would pass the budget.
    a = cli.parse(args + ["--max-sessions", "5"])
    a.max_sessions = 4
    assert cli.batch(a, cli.config(a)) == 1
    assert "Stopped: H3 atticdb needs 3 more agent sessions, and 2 of --max-sessions 4 are used." \
        in capsys.readouterr().out
    assert len(agent_calls(env["log"])) == 2
    assert [r["case"] for r in rows(tmp_path / "r.csv")] == ["H1"]

    # Inside a run, no session starts past the budget: the reader is the 2nd session here.
    cfg = make_cfg(env, budget=Budget(1))
    res = run_handoff(cfg, Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "budget" and res.sessions == 1 and res.passed is None
    assert len(agent_calls(env["log"])) == 3
    assert_cleaned_up(env)


@needs_atticdb
def test_models_are_required_and_recorded(env, tmp_path):
    with pytest.raises(ValueError, match="no model id for codex"):
        run_handoff(make_cfg(env, models={"claude-code": "claude-x"}), Runner(), CASES["H1"], "native",
                    "claude-code", "codex", 1)
    assert stub_calls(env["log"]) == [] and os.listdir(env["tmp"]) == []

    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "claude-code", "codex", 1)
    assert (res.writer_model, res.reader_model) == ("claude-x", "codex-y")
    writer, reader = agent_calls(env["log"])
    assert writer["argv"][-2:] == ["--model", "claude-x"]
    assert reader["argv"][-3:] == ["-m", "codex-y", CASES["H1"].reader_prompt]
    row = results.row("2026-11-04", res)
    assert (row["writer_model"], row["reader_model"]) == ("claude-x", "codex-y")


def test_model_flags_are_required(tmp_path, capsys):
    base = ["--cases", "H1", "--max-sessions", "9", "--out", str(tmp_path / "r.csv")]
    for missing, flags in (("--codex-model", ["--claude-model", "X"]), ("--claude-model", ["--codex-model", "Y"])):
        with pytest.raises(SystemExit) as e:
            cli.parse(base + flags)
        assert e.value.code == 2 and f"{missing} is required" in capsys.readouterr().err
    # A direction that uses 1 harness needs only its model.
    a = cli.parse(base + ["--directions", "codex:codex", "--codex-model", "Y"])
    assert a.models == {"claude-code": "", "codex": "Y"}


def test_session_argv_adds_the_model_flag():
    prompt = "Do the thing."
    claude = arms.session_argv("claude-code", prompt, "m1", "atticdb")
    assert claude == agents.agent_argv("claude-code", prompt) + ["--model", "m1"]
    codex = arms.session_argv("codex", prompt, "m2", "atticdb")
    assert codex == agents.agent_argv("codex", prompt)[:-1] + ["-m", "m2", prompt]
    plain = arms.session_argv("codex", prompt, "m2", "native")
    assert agents.CODEX_APPROVE not in plain and plain[-3:] == ["-m", "m2", prompt]
    assert len(plain) == len(codex) - 2
    with pytest.raises(ValueError):
        arms.session_argv("codex", prompt, "", "native")


def test_dry_run_runs_nothing(monkeypatch, tmp_path, capsys):
    env = fake_home(monkeypatch, tmp_path)
    parent = tempfile.mkdtemp(prefix="zhe-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    try:
        out = tmp_path / "r.csv"
        code = cli.main(pilot_args("--dry-run", "--out", str(out), "--atticdb", "/nonexistent/atticdb"))
        text = capsys.readouterr().out
        assert code == 0
        assert stub_calls(env["log"]) == [] and not out.exists() and os.listdir(parent) == []
    finally:
        shutil.rmtree(parent, ignore_errors=True)
    assert "Total: 90 agent sessions (42 writer, 48 reader) in 48 runs." in text
    assert text.count("(agent session)") == 90
    assert text.count("--model X") == 45 and text.count("-m Y") == 45
    assert "run 48/48: rep 1 H6 oracle codex to claude-code: 1 agent session" in text


# ---- units ----

def test_claude_project_slug_follows_claude():
    # Seen in the eval dir: /private/var/folders/v3/_bq4h.../T/zse-_so3m4yr/evalrepo
    # became -private-var-folders-v3--bq4h...-T-zse--so3m4yr-evalrepo.
    assert run.claude_project_slug("/private/tmp/zse-_so3m4yr/eval.repo") == "-private-tmp-zse--so3m4yr-eval-repo"


def test_tool_outputs_stop_at_the_final_reply():
    import json
    claude = [
        {"type": "user", "message": {"content": [{"type": "tool_result", "content": "INC-2209 deadlock"}]}},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": "Final."}]}},
        {"type": "user", "message": {"content": [{"type": "tool_result", "content": "FIN-88 later"}]}},
    ]
    text = "\n".join(run.tool_outputs("claude-code", "\n".join(map(json.dumps, claude)).encode()))
    assert "INC-2209" in text and "FIN-88" not in text
    codex = [
        {"type": "item.completed", "item": {"type": "command_execution", "command": "grep FIN-88",
                                            "aggregated_output": "STOR-5150"}},
        {"type": "item.completed", "item": {"type": "mcp_tool_call", "server": "atticdb", "tool": "search",
                                            "result": {"content": [{"type": "text", "text": "bramble-ci"}]}}},
        {"type": "item.completed", "item": {"type": "agent_message", "text": "Done."}},
        {"type": "item.completed", "item": {"type": "command_execution", "aggregated_output": "LEDG-7731"}},
    ]
    text = "\n".join(run.tool_outputs("codex", "\n".join(map(json.dumps, codex)).encode()))
    assert "STOR-5150" in text and "bramble-ci" in text
    assert "FIN-88" not in text, "the command the agent typed is not context it received"
    assert "LEDG-7731" not in text


@needs_atticdb
def test_check_timeout_is_flagged(env, monkeypatch):
    monkeypatch.setattr(run, "grade", lambda *a: Verdict(False, ["fixture tests fail: timed out after 20 s"]))
    res = run_handoff(make_cfg(env), Runner(), CASES["H6"], "native", "claude-code", "codex", 1)
    assert not res.errored and res.passed is False and "check timed out" in res.reasons


@needs_atticdb
def test_code_case_runs_the_real_grader(env):
    """H2 with the real grader on the repo the reader leaves. The stub changes no code."""
    res = run_handoff(make_cfg(env), Runner(), CASES["H2"], "files", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and res.captured_where == "repo:HANDOFF.md[INC-2209]"
    assert res.delivered is True and res.passed is True
    assert_cleaned_up(env)


def test_summary_first_line_and_funnel():
    def r(arm, case, **kw):
        return run.HandoffResult(case, arm, "claude-code", "codex", 1, writer_model="X", reader_model="Y", **kw)

    done = [r("atticdb", "H1", captured=True, delivered=True, passed=True),
            r("atticdb", "H5", captured=True, passed=False),
            r("native", "H1", captured=False, delivered=False, passed=False),
            r("native", "H5", errored=True, error_stage="writer"),
            r("oracle", "H1", delivered=True, passed=True),
            r("oracle", "H5", passed=True)]
    lines = results.summary(done)
    assert lines[0] == "Integration pilot. Not a statistical result."
    text = "\n".join(lines)
    assert "| atticdb | 1/1 | 0/1 | 0 |" in text
    assert "| native | 0/1 | n/a | 1 |" in text
    assert "| atticdb | 2/2 | 1/1 | 1/2 | 2/2 | 0 |" in text
    assert "| native | 0/1 | 0/1 | 0/1 | 2/2 | 1 |" in text
    assert "| oracle | n/a | 1/1 | 2/2 | 2/2 | 0 |" in text
    assert "Errored runs by stage: writer 1." in text
