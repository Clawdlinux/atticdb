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
from research.setup_eval import agents, guard
from research.setup_eval.proc import Runner
from research.setup_eval.scenarios import Verdict

from .helpers import ATTICDB, agent_calls, fake_home, stub_calls

needs_atticdb = pytest.mark.skipif(not os.access(ATTICDB, os.X_OK),
                                   reason="build target/debug/atticdb or set ATTICDB_BIN")
MODELS = {"claude-code": "claude-x", "codex": "codex-y", "copilot": "copilot-z"}
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


def claude_stream(reply: str, tool_result: str = "") -> str:
    """A claude reader's stream: 1 Bash call with `tool_result` when given, then `reply`."""
    events = [{"type": "system", "subtype": "init", "session_id": "s1", "tools": []}]
    if tool_result:
        events += [{"type": "assistant", "message": {"role": "assistant", "content": [
                       {"type": "tool_use", "id": "toolu_0", "name": "Bash", "input": {"command": "grep -r x ."}}]}},
                   {"type": "user", "message": {"role": "user", "content": [
                       {"type": "tool_result", "tool_use_id": "toolu_0",
                        "content": [{"type": "text", "text": tool_result}]}]}}]
    events += [{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}},
               {"type": "result", "subtype": "success", "is_error": False, "num_turns": 2, "result": reply}]
    return "".join(json.dumps(ev) + "\n" for ev in events)


def test_blank_plan_has_reader_only(env):
    cfg = make_cfg(env, models=dict(MODELS))
    steps = run.plan(cfg, CASES["H1"], "blank", "claude-code", "copilot", "/tmp/zhe-root")
    assert steps["setup"] == [] and steps["writer"] == [] and steps["capture"] == [] and steps["hook"] == []
    (reader,) = steps["reader"]
    assert reader.argv == arms.session_argv("copilot", CASES["H1"].reader_prompt, MODELS["copilot"], "blank")
    assert arms.reader_prompt(CASES["H1"], "blank") == CASES["H1"].reader_prompt
    assert arms.has_writer("blank") is False and arms.ARMS[-1] == "blank"
    assert arms.planned_sessions(CASES["H3"], "blank") == 1


@needs_atticdb
def test_blank_leak_flags_token_in_reply(env, monkeypatch, tmp_path):
    replay = tmp_path / "reply.jsonl"
    replay.write_text(claude_stream("LEDG-7731"))
    monkeypatch.setenv("STUB_REPLAY", str(replay))
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "blank", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.leaked is True and res.sessions == 1 and res.captured is None
    assert results.row("2026-11-05", res)["leaked"] == "1"
    (call,) = agent_calls(env["log"])
    assert call["prog"] == "claude" and call["argv"][1] == CASES["H1"].reader_prompt
    assert_cleaned_up(env)

    # The stub reader with nothing to read replies without a token.
    monkeypatch.delenv("STUB_REPLAY")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "blank", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.leaked is False and res.delivered is False and res.passed is False
    assert results.row("2026-11-05", res)["leaked"] == "0"

    # Only the blank arm measures a leak.
    for arm in ("native", "oracle"):
        res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, "codex", "claude-code", 1)
        assert not res.errored, res.reasons
        assert res.leaked is None and results.row("2026-11-05", res)["leaked"] == ""
    assert_cleaned_up(env)


@needs_atticdb
@pytest.mark.parametrize("cid", ["H1", "H5"])
def test_blank_leak_flags_delivery(env, monkeypatch, tmp_path, cid):
    """A token in the reader's tool output is a leak, even when the reply doesn't cite it. H5 has no
    delivery stage, but its blank reader's transcript is still read for a leak."""
    replay = tmp_path / "reply.jsonl"
    replay.write_text(claude_stream("I have no record of that.", tool_result="see ticket LEDG-7731"))
    monkeypatch.setenv("STUB_REPLAY", str(replay))
    res = run_handoff(make_cfg(env), Runner(), CASES[cid], "blank", "codex", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.leaked is True
    if cid == "H1":
        assert res.delivered is True and res.delivered_where == "transcript[LEDG-7731]"
    else:
        assert res.delivered is None and res.delivered_where == ""
    assert_cleaned_up(env)


def test_atticdb_only_env_on_every_session(env):
    cfg = make_cfg(env, models=dict(MODELS))
    steps = run.plan(cfg, CASES["H3"], "atticdb-only", "claude-code", "claude-code", "/tmp/zhe-root")
    sessions = steps["writer"] + steps["reader"]
    assert len(sessions) == 3
    for step in sessions:
        assert step.extra_env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1"
    steps = run.plan(cfg, CASES["H3"], "atticdb", "claude-code", "claude-code", "/tmp/zhe-root")
    for step in steps["writer"] + steps["reader"]:
        assert "CLAUDE_CODE_DISABLE_AUTO_MEMORY" not in step.extra_env
    assert arms.session_env("codex", "atticdb-only") == {} and arms.session_env("copilot", "atticdb-only") == {}
    assert arms.native_memory_off("atticdb-only") is True
    assert not any(arms.native_memory_off(a) for a in arms.ARMS if a != "atticdb-only")
    assert arms.ARMS[-2:] == ("atticdb-only", "blank")
    assert "atticdb-only" not in arms.DEFAULT_ARMS


def test_atticdb_only_codex_argv():
    argv = arms.session_argv("codex", "p", "m", "atticdb-only")
    assert argv[-5:] == ["--disable", "memories", "-m", "m", "p"]
    assert agents.CODEX_APPROVE in argv
    assert "--disable" not in arms.session_argv("codex", "p", "m", "atticdb")
    claude = arms.session_argv("claude-code", "p", "m", "atticdb-only")
    assert claude == arms.session_argv("claude-code", "p", "m", "atticdb")


def test_inherited_memory_switch_is_dropped(env):
    """A CLAUDE_CODE_DISABLE_AUTO_MEMORY from the owner's shell would make atticdb and atticdb-only
    the same. Only the arm sets it."""
    cfg = make_cfg(env)
    cfg.base_env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] = "1"
    root, hroot = "/tmp/zhe-root", "/tmp/zhh-root"
    for arm in ("native", "atticdb"):
        steps = run.plan(cfg, CASES["H1"], arm, "codex", "claude-code", root, hroot)
        for phase in ("disconnect", "setup", "writer", "hook", "reader", "teardown"):
            for step in steps[phase]:
                assert "CLAUDE_CODE_DISABLE_AUTO_MEMORY" not in run.env_for(cfg, root, step, hroot), (arm, phase)
    steps = run.plan(cfg, CASES["H1"], "atticdb-only", "codex", "claude-code", root, hroot)
    (reader,) = steps["reader"]
    assert run.env_for(cfg, root, reader, hroot)["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1"
    (writer,) = steps["writer"]
    assert "CLAUDE_CODE_DISABLE_AUTO_MEMORY" not in run.env_for(cfg, root, writer, hroot)


def test_copilot_never_enables_memory():
    for arm in arms.ARMS:
        assert "--enable-memory" not in arms.session_argv("copilot", "p", "m", arm)


def test_atticdb_only_sets_up_atticdb(env):
    cfg = make_cfg(env, models=dict(MODELS))
    assert arms.uses_atticdb("atticdb-only") is True and arms.has_writer("atticdb-only") is True
    only = run.plan(cfg, CASES["H1"], "atticdb-only", "claude-code", "codex", "/tmp/zhe-root")
    full = run.plan(cfg, CASES["H1"], "atticdb", "claude-code", "codex", "/tmp/zhe-root")
    for phase in ("setup", "hook", "teardown"):
        assert only[phase] and only[phase] == full[phase], phase


@needs_atticdb
def test_atticdb_only_flags_native_memory_write(env, monkeypatch):
    """The stub ignores CLAUDE_CODE_DISABLE_AUTO_MEMORY, like a harness that ignores the switch."""
    monkeypatch.setenv("STUB_NATIVE_MEMORY", "1")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb-only", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert "claude-memory:MEMORY.md[LEDG-7731,412]" in res.captured_where
    assert "native memory written with memory off" in res.reasons
    assert_cleaned_up(env)

    # Without a memory write, no flag. The atticdb arm never sets it.
    monkeypatch.delenv("STUB_NATIVE_MEMORY")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb-only", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and "native memory written with memory off" not in res.reasons
    monkeypatch.setenv("STUB_NATIVE_MEMORY", "1")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb", "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert "native memory written with memory off" not in res.reasons
    assert_cleaned_up(env)


def test_default_arms_unchanged():
    a = cli.parse(["--cases", "H1", "--claude-model", "X", "--codex-model", "Y", "--dry-run"])
    assert a.arms == ["native", "files", "atticdb", "oracle"] == list(arms.DEFAULT_ARMS)
    a = cli.parse(["--cases", "H1", "--arms", "blank,native", "--claude-model", "X", "--codex-model", "Y", "--dry-run"])
    assert a.arms == ["blank", "native"]
    assert a.writer_sessions == 2 and a.reader_sessions == 4


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
    ("412", "the ticket cited 412 B", True), ("412", "412B header", True), ("412", "412 vs 24 bytes", True),
    ("412", "412 → 24 B", True), ("412", "412 to 24-byte headers", True), ("412", "HTTP 412 Bad Request", False),
    ("412", "412 vs 24 rows", False),
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


# ---- Copilot CLI ----

COPILOT_DIRECTIONS = [("claude-code", "copilot"), ("copilot", "claude-code")]


@needs_atticdb
def test_copilot_writer_captures_and_claude_reader_gets_delivery(env):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb", "copilot", "claude-code", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and "atticdb:fact repo/evalrepo/decision-" in res.captured_where
    assert res.delivered is True and res.delivered_where.startswith("hook[LEDG-7731,412]")
    assert res.passed is True and res.sessions == 2
    assert (res.writer_model, res.reader_model) == ("copilot-z", "claude-x")
    # The writer ran AtticDB's sessionStart hook from its own COPILOT_HOME. The store was empty then.
    assert "copilot writer: sessionStart hook ran in 1 of 1 sessions, block has no evidence token" in res.reasons
    assert "writer 1 premium_requests=1" in res.reasons and res.premium_requests == 1
    writer, reader = agent_calls(env["log"])
    assert writer["prog"] == "copilot" and writer["argv"][:2] == ["-p", CASES["H1"].writer_prompts[0]]
    assert writer["argv"][2:] == ["--output-format", "json", "--allow-all-tools", "--no-auto-update",
                                  "--model", "copilot-z"]
    assert {"mcp-config.json", "hooks", "instructions"} <= set(writer["copilot_home_files"])
    # Only the run's own COPILOT_HOME reached a child, and no inherited COPILOT_* or GitHub token did.
    for call in stub_calls(env["log"]):
        assert call["copilot_env"] in ([], ["COPILOT_HOME"]), call["copilot_env"]
    # Codex was not set up: the run's harnesses are its direction's 2.
    assert not any(c["prog"] == "codex" for c in stub_calls(env["log"]))
    assert_cleaned_up(env)


@needs_atticdb
def test_copilot_reader_gets_the_hook(env):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "atticdb", "claude-code", "copilot", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and res.delivered is True and res.passed is True
    assert res.delivered_where.startswith("hook[LEDG-7731,412]")
    assert "copilot reader: sessionStart hook ran in 1 of 1 sessions, block has [LEDG-7731,412]" in res.reasons
    assert_cleaned_up(env)


@needs_atticdb
def test_files_arm_copilot_reader_reads_handoff(env):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "claude-code", "copilot", 1)
    assert not res.errored, res.reasons
    assert res.captured is True and res.captured_where == "repo:HANDOFF.md[LEDG-7731,412]"
    assert res.delivered is True and res.delivered_where == "transcript[LEDG-7731,412]"
    assert res.passed is True
    # No atticdb hook ran in this arm.
    assert not any("sessionStart hook" in r for r in res.reasons)
    assert_cleaned_up(env)


@needs_atticdb
@pytest.mark.parametrize("arm", ["native", "files"])
@pytest.mark.parametrize("writer,reader", COPILOT_DIRECTIONS)
def test_copilot_reader_grep_finds_no_writer_transcript(env, monkeypatch, arm, writer, reader):
    """A Copilot writer's session-store.db holds its prompts. It goes before the reader starts."""
    monkeypatch.setenv("STUB_READER_GREP", "1")
    monkeypatch.setenv("STUB_WRITER", "silent")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, writer, reader, 1)
    assert not res.errored, res.reasons
    assert res.captured is False and res.delivered is False
    if writer == "copilot":
        _, rd = agent_calls(env["log"])
        assert rd["copilot_home_files"] == [], "the copilot writer's dir is empty once it is done"
    monkeypatch.setattr(run, "scrub_harness_dir", lambda hroot, harness: [])
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, writer, reader, 1)
    assert res.delivered is True and res.delivered_where == "transcript[LEDG-7731,412]"


@needs_atticdb
def test_copilot_home_is_per_run_and_removed(env):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "copilot", "claude-code", 1)
    assert not res.errored, res.reasons
    homes = {c["dirs"]["COPILOT_HOME"] for c in stub_calls(env["log"])}
    (home,) = homes
    assert home.startswith(env["tmp"] + "/") and home.endswith("/copilot")
    assert not home.startswith(env["home"]), "never the user's ~/.copilot"
    assert not os.path.exists(home) and not os.path.exists(os.path.dirname(home))
    assert not os.path.exists(os.path.join(env["home"], ".copilot")), "the fake ~/.copilot is never made"
    # The next run gets a new one.
    run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "copilot", "claude-code", 1)
    assert len({c["dirs"]["COPILOT_HOME"] for c in stub_calls(env["log"])}) == 2
    assert_cleaned_up(env)


def test_guard_refuses_users_copilot_home(tmp_path):
    home = tmp_path / "home"
    (home / ".copilot").mkdir(parents=True)
    claude, codex = guard.eval_dirs(str(home))
    for bad in (home / ".copilot", home / ".copilot" / "run", home / ".COPILOT"):
        with pytest.raises(guard.Refused, match="COPILOT_HOME"):
            guard.check_config_dirs(claude, codex, str(home), copilot_dir=str(bad))
    link = tmp_path / "elsewhere"
    link.symlink_to(home / ".copilot")
    with pytest.raises(guard.Refused, match="COPILOT_HOME"):
        guard.check_config_dirs(claude, codex, str(home), copilot_dir=str(link))
    with pytest.raises(guard.Refused, match="must differ"):
        guard.check_config_dirs(claude, codex, str(home), copilot_dir=codex)
    guard.check_config_dirs(claude, codex, str(home), copilot_dir=str(tmp_path / "zhh-1" / "copilot"))


@needs_atticdb
def test_run_refuses_copilot_home_in_the_users_dir(env, monkeypatch):
    user_copilot = os.path.join(env["home"], ".copilot")
    real = run.harness_dir
    monkeypatch.setattr(run, "harness_dir", lambda hroot, h: user_copilot if h == "copilot" else real(hroot, h))
    with pytest.raises(guard.Refused, match="COPILOT_HOME .* ~/.copilot"):
        run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "copilot", "claude-code", 1)
    assert agent_calls(env["log"]) == [] and not os.path.exists(user_copilot)


def test_child_env_sets_only_the_runs_copilot_home():
    base = {"PATH": "/bin", "COPILOT_HOME": "/u/.copilot", "COPILOT_GITHUB_TOKEN": "t1", "COPILOT_MODEL": "m",
            "COPILOT_ALLOW_ALL": "true", "GH_TOKEN": "t2", "GITHUB_TOKEN": "t3", "GH_HOST": "x.ghe.com"}
    kw = dict(home="/h", atticdb_home="/r/zh", claude_dir="/e/claude", codex_dir="/e/codex")
    env = agents.child_env(base, copilot_dir="/t/zhh-1/copilot", **kw)
    assert {k for k in env if k.startswith(("COPILOT_", "GH_", "GITHUB_"))} == {"COPILOT_HOME"}
    assert env["COPILOT_HOME"] == "/t/zhh-1/copilot"
    # COPILOT_* never passes, even with --keep-api-keys; the GitHub vars do then.
    kept = agents.child_env(base, copilot_dir="/t/zhh-1/copilot", keep_api_keys=True, **kw)
    assert kept["GH_TOKEN"] == "t2" and "COPILOT_GITHUB_TOKEN" not in kept
    # Without a copilot dir (setup_eval), no COPILOT_HOME at all.
    assert "COPILOT_HOME" not in agents.child_env(base, **kw)
    with pytest.raises(ValueError):
        agents.child_env(base, copilot_dir="/t/c", extra={"COPILOT_HOME": "/u/.copilot"}, **kw)
    with pytest.raises(ValueError):
        agents.child_env(base, copilot_dir="relative/copilot", **kw)


@needs_atticdb
def test_vscode_atticdb_entry_stops_a_copilot_setup(env):
    cfg = make_cfg(env)
    path = run.vscode_mcp_file(cfg)
    os.makedirs(os.path.dirname(path))
    with open(path, "w") as f:
        f.write('{"servers": {"atticdb": {"command": "/x/atticdb"}}}\n')
    before = os.stat(path).st_mtime_ns
    res = run_handoff(cfg, Runner(), CASES["H1"], "atticdb", "claude-code", "copilot", 1)
    assert res.errored and res.error_stage == "setup" and res.sessions == 0
    assert any("has an atticdb entry, and setting up copilot would remove it" in r for r in res.reasons)
    assert os.stat(path).st_mtime_ns == before
    # Other arms set nothing up, so they run.
    res = run_handoff(cfg, Runner(), CASES["H1"], "files", "claude-code", "copilot", 1)
    assert not res.errored, res.reasons
    assert_cleaned_up(env)


def test_copilot_directions_need_only_their_models(tmp_path, capsys, monkeypatch):
    base = ["--cases", "H1", "--arms", "atticdb", "--directions", "claude-code:copilot", "--out", str(tmp_path / "r.csv")]
    with pytest.raises(SystemExit) as e:
        cli.parse(base + ["--dry-run", "--claude-model", "X", "--codex-model", "Y"])
    assert e.value.code == 2 and "--copilot-model is required" in capsys.readouterr().err
    a = cli.parse(base + ["--dry-run", "--claude-model", "X", "--copilot-model", "Z"])
    assert a.models == {"claude-code": "X", "codex": "", "copilot": "Z"}
    with pytest.raises(SystemExit):
        cli.parse(base[:-4] + ["--directions", "copilot:copilot", "--copilot-model", "Z", "--dry-run"])
    assert "the pilot is cross-harness only" in capsys.readouterr().err

    env = fake_home(monkeypatch, tmp_path)
    parent = tempfile.mkdtemp(prefix="zhe-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    try:
        args = ["--cases", "H1,H2", "--arms", "native,files,atticdb,oracle", "--directions",
                "claude-code:copilot,copilot:claude-code", "--claude-model", "X", "--copilot-model", "Z",
                "--dry-run", "--out", str(tmp_path / "r.csv"), "--atticdb", "/nonexistent/atticdb"]
        assert cli.main(args) == 0
        text = capsys.readouterr().out
        assert stub_calls(env["log"]) == [] and os.listdir(parent) == []
    finally:
        shutil.rmtree(parent, ignore_errors=True)
    # 2 cases x 4 arms x 2 directions; H1 and H2 have 1 writer each, and the oracle none.
    assert "Total: 28 agent sessions (12 writer, 16 reader) in 16 runs." in text
    for absent in ("codex exec", "disconnect codex", "--harness codex", "Overrides for codex", "auth.json"):
        assert absent not in text, absent
    assert "Overrides for copilot: --allow-all-tools" in text
    assert "copilot -p" in text and "--model Z" in text
    assert "setup --yes --harness copilot" in text and "hook session-start" in text
    assert "an atticdb entry in the real VS Code user mcp.json stops the run" in text
    assert "Copilot's setup and disconnects run with HOME=<harness-root>/home" in text
    assert "setup --yes --harness copilot\n      timeout 120 s; env: base + HOME, XDG_CONFIG_HOME" in text


def test_copilot_tool_outputs_skip_writes_and_own_messages():
    stream = [
        {"type": "user.message", "data": {"content": "the prompt names FIN-88"}},
        {"type": "assistant.message", "data": {"content": "I recall STOR-5150", "toolRequests": [
            {"toolCallId": "c1", "name": "create", "arguments": {"path": "n.md", "file_text": "LEDG-7731"}}]}},
        {"type": "tool.execution_start", "data": {"toolCallId": "c1", "toolName": "create",
                                                  "arguments": {"file_text": "LEDG-7731"}}},
        {"type": "tool.execution_complete", "data": {"toolCallId": "c1", "success": True,
                                                     "result": {"content": "Created n.md: LEDG-7731"}}},
        {"type": "tool.execution_start", "data": {"toolCallId": "c2", "toolName": "bash",
                                                  "arguments": {"command": "grep bramble-ci"}}},
        {"type": "tool.shell_output", "data": {"toolCallId": "c2", "text": "INC-2209 streamed"}},
        {"type": "tool.execution_complete", "data": {"toolCallId": "c2", "success": True,
                                                     "result": {"content": "INC-2209", "detailedContent": "INC-2209"}}},
        {"type": "assistant.message", "data": {"content": "Done, see FIN-88.", "phase": "final_answer",
                                               "toolRequests": []}},
        {"type": "tool.execution_complete", "data": {"toolCallId": "c3", "result": {"content": "STOR-5150"}}},
    ]
    outputs = "\n".join(run.tool_outputs("copilot", "\n".join(map(json.dumps, stream)).encode()))
    assert run.tokens_in(outputs, ("LEDG-7731", "FIN-88", "INC-2209", "STOR-5150", "bramble-ci")) == ["INC-2209"]


def test_copilot_stream_errors():
    assert run.stream_errors("copilot", b'{"type":"session.error","data":{"message":"quota exceeded"}}\n'
                                        b'{"type":"result","exitCode":1,"usage":{"premiumRequests":0}}\n') == [
        "quota exceeded", "exitCode 1"]
    assert run.stream_errors("copilot", b'{"type":"result","exitCode":0}\n') == []


@needs_atticdb
def test_copilot_bad_model_is_errored(env, monkeypatch, tmp_path):
    """A bad --model makes Copilot print to stderr and exit 1 with no result line."""
    replay = tmp_path / "empty.jsonl"
    replay.write_text('{"type":"session.mcp_server_tools_loaded","data":{"serverName":"atticdb","toolCount":8}}\n')
    monkeypatch.setenv("STUB_REPLAY", str(replay))
    monkeypatch.setenv("STUB_STDERR", 'Error: Model "nope" from --model flag is not available.')
    monkeypatch.setenv("STUB_EXIT_AFTER", "1")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "native", "copilot", "claude-code", 1)
    assert res.errored and res.error_stage == "writer"
    assert ('error in writer: writer 1 exited 1: stderr: Error: Model "nope" from --model flag is not available.'
            in res.reasons), res.reasons
    assert_cleaned_up(env)


def test_copilot_setup_steps_use_a_per_run_home(tmp_path, monkeypatch):
    """`atticdb connect copilot` finds VS Code's user mcp.json through HOME or XDG_CONFIG_HOME. The copilot
    setup and disconnect steps get a per-run HOME, so the real file is out of reach. Agent sessions and
    the other harnesses' steps keep the real HOME."""
    env = fake_home(monkeypatch, tmp_path)
    cfg = make_cfg(dict(env, tmp=str(tmp_path)))
    root, hroot = str(tmp_path / "zse-1"), str(tmp_path / "zhh-1")
    real_vscode = run.vscode_mcp_file(cfg)
    steps = run.plan(cfg, CASES["H1"], "atticdb", "claude-code", "copilot", root, hroot)
    copilot_steps = [s for phase in ("disconnect", "setup", "teardown") for s in steps[phase] if s.argv[-1] == "copilot"]
    assert len(copilot_steps) == 3
    for step in copilot_steps:
        e = run.env_for(cfg, root, step, hroot)
        assert e["HOME"] == os.path.join(hroot, "home") and e["XDG_CONFIG_HOME"] == os.path.join(hroot, "home", ".config")
        # The paths atticdb's Harness::file would take, on macOS and elsewhere.
        for vscode in (os.path.join(e["HOME"], "Library", "Application Support", "Code", "User", "mcp.json"),
                       os.path.join(e["XDG_CONFIG_HOME"], "Code", "User", "mcp.json")):
            assert vscode != real_vscode and vscode.startswith(hroot + os.sep) and not vscode.startswith(env["home"])
        assert e["COPILOT_HOME"] == os.path.join(hroot, "copilot")
    for step in [s for phase in ("disconnect", "setup", "writer", "reader") for s in steps[phase]
                 if s.argv[-1] != "copilot"]:
        assert run.env_for(cfg, root, step, hroot)["HOME"] == env["home"]


@needs_atticdb
def test_copilot_setup_leaves_the_real_vscode_file_alone(env):
    cfg = make_cfg(env)
    path = run.vscode_mcp_file(cfg)
    os.makedirs(os.path.dirname(path))
    with open(path, "w") as f:
        f.write('{"servers": {"other": {"command": "x"}}}\n')
    before = (os.stat(path).st_mtime_ns, open(path).read())
    res = run_handoff(cfg, Runner(), CASES["H1"], "atticdb", "claude-code", "copilot", 1)
    assert not res.errored, res.reasons
    assert res.delivered is True and res.passed is True
    assert (os.stat(path).st_mtime_ns, open(path).read()) == before
    assert_cleaned_up(env)


@needs_atticdb
def test_copilot_skill_note(env, monkeypatch):
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "claude-code", "copilot", 1)
    assert not res.errored and not any("skills offered" in r for r in res.reasons), res.reasons
    monkeypatch.setenv("STUB_COPILOT_SKILL", "activitykit")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "copilot", "claude-code", 1)
    assert not res.errored, res.reasons
    assert "copilot writer: 1 non-builtin skills offered (activitykit)" in res.reasons
    assert_cleaned_up(env)


def test_copilot_skill_note_flags_a_missing_skills_list(tmp_path):
    """Builtins are always offered, so 0 skills means the list wasn't found, not a clean setup."""
    assert run.copilot_skill_note(str(tmp_path), "reader") == f"{run.SKILLS_NOT_FOUND} (reader)"
    log = tmp_path / run.HARNESS_DIRS["copilot"] / "session-state" / "s1" / "events.jsonl"
    log.parent.mkdir(parents=True)
    log.write_text(json.dumps({"type": "system.message", "data": {"content": "You are Copilot."}}) + "\n")
    assert run.copilot_skill_note(str(tmp_path), "writer") == f"{run.SKILLS_NOT_FOUND} (writer)"
    log.write_text(json.dumps({"type": "system.message", "data": {"content": (
        "<skill>\n<name>github-pr-media</name>\n<location>builtin</location>\n</skill>")}}) + "\n")
    assert run.copilot_skill_note(str(tmp_path), "writer") is None


@needs_atticdb
def test_copilot_skill_note_when_the_session_lists_no_skills(env, monkeypatch):
    monkeypatch.setenv("STUB_COPILOT_NO_SKILLS", "1")
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], "files", "claude-code", "copilot", 1)
    assert not res.errored, res.reasons
    assert f"{run.SKILLS_NOT_FOUND} (reader)" in res.reasons
    assert_cleaned_up(env)


# ---- captured with reason, and the whole reply ----

@needs_atticdb
@pytest.mark.parametrize("arm,want", [("atticdb", True), ("files", True), ("native", False), ("oracle", None)])
def test_captured_all_and_the_whole_reply(env, arm, want):
    """The stub writer saves its whole prompt in 1 fact or 1 HANDOFF.md line, so LEDG-7731 and 412 sit
    together. Native has no capture path here. The oracle has no capture."""
    res = run_handoff(make_cfg(env), Runner(), CASES["H1"], arm, "claude-code", "codex", 1)
    assert not res.errored, res.reasons
    assert res.captured_all is want
    row = results.row("2026-11-04", res)
    assert row["captured_all"] == results.flag(want)
    stub_reply = "JSON headers were 412 bytes and struct cut them to 24 bytes, per ticket LEDG-7731."
    want_reply = stub_reply if arm in ("atticdb", "files", "oracle") else "I have no record of that."
    assert res.reply == row["reply"] == want_reply
    assert_cleaned_up(env)


@needs_atticdb
def test_batch_refuses_a_csv_with_old_columns(env, tmp_path, capsys):
    out = tmp_path / "old.csv"
    out.write_text("date,case,arm,reasons\n2026-11-04,H1,files,reply=x\n")
    before = out.read_text()
    args = ["--cases", "H1", "--arms", "files", "--directions", "claude-code:codex", "--claude-model", "X",
            "--codex-model", "Y", "--atticdb", ATTICDB, "--max-sessions", "2", "--out", str(out)]
    assert cli.main(args) == 2
    assert "has other columns than this version writes" in capsys.readouterr().err
    assert stub_calls(env["log"]) == [] and out.read_text() == before
    assert not (tmp_path / "old.md").exists()
