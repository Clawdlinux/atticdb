"""The runner, the 6 arms and the outputs.

Tests that start a server need a real atticdb binary in ATTICDB_BIN, and skip without it.
Build one with: cargo build --locked -p atticdb. The rest run without it.
"""

from __future__ import annotations

import csv
import hashlib
import os
import platform
import random
import shutil
import tempfile
from pathlib import Path

import pytest

from research.locomo_retrieval import arms, cli, data, run, score
from research.locomo_retrieval.data import Question
from research.locomo_retrieval.tests import fixture

ATTICDB = os.environ.get("ATTICDB_BIN", "")
needs_atticdb = pytest.mark.skipif(not (ATTICDB and os.access(ATTICDB, os.X_OK)),
                                   reason="set ATTICDB_BIN to a built atticdb binary")

ARMS = ["atticdb-question", "atticdb-1word", "atticdb-2words", "fts5-bm25", "random", "atticdb-ranked"]
COLUMNS = ["sample_id", "q_index", "category", "arm", "query", "retrieved", "n_evidence",
           "recall@5", "recall@10", "recall@20", "hit@5", "hit@10", "hit@20",
           "tokens@5", "tokens@10", "tokens@20", "ms"]
# Kept questions: fx-a keeps qa 0, 1, 2, 5, 6, 7 and 8. fx-b keeps qa 0 and 3.
KEPT = {"fx-a": [0, 1, 2, 5, 6, 7, 8], "fx-b": [0, 3]}


@pytest.fixture
def short_tmp(tmp_path_factory, monkeypatch):
    """A short temp dir, as the atticdb socket path must fit in 104 bytes.

    Pytest's own basetemp is made first, so it never lands in this dir.
    """
    tmp_path_factory.getbasetemp()
    parent = tempfile.mkdtemp(prefix="zlr-t-", dir="/tmp")
    monkeypatch.setattr(tempfile, "tempdir", parent)
    yield parent
    shutil.rmtree(parent, ignore_errors=True)


@pytest.fixture
def dataset(tmp_path):
    return fixture.write(tmp_path / "locomo.json")


def _main(capsys, *args) -> tuple[int, str, str]:
    code = cli.main([str(a) for a in args])
    got = capsys.readouterr()
    return code, got.out, got.err


def _rows(out: Path) -> list[dict]:
    with open(out / "results.csv", newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))


def _row(rows, sample_id, q_index, arm) -> dict:
    [row] = [r for r in rows if (r["sample_id"], r["q_index"], r["arm"]) == (sample_id, str(q_index), arm)]
    return row


def _conv(sample_id: str) -> data.Conversation:
    return next(c for c in data.parse(fixture.samples()) if c.sample_id == sample_id)


def _turn_texts() -> list[str]:
    """Every turn text and note content in the fixture, and the image caption."""
    out = []
    for conv in data.parse(fixture.samples()):
        for t in conv.turns:
            out += [t.text, t.content]
    return out + ["a photo of a lopsided bowl"]


# ---- the end-to-end run ----

@needs_atticdb
def test_end_to_end_fixture(short_tmp, dataset, tmp_path, capsys):
    out = tmp_path / "out"
    code, _, err = _main(capsys, "--atticdb", ATTICDB, "--out", out, "--dataset", dataset)
    assert code == 0, err
    rows = _rows(out)
    assert list(rows[0]) == COLUMNS
    # 9 kept questions times 6 arms.
    assert len(rows) == 54
    for sid, indices in KEPT.items():
        for i in indices:
            assert sorted(r["arm"] for r in rows if (r["sample_id"], r["q_index"]) == (sid, str(i))) == sorted(ARMS)

    # fx-a qa[0]: "Where did Orla sign up for a pottery workshop?", evidence D1:1.
    # Search matches every word as a substring, so "workshop?" with its "?" finds nothing.
    q0 = _row(rows, "fx-a", 0, "atticdb-question")
    assert (q0["retrieved"], q0["recall@5"], q0["hit@20"]) == ("", "0.0", "0")
    assert q0["query"] == "Where did Orla sign up for a pottery workshop?"
    assert float(q0["ms"]) >= 0
    # Content words: orla 4 turns, sign 0, pottery 1, workshop 2. So pottery, then workshop.
    w1 = _row(rows, "fx-a", 0, "atticdb-1word")
    assert (w1["query"], w1["retrieved"], w1["recall@5"], w1["hit@5"]) == ("pottery", "D1:1", "1.0", "1")
    assert float(w1["tokens@5"]) == len("Orla: I signed up for a pottery workshop downtown.") / 4
    w2 = _row(rows, "fx-a", 0, "atticdb-2words")
    assert (w2["query"], w2["retrieved"], w2["recall@20"]) == ("pottery workshop", "D1:1", "1.0")
    fts = _row(rows, "fx-a", 0, "fts5-bm25")
    assert fts["query"] == '"orla" OR "sign" OR "pottery" OR "workshop"'
    assert fts["retrieved"].split()[0] == "D1:1" and fts["recall@5"] == "1.0"
    # Ranked, the whole question finds D1:1 first: it holds every content word, rare ones too.
    ranked = _row(rows, "fx-a", 0, "atticdb-ranked")
    assert ranked["query"] == q0["query"] and float(ranked["ms"]) >= 0
    assert ranked["retrieved"].split()[0] == "D1:1" and ranked["recall@5"] == "1.0"

    # fx-a qa[2]: evidence D2:2 and D10:2. Words: glaze 2, orla 4, teacher 1, survive 1.
    # 1 word: "survive" (tie with teacher, alphabetical) finds D10:2 only, so recall is 1/2.
    w1 = _row(rows, "fx-a", 2, "atticdb-1word")
    assert (w1["query"], w1["retrieved"], w1["n_evidence"]) == ("survive", "D10:2", "2")
    assert [w1[f"recall@{k}"] for k in (5, 10, 20)] == ["0.5"] * 3
    assert [w1[f"hit@{k}"] for k in (5, 10, 20)] == ["1"] * 3
    # 2 words: no turn holds both survive and teacher.
    w2 = _row(rows, "fx-a", 2, "atticdb-2words")
    assert (w2["query"], w2["retrieved"], w2["recall@20"], w2["hit@20"]) == ("survive teacher", "", "0.0", "0")
    # FTS5 matches at most 5 turns here, so its top 5 holds both.
    assert _row(rows, "fx-a", 2, "fts5-bm25")["recall@5"] == "1.0"

    # fx-a qa[1]: chess and club tie at 2 turns, so "chess". Newest first: D2:1, then D1:2.
    assert _row(rows, "fx-a", 1, "atticdb-1word")["retrieved"] == "D2:1 D1:2"

    # Random draws every turn of the 7-turn conversation, in the seeded order, and has no latency.
    conv = _conv("fx-a")
    corpus = arms.Corpus.build(conv)
    try:
        for q in conv.questions:
            r = _row(rows, "fx-a", q.index, "random")
            assert r["retrieved"].split() == arms.random_turns(corpus, q, 0)[0]
            assert (r["recall@20"], r["ms"], r["query"]) == ("1.0", "", "")
    finally:
        corpus.close()
    assert all(r["ms"] for r in rows if r["arm"] != "random" and r["query"])

    summary = (out / "summary.md").read_text(encoding="utf-8")
    for arm in ARMS:
        assert f"| {arm} |" in summary
    assert hashlib.sha256(dataset.read_bytes()).hexdigest() in summary
    assert "atticdb 0.1.0" in summary
    assert platform.platform() in summary
    assert f"CPUs: {os.cpu_count()}" in summary
    for key, n in {"category_5": 2, "empty_evidence": 2, "unscorable_evidence": 1,
                   "normalized_questions": 6, "dropped_ids": 3}.items():
        assert f"| {key} | {n} |" in summary
    full = (score.approx_tokens(t.content for t in _conv("fx-a").turns)
            + score.approx_tokens(t.content for t in _conv("fx-b").turns)) / 2
    assert f"Full context: ≈ {full:,.0f} tokens (bytes/4)" in summary
    assert "Questions scored: 9" in summary


@needs_atticdb
def test_conversations_limits_the_run(short_tmp, dataset, tmp_path, capsys):
    out = tmp_path / "out"
    code, _, err = _main(capsys, "--atticdb", ATTICDB, "--out", out, "--dataset", dataset,
                         "--conversations", 1)
    assert code == 0, err
    rows = _rows(out)
    assert {r["sample_id"] for r in rows} == {"fx-a"} and len(rows) == 42
    summary = (out / "summary.md").read_text(encoding="utf-8")
    # Counts cover the conversations that ran: fx-a has 1 category 5 and 1 empty evidence.
    assert "| category_5 | 1 |" in summary and "| empty_evidence | 1 |" in summary


# ---- isolation ----

@needs_atticdb
def test_stores_are_isolated(short_tmp, dataset, tmp_path, capsys, monkeypatch):
    a, b = _conv("fx-a"), _conv("fx-b")
    with run.Store(ATTICDB) as sa, run.Store(ATTICDB) as sb:
        assert sa.root != sb.root
        run.ingest(sa.client, a)
        run.ingest(sb.client, b)
        ca, cb = arms.Corpus.build(a), arms.Corpus.build(b)
        try:
            # "kiln" is only in fx-a, and "greyhound" only in fx-b.
            assert arms.atticdb_1word(sa.client, ca, Question(90, 4, "kiln", ("D10:1",)))[0] == ["D10:1"]
            assert sb.client.search("kiln", limit=100)["hits"] == []
            assert sa.client.search("greyhound", limit=100)["hits"] == []
            assert arms.atticdb_question(sb.client, cb, Question(91, 4, "greyhound", ("D1:1",)))[0] == ["D1:1"]
            # A whole question without punctuation finds its turn.
            assert arms.atticdb_question(sa.client, ca, Question(92, 4, "Orla pottery", ("D1:1",)))[0] == ["D1:1"]
            hits = sa.client.search("orla", limit=100)["hits"]
            assert {h["session"] for h in hits} == {"fx-a"} and len(hits) == 4
        finally:
            ca.close()
            cb.close()
    for s in (sa, sb):
        assert not os.path.exists(s.root)
        assert s.process.poll() is not None

    # A full run makes a fresh home per conversation, and deletes each one.
    roots = []

    class Recording(run.Store):
        def start(self):
            super().start()
            roots.append(self.root)

    monkeypatch.setattr(run, "Store", Recording)
    code, _, err = _main(capsys, "--atticdb", ATTICDB, "--out", tmp_path / "out", "--dataset", dataset)
    assert code == 0, err
    assert len(roots) == 2 and len(set(roots)) == 2
    assert not any(os.path.exists(r) for r in roots)


# ---- no dataset text in the outputs ----

@needs_atticdb
def test_results_hold_no_text(short_tmp, dataset, tmp_path, capsys):
    out = tmp_path / "out"
    code, stdout, stderr = _main(capsys, "--atticdb", ATTICDB, "--out", out, "--dataset", dataset)
    assert code == 0, stderr
    assert sorted(p.name for p in out.iterdir()) == ["results.csv", "summary.md"]
    csv_text = (out / "results.csv").read_text(encoding="utf-8")
    summary = (out / "summary.md").read_text(encoding="utf-8")
    for text in _turn_texts():
        for where, body in (("results.csv", csv_text), ("summary.md", summary), ("stdout", stdout),
                            ("stderr", stderr)):
            assert text not in body, f"turn text in {where}"
    # The summary and the console hold numbers and ids, never a question or a fixture word.
    questions = [q["question"] for s in fixture.samples() for q in s["qa"]]
    words = ["orla", "bram", "tamsin", "ezra", "pottery", "workshop", "chess", "kiln", "glaze",
             "greyhound", "pickle", "thursday"]
    for body in (summary, stdout, stderr):
        assert not any(q in body for q in questions)
        assert not any(w in body.lower() for w in words)


# ---- failures stop the run ----

@needs_atticdb
def test_search_error_stops_the_run(short_tmp, dataset, tmp_path, capsys, monkeypatch):
    stores = []

    class Dying(run.Store):
        """Kills its own server on the 3rd search."""

        def start(self):
            super().start()
            stores.append(self)
            search, calls = self.client.search, []

            def dying(*args, **kwargs):
                calls.append(1)
                if len(calls) == 3:
                    self.process.kill()
                    self.process.wait()
                return search(*args, **kwargs)

            self.client.search = dying

    monkeypatch.setattr(run, "Store", Dying)
    out = tmp_path / "out"
    code, _, err = _main(capsys, "--atticdb", ATTICDB, "--out", out, "--dataset", dataset)
    assert code == 1
    assert "error:" in err and "search failed" in err and "fx-a" in err
    assert "server exited" in err
    # Nothing is scored, and the store is gone.
    assert not (out / "results.csv").exists() and not (out / "summary.md").exists()
    assert len(stores) == 1 and not os.path.exists(stores[0].root)


class FakeClient:
    def __init__(self, reply=None, error=None):
        self.reply, self.error = reply, error
        self.calls = []

    def search(self, query, **kwargs):
        self.calls.append((query, kwargs))
        if self.error:
            raise self.error
        return self.reply


def _hit(dia_id, session="fx-a", **extra):
    return {"kind": "turn", "agent": "locomo", "session": session, "meta": {"dia_id": dia_id}, **extra}


@pytest.mark.parametrize("reply", [
    None,
    {"next": None},
    {"hits": [{"kind": "turn", "agent": "locomo", "session": "fx-a", "meta": {}}]},
    {"hits": [{"kind": "turn", "agent": "locomo", "session": "fx-a", "meta": None}]},
    {"hits": [_hit("D99:1")]},
    {"hits": [_hit("D1:1", session="fx-b")]},
    {"hits": [_hit("D1:1"), _hit("D1:1")]},
    {"hits": [{"kind": "fact", "key": "k", "meta": {"dia_id": "D1:1"}}]},
])
def test_bad_reply_raises(reply):
    corpus = arms.Corpus.build(_conv("fx-a"))
    try:
        with pytest.raises(arms.SearchError):
            arms.atticdb_question(FakeClient(reply), corpus, Question(0, 4, "pottery", ("D1:1",)))
    finally:
        corpus.close()


def test_client_error_raises_search_error():
    corpus = arms.Corpus.build(_conv("fx-a"))
    try:
        error = arms.atticdb_client.Error("GET /v1/search failed (connect failed)")
        with pytest.raises(arms.SearchError, match="connect failed"):
            arms.atticdb_1word(FakeClient(error=error), corpus, Question(0, 4, "pottery", ("D1:1",)))
    finally:
        corpus.close()


def test_good_reply_keeps_order():
    corpus = arms.Corpus.build(_conv("fx-a"))
    try:
        reply = {"hits": [_hit("D2:1"), _hit("D1:2")], "next": None}
        ids, query, ms = arms.atticdb_2words(FakeClient(reply), corpus, Question(1, 2, "chess club meet", ("D2:1",)))
        assert (ids, query) == (["D2:1", "D1:2"], "chess club")
        assert ms is not None and ms >= 0
    finally:
        corpus.close()


def test_only_the_ranked_arm_sends_order():
    corpus = arms.Corpus.build(_conv("fx-a"))
    try:
        reply = {"hits": [_hit("D1:1")], "next": None}
        q = Question(0, 4, "Where did Orla sign up for a pottery workshop?", ("D1:1",))
        client = FakeClient(reply)
        ids, query, ms = arms.atticdb_ranked(client, corpus, q)
        assert (ids, query) == (["D1:1"], q.question) and ms is not None
        # The whole question, as it is, ranked by the server.
        assert client.calls == [(q.question, {"limit": 100, "order": "relevance"})]
        # The other AtticDB arms send no order, so the server keeps them newest first.
        for arm, sent in ((arms.atticdb_question, q.question), (arms.atticdb_1word, "pottery"),
                          (arms.atticdb_2words, "pottery workshop")):
            client = FakeClient(reply)
            arm(client, corpus, q)
            assert client.calls == [(sent, {"limit": 100})]
    finally:
        corpus.close()


def test_client_sends_order_only_when_set(monkeypatch):
    client = arms.atticdb_client.Client("unix:///nonexistent/s", "t")
    targets = []
    monkeypatch.setattr(client, "_read", lambda target, ok: targets.append(target) or {})
    client.search("a b", limit=100)
    client.search("a b", limit=100, order="relevance")
    assert targets == ["/v1/search?q=a+b&limit=100", "/v1/search?q=a+b&limit=100&order=relevance"]


def test_no_content_words_means_no_search():
    corpus = arms.Corpus.build(_conv("fx-a"))
    try:
        boom = FakeClient(error=AssertionError("searched"))
        q = Question(0, 4, "What did they do?", ("D1:1",))
        assert arms.atticdb_1word(boom, corpus, q) == ([], "", None)
        assert arms.atticdb_2words(boom, corpus, q) == ([], "", None)
        assert arms.fts5_bm25(corpus, q) == ([], "", None)
        # Words the conversation never holds are skipped by the keyword arms only.
        q = Question(0, 4, "zeppelin", ("D1:1",))
        assert arms.atticdb_1word(boom, corpus, q) == ([], "", None)
        ids, query, ms = arms.fts5_bm25(corpus, q)
        assert (ids, query) == ([], '"zeppelin"') and ms is not None
    finally:
        corpus.close()


def test_fts5_orders_by_bm25_and_takes_20():
    turns = [data.Turn(f"D1:{i}", "Pat", "kayak " * (i % 3 + 1) + f"filler{i}") for i in range(1, 31)]
    turns.append(data.Turn("D2:1", "Pat", "kayak paddle"))
    corpus = arms.Corpus.build(data.Conversation("syn", turns, []))
    try:
        ids, query, ms = arms.fts5_bm25(corpus, Question(0, 4, "kayak paddle", ("D2:1",)))
        assert query == '"kayak" OR "paddle"'
        assert len(ids) == 20 and ids[0] == "D2:1" and len(set(ids)) == 20
    finally:
        corpus.close()


@needs_atticdb
def test_ingest_batches_and_keeps_order(short_tmp, monkeypatch):
    turns = [data.Turn(f"D1:{i}", "Pat", f"note {i} marker{i:04d}") for i in range(1, 2502)]
    conv = data.Conversation("big", turns, [])
    monkeypatch.setattr(run, "BATCH_TURNS", 1000)
    with run.Store(ATTICDB) as store:
        batches = run.ingest(store.client, conv)
        assert batches == 3
        hits = store.client.search("note", limit=100)["hits"]
        assert hits[0]["meta"] == {"dia_id": "D1:2501"} and hits[0]["content"] == "Pat: note 2501 marker2501"
        assert hits[0]["agent"] == "locomo" and hits[0]["session"] == "big"
        assert store.client.search("marker0001", limit=100)["hits"][0]["meta"]["dia_id"] == "D1:1"


def test_bad_checksum_stops(tmp_path, capsys, monkeypatch):
    cache = tmp_path / "cache"
    cache.mkdir()
    # A cached file with the wrong hash: fetch raises before any network or store.
    fixture.write(cache / "locomo10.json")

    class NoStore:
        def __init__(self, *a, **k):
            raise AssertionError("a store started")

    monkeypatch.setattr(run, "Store", NoStore)
    out = tmp_path / "out"
    code, _, err = _main(capsys, "--atticdb", "/nonexistent/atticdb", "--out", out, "--cache-dir", cache)
    assert code == 1
    assert "error:" in err and "SHA-256" in err and data.SHA256 in err
    assert not out.exists() or not any(out.iterdir())


def test_missing_binary_stops(dataset, tmp_path, capsys):
    out = tmp_path / "out"
    code, _, err = _main(capsys, "--atticdb", tmp_path / "nope", "--out", out, "--dataset", dataset)
    assert code == 1 and "error:" in err and "atticdb" in err
    assert not (out / "results.csv").exists()


# ---- random ----

def test_random_is_seeded():
    turns = [data.Turn(f"D1:{i}", "Pat", f"line {i}") for i in range(1, 51)]
    corpus = arms.Corpus.build(data.Conversation("syn", turns, []))
    try:
        q = Question(7, 4, "anything", ("D1:1",))
        first = arms.random_turns(corpus, q, 0)
        assert first == arms.random_turns(corpus, q, 0)
        ids, query, ms = first
        assert (query, ms) == ("", None)
        assert len(ids) == 20 and len(set(ids)) == 20 and set(ids) <= set(corpus.ids)
        # The seed for a question is seed * 1_000_003 + its index, over a shuffle of every turn.
        order = list(corpus.ids)
        random.Random(0 * 1_000_003 + 7).shuffle(order)
        assert ids == order[:20]
        order = list(corpus.ids)
        random.Random(2 * 1_000_003 + 7).shuffle(order)
        assert arms.random_turns(corpus, q, 2)[0] == order[:20]
        assert arms.random_turns(corpus, q, 1)[0] != ids
        assert arms.random_turns(corpus, Question(8, 4, "anything", ("D1:1",)), 0)[0] != ids
        # A conversation shorter than 20 turns gives every turn.
        small = arms.Corpus.build(data.Conversation("s", turns[:3], []))
        try:
            assert sorted(arms.random_turns(small, q, 0)[0]) == sorted(small.ids)
        finally:
            small.close()
    finally:
        corpus.close()


@needs_atticdb
def test_random_rows_repeat_across_runs(short_tmp, dataset, tmp_path, capsys):
    got = []
    for name in ("one", "two"):
        code, _, err = _main(capsys, "--atticdb", ATTICDB, "--out", tmp_path / name, "--dataset", dataset,
                             "--seed", 3)
        assert code == 0, err
        got.append([r["retrieved"] for r in _rows(tmp_path / name) if r["arm"] == "random"])
    assert got[0] == got[1]


# ---- the CLI ----

def test_cli_rejects_bad_counts(capsys):
    for args in (["--conversations", "0"], ["--seed", "x"]):
        with pytest.raises(SystemExit) as e:
            cli.main(["--atticdb", "a", "--out", "o", *args])
        assert e.value.code == 2
    capsys.readouterr()


def test_summary_is_numbers_only():
    rows = [
        {"sample_id": "s", "q_index": 0, "category": 4, "arm": arm, "query": "secret words",
         "retrieved": ["D1:1"], "n_evidence": 1, "ms": None if arm == "random" else 1.5,
         **{f"recall@{k}": 1.0 for k in (5, 10, 20)}, **{f"hit@{k}": True for k in (5, 10, 20)},
         **{f"tokens@{k}": 3.0 for k in (5, 10, 20)}}
        for arm in ARMS
    ]
    meta = {"sha256": "ab" * 32, "pinned": False, "atticdb_version": "atticdb 9", "platform": "plat",
            "cpus": 4, "date": "2026-11-07", "seed": 0, "conversations": 1, "full_context": 100.0,
            "excluded": dict.fromkeys(data.COUNT_KEYS, 0)}
    text = run.summary_markdown(rows, meta)
    assert "secret" not in text
    assert "| atticdb-1word | 5 | 1 | 100.0% | 100.0% | 3 |" in text
    assert "| random | 0 | n/a | n/a |" in text
    assert "| atticdb-1word | 1 | 1.50 | 1.50 |" in text
    assert "not the pinned file" in text
