"""Loading and filtering LoCoMo-shaped data, and the hash-checked fetch."""

import hashlib
import io
import json

import pytest

from research.locomo_retrieval import data
from research.locomo_retrieval.data import Conversation, Question, Turn
from research.locomo_retrieval.tests import fixture


@pytest.fixture
def path(tmp_path):
    return fixture.write(tmp_path / "locomo.json")


def test_load_returns_one_conversation_per_sample(path):
    convs = data.load(path)
    assert [c.sample_id for c in convs] == ["fx-a", "fx-b"]
    assert all(isinstance(c, Conversation) for c in convs)


def test_turns_in_numeric_session_order_then_turn_order(path):
    conv = data.load(path)[0]
    assert [t.dia_id for t in conv.turns] == ["D1:1", "D1:2", "D1:3", "D2:1", "D2:2", "D10:1", "D10:2"]
    assert conv.turns[0] == Turn("D1:1", "Orla", "I signed up for a pottery workshop downtown.")


def test_turn_extras_are_ignored(path):
    turn = data.load(path)[0].turns[2]
    assert turn == Turn("D1:3", "Orla", "Here is my first bowl.")
    assert "lopsided" not in turn.content


def test_turn_content_is_speaker_then_text():
    assert Turn("D1:1", "Orla", "Hi there.").content == "Orla: Hi there."


def test_only_categories_1_to_4_with_evidence_are_kept(path):
    a, b = data.load(path)
    assert [(q.index, q.category) for q in a.questions] == [(0, 4), (1, 2), (2, 1), (5, 1), (6, 2), (7, 4), (8, 4)]
    assert [(q.index, q.category) for q in b.questions] == [(0, 3), (3, 4)]


def test_evidence_parses_from_list_and_string(path):
    a, b = data.load(path)
    assert a.questions[0] == Question(0, 4, "Where did Orla sign up for a pottery workshop?", ("D1:1",))
    assert a.questions[1].evidence == ("D2:1",)
    assert b.questions[0].evidence == ("D1:1", "D1:2")


def test_evidence_is_normalized_at_load(path):
    a, b = data.load(path)
    ev = {q.index: q.evidence for q in a.questions}
    assert ev[2] == ("D2:2", "D10:2")  # Duplicate removed, order kept.
    assert ev[5] == ("D1:1", "D2:1")  # ";"-joined.
    assert ev[6] == ("D1:2", "D2:1")  # Space-joined, inside a string.
    assert ev[7] == ("D10:1",)  # Zero-padded.
    assert ev[8] == ("D2:1",)  # Unknown D9:9 dropped.
    assert 9 not in ev  # Every id unknown: unscorable.
    assert b.questions[1].evidence == ("D1:2", "D1:1")  # ","-joined.


def test_category_is_an_int(path):
    for conv in data.load(path):
        for q in conv.questions:
            assert type(q.category) is int


EXCLUDED = {"category_5": 2, "empty_evidence": 2, "unscorable_evidence": 1,
            "normalized_questions": 6, "dropped_ids": 3}


def test_excluded_counts_exclusions_and_normalization(path):
    # A category 5 question with empty evidence counts as category 5 only.
    # dropped_ids includes the 2 from the unscorable question.
    assert data.excluded(path) == EXCLUDED


@pytest.mark.parametrize("ids, want, dropped", [
    (("D1:1",), ("D1:1",), 0),
    (("D1:1; D2:1",), ("D1:1", "D2:1"), 0),
    (("D1:1,D2:1",), ("D1:1", "D2:1"), 0),
    (("D1:1 \tD2:1 ",), ("D1:1", "D2:1"), 0),
    (("D01:001", "D1:1"), ("D1:1",), 0),
    (("D2:1", "D1:1", "D2:1"), ("D2:1", "D1:1"), 0),
    (("D9:9", "D", "D:1:1", "d1:1", "D1:1"), ("D1:1",), 4),
    (("; ,",), (), 0),
])
def test_normalize_evidence(ids, want, dropped):
    assert data.normalize_evidence(ids, {"D1:1", "D2:1"}) == (want, dropped)


def test_question_with_only_separators_is_unscorable(tmp_path):
    raw = fixture.samples()
    raw[1]["qa"][0]["evidence"] = ["; ,"]
    path = fixture.write(tmp_path / "sep.json", raw)
    assert [q.index for q in data.load(path)[1].questions] == [3]
    assert data.excluded(path)["unscorable_evidence"] == 2


def test_bad_evidence_string_raises(tmp_path):
    raw = fixture.samples()
    raw[0]["qa"][1]["evidence"] = "['D2:1'"
    with pytest.raises(ValueError, match="fx-a.*evidence"):
        data.load(fixture.write(tmp_path / "bad.json", raw))


@pytest.mark.parametrize("evidence", ["'D2:1'", "[1, 2]", "{'D2:1'}", 7])
def test_evidence_that_is_not_a_list_of_strings_raises(tmp_path, evidence):
    raw = fixture.samples()
    raw[0]["qa"][1]["evidence"] = evidence
    with pytest.raises(ValueError, match="evidence"):
        data.load(fixture.write(tmp_path / "bad.json", raw))


@pytest.mark.parametrize("category", [0, 6, "x", None])
def test_unknown_category_raises(tmp_path, category):
    raw = fixture.samples()
    raw[0]["qa"][0]["category"] = category
    with pytest.raises(ValueError, match="category"):
        data.load(fixture.write(tmp_path / "bad.json", raw))


def test_duplicate_dia_id_raises(tmp_path):
    raw = fixture.samples()
    raw[1]["conversation"]["session_1"][1]["dia_id"] = "D1:1"
    with pytest.raises(ValueError, match="fx-b.*D1:1"):
        data.load(fixture.write(tmp_path / "bad.json", raw))


class _NoAnswer(dict):
    """A qa dict that fails the test if anything reads `answer`."""

    def __getitem__(self, key):
        assert key != "answer", "the harness must never read answer"
        return super().__getitem__(key)

    def get(self, key, default=None):
        assert key != "answer", "the harness must never read answer"
        return super().get(key, default)


def test_parse_never_reads_answer():
    raw = fixture.samples()
    for sample in raw:
        sample["qa"] = [_NoAnswer(q) for q in sample["qa"]]
    assert sum(len(c.questions) for c in data.parse(raw)) == 9
    assert data.count_excluded(raw) == EXCLUDED


# ---- fetch ----

def _payload(tmp_path):
    src = tmp_path / "src" / "locomo10.json"
    src.parent.mkdir()
    fixture.write(src)
    return src, hashlib.sha256(src.read_bytes()).hexdigest()


def test_fetch_downloads_and_checks_hash(tmp_path):
    src, digest = _payload(tmp_path)
    out = data.fetch(src.as_uri(), digest, tmp_path / "cache")
    assert out == tmp_path / "cache" / "locomo10.json"
    assert out.read_bytes() == src.read_bytes()


def test_fetch_wrong_hash_raises_and_keeps_nothing(tmp_path):
    src, _ = _payload(tmp_path)
    cache = tmp_path / "cache"
    with pytest.raises(ValueError, match="SHA-256"):
        data.fetch(src.as_uri(), "0" * 64, cache)
    assert not cache.exists() or list(cache.iterdir()) == []


def test_fetch_uses_a_good_cache_without_the_network(tmp_path):
    src, digest = _payload(tmp_path)
    cache = tmp_path / "cache"
    data.fetch(src.as_uri(), digest, cache)

    def no_network(url, timeout=None):
        raise AssertionError("fetch hit the network with a good cache")

    assert data.fetch(src.as_uri(), digest, cache, opener=no_network).read_bytes() == src.read_bytes()


def test_fetch_bad_cache_raises(tmp_path):
    src, digest = _payload(tmp_path)
    cache = tmp_path / "cache"
    cache.mkdir()
    (cache / "locomo10.json").write_text("tampered")
    with pytest.raises(ValueError, match="SHA-256"):
        data.fetch(src.as_uri(), digest, cache)


def test_fetch_with_a_stub_opener(tmp_path):
    body = json.dumps(fixture.samples()).encode()
    calls = []

    def opener(url, timeout=None):
        calls.append(url)
        return io.BytesIO(body)

    url = "https://example.invalid/data/locomo10.json"
    out = data.fetch(url, hashlib.sha256(body).hexdigest(), tmp_path / "c", opener=opener)
    assert calls == [url] and out.read_bytes() == body
    with pytest.raises(ValueError, match="SHA-256"):
        data.fetch(url, "f" * 64, tmp_path / "d", opener=opener)


def test_dataset_constants():
    assert data.URL == "https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json"
    assert data.SHA256 == "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4"
    assert data.CATEGORY_NAMES == {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop"}
