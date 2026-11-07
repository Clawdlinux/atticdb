"""Content words, the stopword list and the rarest-word pick."""

from research.locomo_retrieval import words
from research.locomo_retrieval.words import STOPWORDS, content_words, doc_freq, rarest


def test_stopwords_is_a_fixed_frozenset_of_about_150_words():
    assert isinstance(STOPWORDS, frozenset)
    assert 130 <= len(STOPWORDS) <= 170
    assert all(w == w.lower() and w.isalpha() for w in STOPWORDS)
    assert {"when", "did", "the", "what", "where", "who", "how", "which"} <= STOPWORDS


def test_content_words_drop_stopwords_and_short_tokens():
    # The brief's example shape, with invented words. "go" and "to" are too short.
    assert content_words("When did Marisol go to the harbor rowing club?") == [
        "marisol", "harbor", "rowing", "club"]


def test_content_words_are_unique_and_in_order():
    assert content_words("Pickle, pickle and PICKLE with Bram's 3 cats in 2031") == [
        "pickle", "bram", "cats", "2031"]


def test_content_words_split_on_non_alphanumerics():
    assert content_words("teal-glaze/kiln_cone6 café") == ["teal", "glaze", "kiln", "cone6", "caf"]


def test_content_words_of_only_stopwords_is_empty():
    assert content_words("What did you do?") == []
    assert content_words("") == []


def test_doc_freq_counts_texts_not_occurrences():
    df = doc_freq(["Orla: kiln kiln glaze", "Bram: glaze", "Orla: chess"])
    assert df == {"orla": 2, "kiln": 1, "glaze": 2, "bram": 1, "chess": 1}


def test_rarest_orders_by_doc_freq():
    df = {"kiln": 3, "glaze": 1, "teal": 2}
    assert rarest(["kiln", "glaze", "teal"], df, 2) == ["glaze", "teal"]


def test_rarest_breaks_ties_alphabetically():
    df = {"zebra": 1, "apple": 1, "mango": 1, "kiln": 4}
    assert rarest(["zebra", "kiln", "mango", "apple"], df, 3) == ["apple", "mango", "zebra"]


def test_rarest_skips_words_the_conversation_never_has():
    df = {"kiln": 2, "glaze": 0}
    assert rarest(["unseen", "glaze", "kiln"], df, 2) == ["kiln"]


def test_rarest_of_nothing_is_empty():
    assert rarest([], {"kiln": 1}, 2) == []
    assert rarest(["kiln"], {"kiln": 1}, 0) == []


def test_module_exports():
    assert words.TOKEN.pattern == "[a-z0-9]+"
    assert words.MIN_LEN == 3
