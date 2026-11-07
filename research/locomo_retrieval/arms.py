"""The 5 arms. Each one returns (dia_ids, query, ms) for 1 question.

`dia_ids` is the ranked list, best first. `query` is what the arm searched for.
`ms` is the search call's wall time, or None when no search ran.
The arms are fixed in the plan, and never tuned on answers.
"""

from __future__ import annotations

import importlib.util
import random
import sqlite3
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

from . import words
from .data import Conversation, Question

CLIENT_PATH = Path(__file__).resolve().parents[2] / "clients" / "python" / "atticdb_client.py"

AGENT = "locomo"
SEARCH_LIMIT = 100  # The server's cap. Arms 1 to 3 keep the hits in returned order.
FTS_TOP = 20
RANDOM_TOP = 20
SEED_STRIDE = 1_000_003

Result = tuple[list[str], str, float | None]


def _load_client():
    """The repo's Python client, imported from its file, never copied."""
    if (mod := sys.modules.get("atticdb_client")) is not None:
        return mod
    spec = importlib.util.spec_from_file_location("atticdb_client", CLIENT_PATH)
    if spec is None or spec.loader is None:
        raise ImportError(f"can't load the atticdb client from {CLIENT_PATH}")
    mod = importlib.util.module_from_spec(spec)
    sys.modules["atticdb_client"] = mod
    try:
        spec.loader.exec_module(mod)
    except BaseException:
        del sys.modules["atticdb_client"]
        raise
    return mod


atticdb_client = _load_client()


class SearchError(Exception):
    """A search failed or gave a reply that can't be scored. It stops the run."""


@dataclass
class Corpus:
    """1 conversation's turns, as every arm sees them.

    `db` is an in-memory SQLite FTS5 table over the same note contents as the store.
    """

    sample_id: str
    ids: tuple[str, ...]
    contents: dict[str, str]
    doc_freq: dict[str, int]
    db: sqlite3.Connection = field(repr=False)

    @classmethod
    def build(cls, conv: Conversation) -> Corpus:
        contents = {t.dia_id: t.content for t in conv.turns}
        db = sqlite3.connect(":memory:")
        try:
            db.execute("CREATE VIRTUAL TABLE t USING fts5(content, dia_id UNINDEXED)")
            db.executemany("INSERT INTO t (content, dia_id) VALUES (?, ?)",
                           [(t.content, t.dia_id) for t in conv.turns])
        except BaseException:
            db.close()
            raise
        return cls(conv.sample_id, tuple(contents), contents, words.doc_freq(contents.values()), db)

    def close(self) -> None:
        self.db.close()


def fts5_available() -> bool:
    try:
        with sqlite3.connect(":memory:") as db:
            db.execute("CREATE VIRTUAL TABLE t USING fts5(x)")
        return True
    except sqlite3.OperationalError:
        return False


# ---- the AtticDB arms ----

def _hit_ids(reply, corpus: Corpus) -> list[str]:
    """The dia_ids of a search reply, in order. Anything unscorable raises SearchError."""
    if not isinstance(reply, dict) or not isinstance(reply.get("hits"), list):
        raise SearchError("search reply has no hits list")
    ids: list[str] = []
    for n, hit in enumerate(reply["hits"]):
        if not isinstance(hit, dict) or hit.get("kind") != "turn":
            raise SearchError(f"hit {n} is not a turn")
        if (hit.get("agent"), hit.get("session")) != (AGENT, corpus.sample_id):
            raise SearchError(f"hit {n} is from another store or session")
        meta = hit.get("meta")
        dia_id = meta.get("dia_id") if isinstance(meta, dict) else None
        if dia_id not in corpus.contents:
            raise SearchError(f"hit {n} has no dia_id of this conversation")
        if dia_id in ids:
            raise SearchError(f"hit {n} repeats dia_id {dia_id}")
        ids.append(dia_id)
    return ids


def _search(client, corpus: Corpus, query: str) -> Result:
    start = time.perf_counter()
    try:
        reply = client.search(query, limit=SEARCH_LIMIT)
    except atticdb_client.Error as e:
        raise SearchError(str(e)) from e
    ms = (time.perf_counter() - start) * 1000
    return _hit_ids(reply, corpus), query, ms


def atticdb_question(client, corpus: Corpus, question: Question) -> Result:
    """Arm 1: the whole question, as an agent might pass it."""
    return _search(client, corpus, question.question)


def _rarest(client, corpus: Corpus, question: Question, n: int) -> Result:
    picked = words.rarest(words.content_words(question.question), corpus.doc_freq, n)
    if not picked:
        return [], "", None
    return _search(client, corpus, " ".join(picked))


def atticdb_1word(client, corpus: Corpus, question: Question) -> Result:
    """Arm 2: the question's rarest content word in this conversation."""
    return _rarest(client, corpus, question, 1)


def atticdb_2words(client, corpus: Corpus, question: Question) -> Result:
    """Arm 3: the question's 2 rarest content words in this conversation."""
    return _rarest(client, corpus, question, 2)


# ---- the controls ----

def fts5_bm25(corpus: Corpus, question: Question) -> Result:
    """Arm 4: every content word, quoted and joined by OR, ranked by bm25, best first.

    Rowid breaks bm25 ties, so the order is the same on every run.
    """
    terms = words.content_words(question.question)
    if not terms:
        return [], "", None
    # Content words are [a-z0-9]+, so quoting needs no escapes.
    query = " OR ".join(f'"{w}"' for w in terms)
    start = time.perf_counter()
    rows = corpus.db.execute(
        "SELECT dia_id FROM t WHERE t MATCH ? ORDER BY bm25(t), rowid LIMIT ?", (query, FTS_TOP)
    ).fetchall()
    ms = (time.perf_counter() - start) * 1000
    return [r[0] for r in rows], query, ms


def random_turns(corpus: Corpus, question: Question, seed: int) -> Result:
    """Arm 5: a seeded shuffle of every turn, cut to 20. Top 5 is a prefix of top 20."""
    order: list[str] = list(corpus.ids)
    random.Random(seed * SEED_STRIDE + question.index).shuffle(order)
    return order[:RANDOM_TOP], "", None
