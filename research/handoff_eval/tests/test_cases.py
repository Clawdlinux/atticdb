import os
import subprocess

import pytest

from research.handoff_eval import fixture
from research.handoff_eval.cases import CASES, Case
from research.handoff_eval.grade import run_python
from research.setup_eval.agents import git_env


def test_six_cases_with_valid_kinds():
    assert sorted(CASES) == ["H1", "H2", "H3", "H4", "H5", "H6"]
    for cid, case in CASES.items():
        assert isinstance(case, Case) and case.id == cid
        assert case.kind in ("answer", "code")
        assert case.writer_prompts and case.reader_prompt and case.gold_note and case.evidence
    assert {c.id for c in CASES.values() if c.kind == "code"} == {"H2", "H6"}


def test_shared_prompts():
    assert CASES["H3"].writer_prompts[0] == CASES["H2"].writer_prompts[0]
    assert len(CASES["H3"].writer_prompts) == 2
    assert CASES["H5"].writer_prompts == CASES["H1"].writer_prompts
    assert CASES["H5"].evidence == CASES["H1"].evidence
    assert "retention" not in CASES["H5"].gold_note.lower()


def test_writer_prompts_never_mention_memory():
    for case in CASES.values():
        for p in case.writer_prompts:
            low = p.lower()
            assert "memory" not in low and "atticdb" not in low and "handoff" not in low, (case.id, p)


def test_evidence_is_in_writer_prompts_and_gold_note():
    for case in CASES.values():
        writer = " ".join(case.writer_prompts).lower()
        for tok in case.evidence:
            assert tok.lower() in writer, (case.id, tok)
            assert tok.lower() in case.gold_note.lower(), (case.id, tok)


def test_no_reader_prompt_leaks_evidence(tmp_path):
    repo = str(tmp_path / "repo")
    fixture.write_fixture(repo)
    texts = {f"reader:{c.id}": c.reader_prompt for c in CASES.values()}
    for name in fixture.FILES:
        with open(os.path.join(repo, name)) as f:
            texts[f"fixture:{name}"] = f.read()
    tokens = {tok.lower() for c in CASES.values() for tok in c.evidence}
    leaks = [(where, tok) for where, text in texts.items() for tok in tokens if tok in text.lower()]
    assert leaks == []


def test_fixture_is_one_commit_with_passing_tests(tmp_path):
    repo = str(tmp_path / "repo")
    fixture.write_fixture(repo)
    env = {"PATH": os.environ.get("PATH", os.defpath), **git_env()}
    log = subprocess.run(["git", "log", "--format=%s"], cwd=repo, env=env, capture_output=True, text=True)
    assert log.stdout.splitlines() == ["Initial commit"]
    status = subprocess.run(["git", "status", "--porcelain"], cwd=repo, env=env, capture_output=True, text=True)
    assert status.stdout == ""
    with open(os.path.join(repo, "README.md")) as f:
        assert len(f.read().splitlines()) == 1
    assert run_python(repo, fixture.TEST_LEDGER_PY) == (True, "")


def test_fixture_refuses_a_used_dir(tmp_path):
    (tmp_path / "left.txt").write_text("from a previous run")
    with pytest.raises(FileExistsError):
        fixture.write_fixture(str(tmp_path))
