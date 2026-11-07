"""Content words for the keyword arms, and the rarest-word pick.

A content word is a lowercase `[a-z0-9]+` token of 3 or more characters.
It must not be in STOPWORDS. The list is fixed before any run, and never tuned on answers.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping

TOKEN = re.compile(r"[a-z0-9]+")
MIN_LEN = 3

# Common English function words, question words and light verbs. Words under 3 letters are dropped anyway.
STOPWORDS: frozenset[str] = frozenset("""
about above after again against all also although among and another any anyone anything are around
because been before being below between both but can could did does doing done down during each either
else even ever every few for from further get gets got had has have having her here hers herself him
himself his how however into its itself just least less let like made make many may might more most
much must myself near neither nor not now off once one only onto other others our ours ourselves out
over own per quite rather really same say said she should since some something still such than that
the their theirs them themselves then there these they thing things this those though through thus
together too toward towards under until upon very was were what whatever when whenever where whether
which while who whom whose why will with within without would yet you your yours yourself yourselves
""".split())


def content_words(text: str) -> list[str]:
    """The content words of `text`, lowercased, unique, in first-seen order."""
    seen: dict[str, None] = {}
    for tok in TOKEN.findall(text.lower()):
        if len(tok) >= MIN_LEN and tok not in STOPWORDS:
            seen.setdefault(tok, None)
    return list(seen)


def doc_freq(texts: Iterable[str]) -> dict[str, int]:
    """How many of `texts` contain each content word.

    It uses the same tokens as content_words, so the 2 always agree.
    """
    df: dict[str, int] = {}
    for text in texts:
        for word in content_words(text):
            df[word] = df.get(word, 0) + 1
    return df


def rarest(words: Iterable[str], doc_freq: Mapping[str, int], n: int) -> list[str]:
    """The `n` words that the fewest texts contain, rarest first.

    Ties break alphabetically. Words with no count, or a count of 0, are skipped.
    """
    seen = {w for w in words if doc_freq.get(w, 0) > 0}
    return sorted(seen, key=lambda w: (doc_freq[w], w))[:max(n, 0)]
