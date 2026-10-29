import builtins
import stat
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


def test_refuses_an_eval_dir_that_shares_a_target_with_any_claude_entry(home, tmp_path):
    # ~/.claude-alt links outside the home, and the eval dir links into the same place. No
    # name check can see it; file identity does.
    elsewhere = tmp_path / "elsewhere" / "cfg"
    (elsewhere / "sub").mkdir(parents=True)
    os.symlink(elsewhere, os.path.join(home, ".claude-alt"))
    claude, codex = guard.eval_dirs(home)
    os.makedirs(os.path.dirname(claude))
    os.symlink(elsewhere / "sub", claude)
    with pytest.raises(guard.Refused, match=r"~/\.claude-alt, the user's own config, overlap"):
        guard.check_config_dirs(claude, codex, home)


def test_refuses_a_claude_entry_that_links_into_the_eval_dir(home):
    claude, codex = guard.eval_dirs(home)
    os.makedirs(claude)
    os.makedirs(codex)
    os.symlink(claude, os.path.join(home, ".claude-work"))
    with pytest.raises(guard.Refused, match=r"~/\.claude-work"):
        guard.check_config_dirs(claude, codex, home)
    os.remove(os.path.join(home, ".claude-work"))
    os.symlink(os.path.join(codex, "deep"), os.path.join(home, ".copilot"))  # dangling, into the eval dir
    with pytest.raises(guard.Refused, match=r"~/\.copilot"):
        guard.check_config_dirs(claude, codex, home)


def test_unrelated_claude_entries_pass(home, tmp_path):
    claude, codex = guard.eval_dirs(home)
    os.makedirs(claude)
    os.makedirs(codex)
    os.makedirs(os.path.join(home, ".claude", "projects"))
    open(os.path.join(home, ".claude.json"), "w").close()
    os.symlink(tmp_path / "gone", os.path.join(home, ".claude-old"))  # dangling, elsewhere
    guard.check_config_dirs(claude, codex, home)


def test_refuses_when_a_claude_entry_cannot_be_inspected(home):
    claude, codex = guard.eval_dirs(home)
    loop = os.path.join(home, ".claude-loop")
    os.symlink(loop, loop)
    with pytest.raises(guard.Refused, match="can't be inspected"):
        guard.check_config_dirs(claude, codex, home)


@pytest.mark.skipif(os.geteuid() == 0, reason="root can list any directory")
def test_refuses_when_the_home_cannot_be_listed(home):
    claude, codex = guard.eval_dirs(home)
    os.makedirs(claude)
    os.makedirs(codex)
    os.chmod(home, 0o311)
    try:
        with pytest.raises(guard.Refused, match="cannot list the home"):
            guard.check_config_dirs(claude, codex, home)
    finally:
        os.chmod(home, 0o755)


def test_check_env_resolves_links_first(tmp_path):
    root, outside = tmp_path / "r", tmp_path / "out"
    root.mkdir()
    outside.mkdir()
    os.symlink(outside, root / "zh")
    with pytest.raises(guard.UnsafeEnv):
        guard.check_env({"ZERODB_HOME": str(root / "zh")}, str(root))
    os.symlink(outside, root / "link")
    with pytest.raises(guard.UnsafeEnv):
        guard.check_env({"ZERODB_HOME": str(root / "link" / "zh")}, str(root))
    with pytest.raises(guard.UnsafeEnv):
        guard.check_env({"ZERODB_HOME": str(root)}, str(root))
    (root / "real").mkdir()
    guard.check_env({"ZERODB_HOME": str(root / "real" / "zh")}, str(root))
    # A root reached through a link still holds its own home.
    os.symlink(root, tmp_path / "alias")
    guard.check_env({"ZERODB_HOME": str(tmp_path / "alias" / "real" / "zh")}, str(root))


def test_config_snapshot_stats_the_top_level_without_opening(home, monkeypatch, tmp_path):
    os.makedirs(os.path.join(home, ".claude", "projects", "deep"))
    with open(os.path.join(home, ".claude", "settings.json"), "w") as f:
        f.write("{}")
    with open(os.path.join(home, ".claude.json"), "w") as f:
        f.write("{}")
    os.makedirs(os.path.join(home, ".codex"))
    os.makedirs(os.path.join(home, ".unrelated"))
    elsewhere = tmp_path / "linked"
    elsewhere.mkdir()
    (elsewhere / "a").write_text("x")
    os.symlink(elsewhere, os.path.join(home, ".copilot"))

    def no_open(*a, **k):
        raise AssertionError("config_snapshot must not open files")

    monkeypatch.setattr(builtins, "open", no_open)
    monkeypatch.setattr(os, "open", no_open)
    before = guard.config_snapshot(home)
    monkeypatch.undo()
    assert sorted(before) == ["~/.claude", "~/.claude.json", "~/.claude/projects", "~/.claude/settings.json",
                              "~/.codex", "~/.copilot", "~/.copilot (link target)", "~/.copilot/a"], \
        "top level only, through a link"
    assert before["~/.claude/settings.json"][1] == 2
    with open(os.path.join(home, ".claude", "settings.json"), "w") as f:
        f.write('{"x": 1}')
    with open(os.path.join(home, ".claude", "projects", "deep", "ignored"), "w") as f:
        f.write("deeper than the top level")
    os.makedirs(os.path.join(home, ".claude-new"))
    changed = guard.compare(before, guard.config_snapshot(home))
    assert "~/.claude/settings.json" in changed and "~/.claude-new" in changed
    assert not any("ignored" in c or "unrelated" in c for c in changed)


def test_watched_data_dirs_add_the_parent_zerodb_home():
    env = {"HOME": "/h"}
    assert [label for label, _ in guard.watched_data_dirs(env)] == ["the real zerodb home's data dir"]
    dirs = guard.watched_data_dirs(dict(env, ZERODB_HOME="/z/home"))
    assert dirs[1] == ("$ZERODB_HOME/data of the parent env", "/z/home/data")
    assert "/z/home" not in dirs[1][0], "the label holds no env value"
    assert len(guard.watched_data_dirs(dict(env, ZERODB_HOME="rel"))) == 1
    assert len(guard.watched_data_dirs(dict(env, ZERODB_HOME=""))) == 1


def test_config_snapshot_sees_writes_behind_a_link(home, tmp_path):
    target = tmp_path / "dotfiles" / "claude.json"
    target.parent.mkdir()
    target.write_text("{}")
    os.symlink(target, os.path.join(home, ".claude.json"))
    real_dir = tmp_path / "dotfiles" / "claude"
    real_dir.mkdir()
    (real_dir / "settings.json").write_text("{}")
    os.makedirs(os.path.join(home, ".claude"))
    os.symlink(real_dir / "settings.json", os.path.join(home, ".claude", "settings.json"))
    before = guard.config_snapshot(home)
    assert before["~/.claude.json"][0] == stat.S_IFLNK, "the link's own stat"
    assert before["~/.claude.json (link target)"][:2] == (stat.S_IFREG, 2), "and its target's"
    assert "~/.claude/settings.json (link target)" in before, "links inside a dir too"
    # Writes behind both links leave the links' own stats as they were.
    target.write_text('{"a": 1}')
    (real_dir / "settings.json").write_text('{"b": 2}')
    changed = guard.compare(before, guard.config_snapshot(home))
    assert changed == ["~/.claude.json (link target)", "~/.claude/settings.json (link target)"]
