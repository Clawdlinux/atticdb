import sys
import time

import pytest

from research.stale_rule import run


def test_child_env_keeps_provider_and_account_overrides_out(monkeypatch, tmp_path):
    overrides = ("ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_USE_BEDROCK",
                 "AWS_PROFILE", "GIT_DIR")
    for key in overrides:
        monkeypatch.setenv(key, "test-override")
    env = run.child_env(str(tmp_path))
    assert not set(overrides).intersection(env)
    assert env["CLAUDE_CONFIG_DIR"] == str(tmp_path)
    assert env["GIT_CONFIG_NOSYSTEM"] == "1"


@pytest.mark.parametrize(("channel", "header", "prefix"), [
    ("memory", "channel,neutral,", "memory,0,"),
    ("memory", "channel,", "memory,"),
    ("file", "", ""),
])
def test_resume_writes_to_current_directory(tmp_path, monkeypatch, channel, header, prefix):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(run, "LOGIN_DIR", str(tmp_path))
    source = tmp_path / "source.csv"
    source.write_text(f"{header}model,convention,history,arm,rep,error\n"
                      f"{prefix}claude-sonnet-5-5,errors,stale,plain,1,\n"
                      f"{prefix}claude-sonnet-5-5,errors,migration,plain,1,\n")

    def unexpected_run(*args):
        pytest.fail("completed cells must not be rerun")

    monkeypatch.setattr(run, "run_one", unexpected_run)
    assert run.main(["run", "--batch", "test", "--channel", channel,
                     "--model", "claude-sonnet-5-5", "--conventions", "errors",
                     "--arms", "plain", "--resume", "source.csv", "--out", "retry.csv"]) == 0
    assert len((tmp_path / "retry.csv").read_text().splitlines()) == 1


def test_retry_preserves_previous_transcript(tmp_path, monkeypatch):
    monkeypatch.setattr(run, "CACHE", str(tmp_path))

    def attempt(text):
        monkeypatch.setattr(run, "argv", lambda *args: [sys.executable, "-c", f"print({text!r})"])
        row = run.run_one("errors", "stale", "plain", 1, "stub", "batch", "file")
        assert not row["error"], row["error"]
        return tmp_path / "batch" / row["transcript"]

    first = attempt("first")
    retry = attempt("retry")
    assert first != retry
    assert first.read_text() == "first\n"
    assert retry.read_text() == "retry\n"


def test_timeout_stops_agent_child(tmp_path, monkeypatch):
    ready = tmp_path / "ready"
    late = tmp_path / "late"
    child = (
        "import pathlib,time; "
        f"pathlib.Path({str(ready)!r}).touch(); time.sleep(2); pathlib.Path({str(late)!r}).touch()"
    )
    parent = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',{child!r}]); time.sleep(5)"
    monkeypatch.setattr(run, "CACHE", str(tmp_path))
    monkeypatch.setattr(run, "TIMEOUT", 1)
    monkeypatch.setattr(run, "argv", lambda *args: [sys.executable, "-c", parent])
    assert run.run_one("errors", "stale", "plain", 1, "stub", "batch", "file")["error"] == "timeout"
    assert ready.exists(), "the child must start before the deadline"
    deadline = time.monotonic() + 2.5
    while time.monotonic() < deadline and not late.exists():
        time.sleep(0.05)
    assert not late.exists()


@pytest.mark.parametrize("flag", [
    ["--channel", "file"],
    ["--channel", "memory", "--neutral"],
    ["--channel", "memory", "--model", "different-model"],
])
def test_resume_rejects_a_different_experiment(tmp_path, monkeypatch, capsys, flag):
    source = tmp_path / "source.csv"
    source.write_text("channel,neutral,model,convention,history,arm,rep,error\n"
                      "memory,0,claude-sonnet-5-5,errors,stale,plain,1,\n")
    out = tmp_path / "out.csv"
    monkeypatch.setattr(run, "LOGIN_DIR", str(tmp_path))

    def unexpected_run(*args):
        pytest.fail("resume started an agent for a mismatched batch")

    monkeypatch.setattr(run, "run_one", unexpected_run)
    argv = ["run", "--batch", "test", "--conventions", "errors", "--arms", "plain",
            "--resume", str(source), "--out", str(out)]
    if "--model" not in flag:
        argv += ["--model", "claude-sonnet-5-5"]
    assert run.main(argv + flag) == 2
    assert "resume" in capsys.readouterr().err
    assert not out.exists()
