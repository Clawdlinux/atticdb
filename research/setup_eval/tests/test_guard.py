import builtins
import os
import sys

import pytest

from research.setup_eval import guard
from research.setup_eval.proc import Runner

from .helpers import fake_home, stub_calls


@pytest.fixture
def home(tmp_path):
    h = tmp_path / "home"
    h.mkdir()
    return str(h)


def test_eval_dirs_pass(home):
    claude, codex = guard.eval_dirs(home)
    os.makedirs(claude)
    os.makedirs(codex)
    guard.check_config_dirs(claude, codex, home)


@pytest.mark.parametrize("name", [".claude", ".claude-work", ".claude.json", ".codex", ".copilot",
                                  ".codex/sub", ".Claude"])
def test_refuses_the_users_own_config(home, name):
    _, codex = guard.eval_dirs(home)
    with pytest.raises(guard.Refused, match="user's own config"):
        guard.check_config_dirs(os.path.join(home, name), codex, home)
    claude, _ = guard.eval_dirs(home)
    with pytest.raises(guard.Refused, match="user's own config"):
        guard.check_config_dirs(claude, os.path.join(home, name), home)


def test_refuses_an_eval_dir_that_links_to_claude(home):
    os.makedirs(os.path.join(home, ".claude"))
    claude, codex = guard.eval_dirs(home)
    os.makedirs(os.path.dirname(claude))
    os.symlink(os.path.join(home, ".claude"), claude)
    with pytest.raises(guard.Refused, match=r"~/\.claude"):
        guard.check_config_dirs(claude, codex, home)


def test_refuses_an_eval_root_that_links_into_codex(home):
    os.makedirs(os.path.join(home, ".codex", "x"))
    os.makedirs(os.path.join(home, ".cache"))
    os.symlink(os.path.join(home, ".codex", "x"), os.path.join(home, ".cache", "zerodb-setup-eval"))
    claude, codex = guard.eval_dirs(home)
    with pytest.raises(guard.Refused):
        guard.check_config_dirs(claude, codex, home)


def test_refuses_home_relative_and_shared(home):
    claude, codex = guard.eval_dirs(home)
    with pytest.raises(guard.Refused):
        guard.check_config_dirs(home, codex, home)
    with pytest.raises(guard.Refused, match="absolute"):
        guard.check_config_dirs("rel/claude", codex, home)
    with pytest.raises(guard.Refused, match="differ"):
        guard.check_config_dirs(claude, claude, home)


def test_check_env(tmp_path):
    root = str(tmp_path / "r")
    os.makedirs(root)
    guard.check_env({"ZERODB_HOME": os.path.join(root, "zh")}, root)
    for env in ({}, {"ZERODB_HOME": ""}, {"ZERODB_HOME": "zh"}, {"ZERODB_HOME": str(tmp_path / "other")},
                {"ZERODB_HOME": root + "-sibling/zh"}):
        with pytest.raises(guard.UnsafeEnv):
            guard.check_env(env, root)


def test_runner_refuses_before_starting_anything(monkeypatch, tmp_path):
    paths = fake_home(monkeypatch, tmp_path)
    root = tmp_path / "root"
    root.mkdir()
    runner = Runner()
    with pytest.raises(guard.UnsafeEnv):
        runner.run(["claude", "auth", "status"], env=dict(os.environ), root=str(root), cwd=str(root), timeout=5)
    assert runner.children == 0 and stub_calls(paths["log"]) == []
    env = dict(os.environ, ZERODB_HOME=str(root / "zh"))
    r = runner.run(["claude", "auth", "status"], env=env, root=str(root), cwd=str(root), timeout=20)
    assert r.code == 0 and runner.children == 1
    assert stub_calls(paths["log"])[0]["zerodb_home"]


def test_runner_kills_on_timeout(monkeypatch, tmp_path):
    fake_home(monkeypatch, tmp_path)
    root = tmp_path / "root"
    root.mkdir()
    env = dict(os.environ, ZERODB_HOME=str(root / "zh"), STUB_SLEEP="30")
    r = Runner().run(["claude", "-p", "x"], env=env, root=str(root), cwd=str(root), timeout=1)
    assert r.timed_out and r.code is None and r.wall_s < 15


def test_real_data_dir_follows_home():
    env = {"HOME": "/h", "XDG_DATA_HOME": "/x"}
    if sys.platform == "darwin":
        assert guard.real_data_dir(env) == "/h/Library/Application Support/zerodb/data"
    else:
        assert guard.real_data_dir(env) == "/x/zerodb/data"
        assert guard.real_data_dir({"HOME": "/h", "XDG_DATA_HOME": "rel"}) == "/h/.local/share/zerodb/data"


def test_snapshot_stats_without_opening(tmp_path, monkeypatch):
    data = tmp_path / "data"
    (data / "sub").mkdir(parents=True)
    (data / "a").write_bytes(b"1")
    (data / "sub" / "b").write_bytes(b"22")
    real_open = builtins.open

    def no_open(*a, **k):
        raise AssertionError("snapshot must not open files")

    monkeypatch.setattr(builtins, "open", no_open)
    monkeypatch.setattr(os, "open", no_open)
    before = guard.snapshot(str(data))
    monkeypatch.setattr(builtins, "open", real_open)
    assert set(before) == {"a", os.path.join("sub", "b")}
    (data / "a").write_bytes(b"111")
    (data / "c").write_bytes(b"")
    assert guard.compare(before, guard.snapshot(str(data))) == ["a", "c"]
    assert guard.snapshot(str(tmp_path / "missing")) == {}


def test_redact():
    text = ("key sk-ant-api03-abcdefghijklmnopqrstuv token=hunter2 Bearer abc.def "
            "ghp_abcdefghijklmnopqrstuvwxyz0123 hex 0123456789abcdef0123456789abcdef at /tmp/zse-1/zh")
    out = guard.redact(text, {"/tmp/zse-1": "<root>"})
    for secret in ("sk-ant", "hunter2", "abc.def", "ghp_", "0123456789abcdef0123"):
        assert secret not in out
    assert "<root>/zh" in out and "token=[redacted]" in out
    assert len(guard.excerpt("word " * 100)) == 200
    assert guard.excerpt("a\n\n b") == "a b"
