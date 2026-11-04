"""Handoff runs with stub agents and the real atticdb binary. Build it first: cargo build -p atticdb,
or set ATTICDB_BIN. Tests that start no child run without it. The real claude and codex never run."""

from __future__ import annotations

import csv
import json
import os
import shutil
import subprocess
import tempfile
from dataclasses import dataclass

import pytest

from research.handoff_eval import arms, cli, fixture, results, run
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
def test_run_gets_its_own_config_dirs(env, monkeypatch):
    """The shared eval Codex dir's memories and sessions never reach a run. Only auth.json and
    config.toml are copied. Claude starts empty and finds its login through the eval dir."""
    for sub in ("memories", "sessions"):
        os.makedirs(os.path.join(env["codex_dir"], sub))
        with open(os.path.join(env["codex_dir"], sub, "old.md"), "w") as f:
            f.write("ticket LEDG-7731\n")
    with open(os.path.join(env["codex_dir"], "config.toml"), "w") as f:
        f.write('model_reasoning_effort = "low"\n')
    with open(os.path.join(env["claude_dir"], "settings.json"), "w") as f:
        f.write('{"note": "LEDG-7731"}\n')
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.captured is False and res.delivered is False
    writer, reader = agent_calls(env["log"])
    for call in (writer, reader):
        assert call["dirs"]["CLAUDE_CONFIG_DIR"].startswith(env["tmp"] + "/")
        assert call["dirs"]["CODEX_HOME"].startswith(env["tmp"] + "/")
        assert call["dirs"]["CLAUDE_SECURESTORAGE_CONFIG_DIR"] == env["claude_dir"]
        assert not {"memories", "sessions"} & set(call["codex_home_files"])
        # The harness dirs are neither under the repo's parent nor next to the repo.
        run_root = os.path.dirname(call["cwd"])
        for d in (call["dirs"]["CLAUDE_CONFIG_DIR"], call["dirs"]["CODEX_HOME"]):
            assert not d.startswith(run_root + "/") and os.path.dirname(d) != run_root
    assert {"auth.json", "config.toml"} <= set(writer["codex_home_files"])
    # The Codex writer's dir kept only its login once the writer was done.
    assert reader["codex_home_files"] == ["auth.json"]
    assert_cleaned_up(env)


@needs_atticdb
def test_missing_codex_login_errors_at_setup(env):
    os.remove(os.path.join(env["codex_dir"], "auth.json"))
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "setup" and res.sessions == 0
    assert any("no auth.json. Log in once: CODEX_HOME=<codex-dir> codex login" in r for r in res.reasons), res.reasons
    assert agent_calls(env["log"]) == []
    assert_cleaned_up(env)


@needs_atticdb
def test_codex_login_refresh_is_kept(env, monkeypatch):
    monkeypatch.setenv("STUB_REFRESH_AUTH", "v2")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert "codex refreshed its login; the eval dir's auth.json was updated" in res.reasons
    with open(os.path.join(env["codex_dir"], "auth.json")) as f:
        assert json.load(f) == {"refreshed": "v2"}
    assert oct(os.stat(os.path.join(env["codex_dir"], "auth.json")).st_mode & 0o777) == "0o600"


@needs_atticdb
def test_stale_token_in_a_copied_config_is_a_start_error(env):
    with open(os.path.join(env["codex_dir"], "config.toml"), "w") as f:
        f.write("# see LEDG-7731\n")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "start"
    assert any("did not start clean: codex-dir:config.toml[LEDG-7731]" in r for r in res.reasons), res.reasons
    assert agent_calls(env["log"]) == []


@needs_atticdb
@pytest.mark.parametrize("arm", ["native", "files"])
@pytest.mark.parametrize("writer,reader", DIRECTIONS)
def test_reader_grep_finds_no_writer_transcript(env, monkeypatch, arm, writer, reader):
    """The reader greps the repo's parent and both harness dirs its env names. The writer's
    transcripts live in a separate temp dir and are deleted before the reader, so it finds none."""
    monkeypatch.setenv("STUB_READER_GREP", "1")
    monkeypatch.setenv("STUB_WRITER", "silent")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is False and res.delivered is False and res.delivered_where == ""
    assert_cleaned_up(env)
    # Control: without the clean-up, the same grep finds the writer's prompt in its transcript.
    monkeypatch.setattr(run, "scrub_harness_dir", lambda hroot, harness: [])
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, writer, reader, 1)
    assert res.delivered is True and res.delivered_where == "transcript[LEDG-7731,412]"


def fake_atticdb(tmp_path, disconnect_exit: int) -> str:
    """The real binary, except `disconnect`, which does nothing and exits `disconnect_exit`."""
    path = tmp_path / "atticdb-wrap"
    path.write_text(f'#!/bin/sh\nif [ "$1" = disconnect ]; then exit {disconnect_exit}; fi\nexec "{ATTICDB}" "$@"\n')
    path.chmod(0o755)
    return str(path)


@needs_atticdb
def test_stale_atticdb_wiring_errors_at_setup(env, tmp_path):
    # A failed pre-disconnect stops a run outside the atticdb arm.
    cfg = make_cfg(env)
    cfg.atticdb = fake_atticdb(tmp_path, 1)
    res = run_handoff(cfg, Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "setup"
    assert "error in setup: pre-disconnect claude-code exited 1" in res.reasons
    # In the atticdb arm, setup rewires the harness anyway, so it is only noted.
    res = run_handoff(cfg, Runner(), CASES["H1"], "atticdb", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert "pre-disconnect claude-code exited 1" in res.reasons
    # Wiring that a disconnect left in place stops the run before any agent.
    with open(os.path.join(env["codex_dir"], "config.toml"), "w") as f:
        f.write('[projects."/src/atticdb-heval"]\ntrust_level = "trusted"\n\n'
                '[mcp_servers.atticdb]\ncommand = "/old/atticdb"\n')
    cfg.atticdb = fake_atticdb(tmp_path, 0)
    before = len(agent_calls(env["log"]))
    res = run_handoff(cfg, Runner(), CASES["H1"], "files", "codex", "claude-code", 1)
    assert res.errored and res.error_stage == "setup"
    assert "error in setup: harness config mentions atticdb before the run: codex-dir:config.toml" in res.reasons
    assert len(agent_calls(env["log"])) == before
    assert_cleaned_up(env)


def test_wiring_check_skips_trust_headers(tmp_path):
    root = str(tmp_path)
    claude, codex = run.run_dirs(root)
    os.makedirs(claude)
    os.makedirs(codex)
    with open(os.path.join(codex, "config.toml"), "w") as f:
        f.write('[projects."/src/atticdb"]\ntrust_level = "trusted"\n')
    assert run.wiring_hits(root) == []
    for folder, name in ((claude, "settings.json"), (claude, ".claude.json"), (codex, "hooks.json"),
                         (codex, "AGENTS.md")):
        with open(os.path.join(folder, name), "w") as f:
            f.write("run atticdb hook session-start\n")
    assert run.wiring_hits(root) == ["claude-dir:settings.json", "claude-dir:.claude.json", "codex-dir:hooks.json",
                                     "codex-dir:AGENTS.md"]


@needs_atticdb
@pytest.mark.parametrize("error", [subprocess.CalledProcessError(128, ["git", "commit"]),
                                   subprocess.TimeoutExpired(["git", "init"], 30)])
def test_fixture_errors_are_recorded(env, monkeypatch, error):
    def broken(repo):
        raise error

    monkeypatch.setattr(run, "write_fixture", broken)
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "claude-code", "codex", 1)
    assert res.errored and res.error_stage == "fixture" and res.sessions == 0
    assert any(r.startswith(f"error in fixture: the fixture could not be written: {type(error).__name__}")
               for r in res.reasons)
    assert stub_calls(env["log"]) == []
    assert_cleaned_up(env)


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
@pytest.mark.parametrize("cid,writer_work", [("H1", True), ("H2", False)])
def test_oracle_repo_holds_the_writer_work(env, monkeypatch, cid, writer_work):
    """The oracle has no writer, so a case whose writer changes code gets that change committed as
    "writer work" before the reader. The diff base is after it, so the reader's diff is its own."""
    seen = {}

    def spy(case, reply, diff, repo):
        with open(os.path.join(repo, "ledger.py")) as f:
            seen["ledger"] = f.read()
        log = subprocess.run(["git", "log", "--format=%s"], cwd=repo, capture_output=True, text=True,
                             env={"PATH": os.environ["PATH"], **agents.git_env()}, timeout=30)
        seen["log"], seen["diff"] = log.stdout.splitlines(), diff
        return Verdict(True, [])

    monkeypatch.setattr(run, "grade", spy)
    res = run_handoff(make_cfg(env), Runner(), CASES[cid], "oracle", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    if writer_work:
        assert seen["ledger"] == CASES[cid].oracle_state["ledger.py"]
        assert seen["log"] == ["writer work", "Initial commit"]
    else:
        assert seen["ledger"] == fixture.LEDGER_PY and seen["log"] == ["Initial commit"]
    assert seen["diff"] == ""
    # Other arms never get it: their writer does the work.
    assert run.oracle_state(CASES["H1"], "native") == {}
    assert_cleaned_up(env)


CODEX_FAILED = ('{"type":"thread.started","thread_id":"t1"}\n{"type":"turn.started"}\n'
                '{"type":"turn.failed","error":{"message":"usage limit"}}\n')
CLAUDE_FAILED = ('{"type":"system","subtype":"init","session_id":"s1","tools":[]}\n'
                 '{"type":"result","subtype":"success","is_error":true,"num_turns":1,'
                 '"result":"Claude AI usage limit reached"}\n')


@needs_atticdb
@pytest.mark.parametrize("writer,stream,message", [("codex", CODEX_FAILED, "usage limit"),
                                                   ("claude-code", CLAUDE_FAILED, "Claude AI usage limit reached")])
def test_stream_errors_come_before_stderr(env, monkeypatch, tmp_path, writer, stream, message):
    replay = tmp_path / "failed.jsonl"
    replay.write_text(stream)
    monkeypatch.setenv("STUB_REPLAY", str(replay))
    monkeypatch.setenv("STUB_STDERR", "Reading additional input from stdin...")
    monkeypatch.setenv("STUB_EXIT_AFTER", "1")
    reader = "claude-code" if writer == "codex" else "codex"
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", writer, reader, 1)
    assert res.errored and res.error_stage == "writer" and res.passed is None
    assert (f"error in writer: writer 1 exited 1: stream error: {message}; "
            "stderr: Reading additional input from stdin...") in res.reasons
    assert_cleaned_up(env)


def test_stream_errors_without_a_failing_exit():
    assert run.stream_errors("codex", b'{"type":"error","message":"stream disconnected"}\n'
                                      b'{"type":"item.completed","item":{"type":"error","message":"bad"}}\n') == [
        "stream disconnected", "bad"]
    assert run.stream_errors("codex", b'{"type":"turn.completed","usage":{}}\n') == []
    assert run.stream_errors("claude-code", b'{"type":"result","subtype":"error_max_turns","is_error":true}\n') == [
        "error_max_turns"]


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
    made = []
    real_hroot = run.make_harness_root

    def recorded_root(c):
        made.append(real_root(c))
        return made[-1]

    def remembered_hroot(c):
        hroot = real_hroot(c)
        mem = run.native_memory_dir(hroot, "claude-code", run.paths(made[-1])[1])
        os.makedirs(mem)
        with open(os.path.join(mem, "MEMORY.md"), "w") as f:
            f.write("bramble-ci\n")
        return hroot

    monkeypatch.setattr(run, "make_root", recorded_root)
    monkeypatch.setattr(run, "make_harness_root", remembered_hroot)
    res = run_handoff(cfg, Runner(), CASES["H4"], "files", "codex", "claude-code", 1)
    assert res.errored and res.error_stage == "start"
    assert any("claude-dir:projects/" in r and "/memory/MEMORY.md[bramble-ci]" in r for r in res.reasons), res.reasons

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
    # The pilot is cross-harness only.
    for same in ("codex:codex", "claude-code:claude-code"):
        with pytest.raises(SystemExit) as e:
            cli.parse(base + ["--directions", same, "--claude-model", "X", "--codex-model", "Y"])
        assert e.value.code == 2 and "the pilot is cross-harness only" in capsys.readouterr().err


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
    assert res.check_timed_out is True and results.row("d", res)["check_timed_out"] == 1


def test_grader_flag_wins_over_reason_text():
    @dataclass
    class Flagged:
        passed: bool
        reasons: list
        check_timed_out: bool

    assert run.grader_timed_out(Flagged(False, ["the test says: timed out"], False)) is False
    assert run.grader_timed_out(Flagged(False, [], True)) is True
    assert run.grader_timed_out(Verdict(False, ["repo tests fail: timed out after 20 s"])) is True
    assert run.grader_timed_out(Verdict(False, ["repo tests fail: AssertionError"])) is False


# ---- evidence patterns ----

def test_every_evidence_token_has_a_pattern():
    tokens = {t for case in CASES.values() for t in case.evidence}
    assert tokens <= set(run.EVIDENCE_PATTERNS), tokens - set(run.EVIDENCE_PATTERNS)


@pytest.mark.parametrize("token,text,hit", [
    ("LEDG-7731", "see ledg-7731.", True), ("LEDG-7731", "LEDG-77310", False), ("LEDG-7731", "XLEDG-7731", False),
    ("INC-2209", "repo/evalrepo/inc-2209-deadlock", True), ("FIN-88", "FIN-880", False),
    ("bramble-ci", "`bramble-ci`", True), ("STOR-5150", "STOR-51501", False),
    ("412", "412 bytes", True), ("412", "412-byte headers", True), ("412", "412bytes", True),
    ("412", "HTTP 412", False), ("412", "-rw-r--r--  1 eval  staff  4120 Nov  4 ledger.py", False),
    ("412", "line 412: bytes", False),
    ("3.9", "only has Python 3.9.", True), ("3.9", "python3.9", False), ("3.9", "Python 3.9.6", False),
    ("3.9", "Python 3.95", False), ("3.9", "requires 3.9", False), ("3.9", "bramble-ci runs 3.9 only", True),
    ("3.9", "3.9 is what bramble-ci has", True),
    ("3.9", 'File "/usr/lib/python 3.9/site-packages/x.py", line 3', False),
    ("3.9", "bramble-ci at /opt/py/3.9/bin", False), ("3.9", "bramble-ci" + "x" * 50 + " 3.9", False),
    ("plain files", "plain-file storage", True), ("plain files", "Plain files now", True),
    ("plain files", "explain files", False),
])
def test_evidence_patterns(token, text, hit):
    assert run.has_token(text, token) is hit


def test_python_version_output_is_not_delivery():
    stream = [{"type": "item.completed", "item": {"type": "command_execution", "command": "python3 --version",
                                                  "aggregated_output": "Python 3.9.6\n"}},
              {"type": "item.completed", "item": {"type": "agent_message", "text": "Yes."}}]
    outputs = "\n".join(run.tool_outputs("codex", "\n".join(map(json.dumps, stream)).encode()))
    assert "Python 3.9.6" in outputs
    assert run.tokens_in(outputs, CASES["H4"].evidence) == []


@pytest.mark.parametrize("output", [
    'Traceback (most recent call last):\n  File "/usr/local/lib/python3.9/site-packages/ledger/x.py", line 4\n',
    'Traceback (most recent call last):\n  File "/opt/Python 3.9/lib/x.py", line 4\n',
    "Name: pip\nVersion: 21.2\nLocation: /usr/lib/python3.9/site-packages\n",
])
def test_interpreter_paths_are_not_delivery(output):
    stream = [{"type": "item.completed", "item": {"type": "command_execution", "command": "pip show pip",
                                                  "aggregated_output": output}},
              {"type": "item.completed", "item": {"type": "agent_message", "text": "Yes."}}]
    outputs = "\n".join(run.tool_outputs("codex", "\n".join(map(json.dumps, stream)).encode()))
    assert run.tokens_in(outputs, CASES["H4"].evidence) == []


def test_short_tokens_in_repo_files(tmp_path):
    (tmp_path / "ls.txt").write_text("-rw-r--r--  1 eval  staff  4120 Nov  4 ledger.py\n")
    assert run.file_hits(str(tmp_path), CASES["H1"].evidence, "repo") == []
    (tmp_path / "NOTES.md").write_text("bramble-ci only has Python 3.9\n")
    assert run.file_hits(str(tmp_path), CASES["H4"].evidence, "repo") == [("repo:NOTES.md", "bramble-ci"),
                                                                         ("repo:NOTES.md", "3.9")]


def test_repo_scan_skips_venvs_and_caches(tmp_path):
    for d in (".git", ".venv", "venv", "__pycache__", ".pytest_cache", "node_modules", "env/lib"):
        (tmp_path / d).mkdir(parents=True)
        (tmp_path / d / "x.txt").write_text("LEDG-7731\n")
    (tmp_path / "env" / "pyvenv.cfg").write_text("home = /usr/bin\n")
    assert run.file_hits(str(tmp_path), CASES["H1"].evidence, "repo") == []
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "x.txt").write_text("LEDG-7731\n")
    assert run.file_hits(str(tmp_path), CASES["H1"].evidence, "repo") == [("repo:src/x.txt", "LEDG-7731")]


def test_write_and_edit_echoes_are_not_delivery():
    stream = [
        {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "w1", "name": "Write",
             "input": {"file_path": "n.md", "content": "LEDG-7731"}}]}},
        {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "w1",
                                                  "content": "File created successfully at: n.md"}]},
         "tool_use_result": {"type": "create", "filePath": "n.md", "content": "LEDG-7731",
                             "structuredPatch": [{"lines": ["+LEDG-7731"]}]}},
        {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "e1", "name": "Edit", "input": {"new_string": "FIN-88"}}]}},
        {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "e1",
                                                  "content": "The file n.md has been updated: 1\tFIN-88"}]},
         "tool_use_result": {"filePath": "n.md", "oldString": "x", "newString": "FIN-88",
                             "structuredPatch": [{"lines": ["+FIN-88"]}]}},
        {"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "r1", "name": "Read", "input": {"file_path": "HANDOFF.md"}}]}},
        {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "r1", "content": "1\tok"}]},
         "tool_use_result": {"type": "text", "file": {"filePath": "HANDOFF.md", "content": "INC-2209"}}},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": "Done."}]}},
    ]
    outputs = "\n".join(run.tool_outputs("claude-code", "\n".join(map(json.dumps, stream)).encode()))
    assert run.tokens_in(outputs, ("LEDG-7731", "FIN-88", "INC-2209")) == ["INC-2209"]


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
