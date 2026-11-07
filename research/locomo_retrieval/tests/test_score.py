"""recall@k, hit@k, the token estimate and the per-arm summary."""

import pytest

from research.locomo_retrieval import score
from research.locomo_retrieval.score import approx_tokens, hit, recall, summarize


def test_recall_dedupes_evidence():
    assert recall(["D1:3"], ("D1:3", "D1:3"), 5) == 1.0


def test_recall_with_one_of_two_found_is_half():
    assert recall(["D9:9", "D1:2", "D7:7"], ("D1:1", "D1:2"), 5) == 0.5


def test_recall_dedupes_retrieved():
    assert recall(["D1:1", "D1:1"], ("D1:1", "D1:2"), 5) == 0.5


def test_recall_of_nothing_found_is_zero():
    assert recall([], ("D1:1",), 5) == 0.0


def test_hit_is_true_when_any_evidence_id_is_found():
    assert hit(["D3:3", "D1:2"], ("D1:1", "D1:2"), 5) is True
    assert hit(["D3:3"], ("D1:1", "D1:2"), 5) is False


def test_k_cuts_the_list():
    retrieved = ["D0:1", "D0:2", "D0:3", "D1:1"]
    assert recall(retrieved, ("D1:1",), 3) == 0.0
    assert recall(retrieved, ("D1:1",), 4) == 1.0
    assert hit(retrieved, ("D1:1",), 3) is False
    assert hit(retrieved, ("D1:1",), 4) is True


@pytest.mark.parametrize("fn", [recall, hit])
def test_empty_evidence_or_bad_k_raises(fn):
    with pytest.raises(ValueError, match="evidence"):
        fn(["D1:1"], (), 5)
    with pytest.raises(ValueError, match="k"):
        fn(["D1:1"], ("D1:1",), 0)


def test_approx_tokens_is_utf8_bytes_over_4():
    assert approx_tokens(["abcd", "efgh"]) == 2.0
    assert approx_tokens(["é"]) == 0.5  # 2 bytes in UTF-8.
    assert approx_tokens([]) == 0.0


def _row(category, r5, h5, t5, latency):
    return {"category": category, "recall@5": r5, "hit@5": h5, "tokens@5": t5, "latency_ms": latency}


def test_summarize_means_and_categories():
    rows = [
        _row(1, 1.0, True, 10.0, 4.0),
        _row(1, 0.0, False, 20.0, 2.0),
        _row(4, 0.5, True, 30.0, 6.0),
    ]
    out = summarize(rows)
    assert out["n"] == 3
    assert out["recall@5"] == pytest.approx(0.5)
    assert out["hit@5"] == pytest.approx(2 / 3)
    assert out["tokens@5"] == pytest.approx(20.0)
    assert out["latency_ms_median"] == 4.0
    assert out["latency_ms_p95"] == 6.0
    assert list(out["by_category"]) == ["multi-hop", "single-hop"]
    mh = out["by_category"]["multi-hop"]
    assert mh["n"] == 2 and mh["recall@5"] == 0.5 and mh["hit@5"] == 0.5
    assert mh["latency_ms_median"] == 3.0
    assert out["by_category"]["single-hop"]["recall@5"] == 0.5


def test_summarize_orders_categories_by_number():
    rows = [_row(c, 0.0, False, 0.0, None) for c in (4, 3, 2, 1)]
    assert list(summarize(rows)["by_category"]) == ["multi-hop", "temporal", "open-domain", "single-hop"]


def test_summarize_omits_latency_when_not_measured():
    out = summarize([_row(2, 1.0, True, 5.0, None)])
    assert "latency_ms_median" not in out and "latency_ms_p95" not in out
    assert "latency_ms" not in out


def test_summarize_of_no_rows():
    assert summarize([]) == {"n": 0, "by_category": {}}


def test_summarize_rejects_mixed_rows():
    with pytest.raises(ValueError, match="metric"):
        summarize([_row(1, 1.0, True, 1.0, None), {"category": 1, "recall@5": 1.0}])
    with pytest.raises(ValueError, match="latency"):
        summarize([_row(1, 1.0, True, 1.0, None), _row(1, 1.0, True, 1.0, 3.0)])
    with pytest.raises(ValueError, match="category"):
        summarize([_row(5, 1.0, True, 1.0, None)])


def test_percentile_is_nearest_rank():
    values = list(range(1, 21))  # 1..20
    assert score.percentile(values, 95) == 19
    assert score.percentile(values, 50) == 10
    assert score.percentile([7.0], 95) == 7.0
    with pytest.raises(ValueError):
        score.percentile([], 95)
