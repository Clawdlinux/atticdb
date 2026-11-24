import hashlib
import json
import sys
from types import SimpleNamespace

import pytest

from research.longmemeval import run
from research.longmemeval.retrieval import evidence_recall, rerank
from research.longmemeval.run import context_prefix, judge_label, recall, select, select_ids


def test_context_budget_counts_the_joined_prompt_and_stops_at_overflow():
    docs = {"a": "aaa", "b": "bbbb", "c": "c"}
    assert context_prefix(["a", "a", "b", "c"], docs, len, 6) == (["a"], "aaa")
    assert context_prefix(["a", "b"], docs, len, 9) == (["a", "b"], "aaa\n\nbbbb")


def test_recall_counts_distinct_evidence_sessions():
    assert recall(["a", "b", "c"], {"a": "s1", "b": "s1", "c": "s3"}, {"s1", "s2"}) == 0.5
    assert recall([], {}, set()) is None


def test_aligned_chunks_keep_normal_turns_whole_and_track_long_turn_fragments():
    record = {
        "haystack_dates": ["2024-01-01"],
        "haystack_session_ids": ["s"],
        "haystack_sessions": [[
            {"role": "user", "content": "x" * 1300},
            {"role": "assistant", "content": "y" * 300, "has_answer": True},
            {"role": "user", "content": "z" * 1700, "has_answer": True},
        ]],
    }
    fixed, _, fixed_evidence = run.chunk_spans(record)
    aligned, sessions, evidence = run.chunk_spans(record, align=True)
    assert fixed == run.chunks(record)[0]
    assert fixed_evidence[0] == {"0", "1"}
    assert aligned["1"].endswith("assistant: " + "y" * 300)
    assert sessions == dict.fromkeys(("0", "1", "2", "3"), "s")
    assert evidence == [{"1"}, {"2", "3"}]
    assert evidence_recall(["1", "2"], evidence) == 0.5
    assert evidence_recall(["1", "2", "3"], evidence) == 1
    assert evidence_recall(["0"], []) is None


def test_rerank_promotes_exact_terms_and_preserves_input_order_for_ties():
    docs = {"substring": "catalog concatenate", "exact": "cat cat",
            "tie": "cat cat", "not_returned": "cat"}
    assert rerank(["substring", "tie", "exact"], docs, "Where is the cat?") == [
        "tie", "exact", "substring",
    ]


def test_pilot_selection_is_paired_and_marks_abstention_separately():
    records = [
        {"question_id": "a", "question_type": "knowledge-update"},
        {"question_id": "a_abs", "question_type": "knowledge-update"},
        {"question_id": "b", "question_type": "temporal-reasoning"},
        {"question_id": "c", "question_type": "multi-session"},
    ]
    assert [r["question_id"] for r in select(records, 1)] == ["a_abs", "a", "b"]


def test_only_valid_judge_outputs_become_scores():
    assert judge_label(" yes\n") is True
    assert judge_label("No") is False
    with pytest.raises(ValueError, match="judge"):
        judge_label("The answer looks fine")


def test_select_ids_keeps_exact_ids_in_requested_order():
    a, b, skip = ({"question_id": qid} for qid in ("a", "b", "b_abs"))
    assert select_ids([a, b, skip], ["b", "a"]) == [b, a]


@pytest.mark.parametrize(("dataset_ids", "ids", "message"), [
    (["a"], ["a", "a"], "duplicate --question-id"),
    (["a", "a"], ["a"], "duplicate dataset question ID: a"),
    (["a_abs"], ["a"], "unknown question IDs: a"),
])
def test_select_ids_rejects_ambiguous_or_missing_ids(dataset_ids, ids, message):
    with pytest.raises(ValueError, match=message):
        select_ids([{"question_id": qid} for qid in dataset_ids], ids)


def test_explicit_run_records_identity_and_refuses_existing_output(tmp_path, monkeypatch, capsys):
    dataset, tokenizer, binary, out = (tmp_path / name for name in ("data", "tok", "bin", "out"))
    dataset.write_text(json.dumps([{"question_id": qid, "haystack_dates": [],
                                    "haystack_session_ids": [], "haystack_sessions": []}
                                   for qid in ("a", "b", "skip")]))
    tokenizer.write_bytes(b"tokenizer")
    binary.write_bytes(b"binary")

    def sha(path):
        return hashlib.sha256(path.read_bytes()).hexdigest()

    monkeypatch.setattr(run, "DATA_SHA", sha(dataset))
    monkeypatch.setattr(run, "TOKENIZER_SHA", sha(tokenizer))
    replies = {
        "/api/tags": {"models": [{"name": name, "digest": f"test-{name}"}
                                 for name in (run.READER, run.EMBEDDER)]},
        "/api/version": {"version": "test"},
    }
    monkeypatch.setattr(run, "ollama", replies.__getitem__)
    monkeypatch.setitem(sys.modules, "tokenizers", SimpleNamespace(
        Tokenizer=SimpleNamespace(from_file=lambda _: object())))
    monkeypatch.setitem(sys.modules, "mem0", SimpleNamespace(
        Memory=SimpleNamespace(from_config=lambda _: object())))
    # Restore the environment even though main() updates it directly.
    for key in ("MEM0_DIR", "MEM0_TELEMETRY"):
        monkeypatch.setenv(key, "")

    def fake_measure(record, *_):
        qid = record["question_id"]
        row = {"question_id": qid, "arm": "fake", "correct": True}
        yield row, {"question_id": qid, "arm": "fake"}

    monkeypatch.setattr(run, "measure", fake_measure)
    monkeypatch.setattr(sys, "argv", ["run", "--dataset", str(dataset), "--tokenizer", str(tokenizer),
                                      "--atticdb", str(binary), "--out", str(out),
                                      "--question-id", "b", "--question-id", "a"])
    run.main()
    before = {p.name: p.read_bytes() for p in out.iterdir()}
    manifest = json.loads(before["run.json"])
    assert manifest["questions"] == ["b", "a"]
    assert manifest["atticdb_sha256"] == sha(binary)
    assert manifest["dataset_sha256"] == sha(dataset)
    assert manifest["tokenizer_sha256"] == sha(tokenizer)
    assert manifest["models"] == {m["name"]: m["digest"] for m in replies["/api/tags"]["models"]}
    assert [json.loads(line)["question_id"] for line in before["metrics.jsonl"].splitlines()] == ["b", "a"]

    with pytest.raises(SystemExit) as exc:
        run.main()
    assert exc.value.code == 2
    assert "--out must be a new directory" in capsys.readouterr().err
    assert {p.name: p.read_bytes() for p in out.iterdir()} == before
