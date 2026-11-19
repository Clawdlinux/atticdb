import pytest

from research.stale_rule import run


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
