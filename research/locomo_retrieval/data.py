"""Load LoCoMo, keep the scorable questions, and fetch the file with a hash check.

The harness never reads a qa item's `answer`. Category 5 items often lack one.
Only categories 1 to 4 with non-empty evidence are kept. The rest are counted.

Evidence normalization is fixed, runs once at load, and is the same for every arm:
- Split each raw entry on ";", "," and whitespace, and strip each piece.
- Rewrite `D<int>:<int>` without leading zeros, so "D30:05" becomes "D30:5".
- Drop ids the conversation has no turn for, and count them in `dropped_ids`.
- Remove duplicates, keeping the first.
A question left with no valid id is excluded as `unscorable_evidence`.
"""

from __future__ import annotations

import ast
import hashlib
import json
import os
import re
import tempfile
import urllib.parse
import urllib.request
from collections.abc import Callable, Collection, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

URL = "https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json"
SHA256 = "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4"
DEFAULT_CACHE_DIR = Path("~/.cache/atticdb-locomo")

CATEGORY_NAMES = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop"}
ADVERSARIAL = 5

_SESSION = re.compile(r"session_(\d+)")
_SPLIT = re.compile(r"[;,\s]+")
_DIA_ID = re.compile(r"D(\d+):(\d+)")

COUNT_KEYS = ("category_5", "empty_evidence", "unscorable_evidence", "normalized_questions", "dropped_ids")


@dataclass(frozen=True)
class Turn:
    dia_id: str
    speaker: str
    text: str

    @property
    def content(self) -> str:
        """The note content: speaker, a colon, then the text."""
        return f"{self.speaker}: {self.text}"


@dataclass(frozen=True)
class Question:
    index: int  # Position in the sample's `qa` list, before any filter.
    category: int
    question: str
    evidence: tuple[str, ...]  # Normalized: valid, unique, in order.


@dataclass(frozen=True)
class Conversation:
    sample_id: str
    turns: list[Turn]  # Session order by number, then turn order.
    questions: list[Question]


# ---- parsing ----

def _category(sample_id: str, index: int, raw) -> int:
    """The category as an int. It may be an int or a string of an int."""
    try:
        if isinstance(raw, bool):
            raise TypeError
        cat = int(raw)
    except (TypeError, ValueError):
        cat = None
    if cat not in CATEGORY_NAMES and cat != ADVERSARIAL:
        raise ValueError(f"{sample_id} qa[{index}]: category must be 1 to 5, got {raw!r}")
    return cat


def _evidence(sample_id: str, index: int, raw) -> tuple[str, ...]:
    """The evidence ids. A string is parsed with ast.literal_eval, and any failure raises."""
    value = raw
    if isinstance(raw, str):
        try:
            value = ast.literal_eval(raw)
        except (ValueError, SyntaxError, TypeError, MemoryError, RecursionError) as e:
            raise ValueError(f"{sample_id} qa[{index}]: can't parse evidence {raw!r}: {e}") from e
    if not isinstance(value, (list, tuple)) or not all(isinstance(x, str) for x in value):
        raise ValueError(f"{sample_id} qa[{index}]: evidence must be a list of strings, got {raw!r}")
    return tuple(value)


def _turns(sample_id: str, conversation: Mapping) -> list[Turn]:
    """All turns in numeric session order. Dates and extra turn fields are ignored."""
    sessions = sorted((int(m.group(1)), key) for key in conversation
                      if (m := _SESSION.fullmatch(key)))
    turns: list[Turn] = []
    seen: set[str] = set()
    for _, key in sessions:
        for pos, raw in enumerate(conversation[key]):
            try:
                turn = Turn(str(raw["dia_id"]), str(raw["speaker"]), str(raw["text"]))
            except (KeyError, TypeError) as e:
                raise ValueError(f"{sample_id} {key}[{pos}]: turn needs dia_id, speaker and text") from e
            if turn.dia_id in seen:
                raise ValueError(f"{sample_id}: duplicate dia_id {turn.dia_id}")
            seen.add(turn.dia_id)
            turns.append(turn)
    return turns


def normalize_evidence(ids: Iterable[str], known: Collection[str]) -> tuple[tuple[str, ...], int]:
    """(the valid ids in first-seen order, the count of unknown ids dropped)."""
    out: dict[str, None] = {}
    dropped = 0
    for entry in ids:
        for piece in _SPLIT.split(entry.strip()):
            if not piece:
                continue
            if m := _DIA_ID.fullmatch(piece):
                piece = f"D{int(m.group(1))}:{int(m.group(2))}"
            if piece in known:
                out.setdefault(piece, None)
            else:
                dropped += 1
    return tuple(out), dropped


def _sample_id(sample: Mapping) -> str:
    return str(sample["sample_id"])


def _scan(samples: Iterable[Mapping]) -> tuple[list[Conversation], dict[str, int]]:
    """The conversations with their kept questions, and the counts.

    Category comes first, so a category 5 item's evidence is never parsed.
    `normalized_questions` counts kept questions whose evidence changed.
    `dropped_ids` counts unknown ids in all category 1 to 4 questions, unscorable ones included.
    """
    counts = dict.fromkeys(COUNT_KEYS, 0)
    convs = []
    for sample in samples:
        sid = _sample_id(sample)
        turns = _turns(sid, sample["conversation"])
        known = {t.dia_id for t in turns}
        kept = []
        for i, qa in enumerate(sample["qa"]):
            cat = _category(sid, i, qa["category"])
            if cat == ADVERSARIAL:
                counts["category_5"] += 1
                continue
            raw = _evidence(sid, i, qa["evidence"])
            if not raw:
                counts["empty_evidence"] += 1
                continue
            evidence, dropped = normalize_evidence(raw, known)
            counts["dropped_ids"] += dropped
            if not evidence:
                counts["unscorable_evidence"] += 1
                continue
            counts["normalized_questions"] += evidence != raw
            kept.append(Question(i, cat, str(qa["question"]), evidence))
        convs.append(Conversation(sid, turns, kept))
    return convs, counts


def parse(samples: Iterable[Mapping]) -> list[Conversation]:
    """Conversations from already-decoded LoCoMo JSON, with only the kept questions."""
    return _scan(samples)[0]


def count_excluded(samples: Iterable[Mapping]) -> dict[str, int]:
    """The exclusion and normalization counts over all samples. Keys are COUNT_KEYS."""
    return _scan(samples)[1]


def _read(path: str | os.PathLike) -> list:
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def load(path: str | os.PathLike) -> list[Conversation]:
    """Read a LoCoMo JSON file. Keeps categories 1 to 4 with non-empty evidence."""
    return parse(_read(path))


def excluded(path: str | os.PathLike) -> dict[str, int]:
    """What load() drops and repairs. Keys are COUNT_KEYS."""
    return count_excluded(_read(path))


# ---- fetch ----

def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def fetch(url: str, sha256: str, cache_dir: str | os.PathLike,
          opener: Callable = urllib.request.urlopen, timeout: float = 60) -> Path:
    """Download `url` into `cache_dir` once, and check its SHA-256.

    A cached file is checked and reused without the network.
    A wrong hash raises ValueError, from the cache or the download.
    A bad download is never written, so the cache holds only checked files.
    """
    name = PurePosixPath(urllib.parse.urlparse(url).path).name
    if not name:
        raise ValueError(f"no file name in URL {url!r}")
    cache = Path(cache_dir).expanduser()
    dest = cache / name
    want = sha256.lower()
    if dest.exists():
        got = _sha256(dest)
        if got != want:
            raise ValueError(f"cached {dest} has SHA-256 {got}, want {want}. Delete it to download again.")
        return dest
    with opener(url, timeout=timeout) as resp:
        body = resp.read()
    got = hashlib.sha256(body).hexdigest()
    if got != want:
        raise ValueError(f"download of {url} has SHA-256 {got}, want {want}")
    cache.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=cache, prefix=f".{name}.", suffix=".part")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(body)
        os.replace(tmp, dest)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return dest
