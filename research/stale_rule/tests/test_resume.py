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


def test_resume_writes_to_current_directory(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(run, "LOGIN_DIR", str(tmp_path))
    source = tmp_path / "source.csv"
    source.write_text("channel,neutral,model,convention,history,arm,rep,error\n"
                      "memory,0,claude-sonnet-5-5,errors,stale,plain,1,\n"
                      "memory,0,claude-sonnet-5-5,errors,migration,plain,1,\n")

    def unexpected_run(*args):
        pytest.fail("completed cells must not be rerun")

    monkeypatch.setattr(run, "run_one", unexpected_run)
    assert run.main(["run", "--batch", "test", "--channel", "memory",
                     "--model", "claude-sonnet-5-5", "--conventions", "errors",
                     "--arms", "plain", "--resume", "source.csv", "--out", "retry.csv"]) == 0
    assert len((tmp_path / "retry.csv").read_text().splitlines()) == 1


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
