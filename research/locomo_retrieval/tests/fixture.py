"""A hand-written fixture in LoCoMo's JSON shape.

Every speaker and sentence here is invented. None of it comes from LoCoMo.
It covers the shapes the loader must handle:
- session keys that skip numbers and sort wrongly as strings
- extra turn fields: img_url, blip_caption and query
- category as an int or a string
- evidence as a list or a string, with duplicates
- category 5 without an answer, and questions with empty evidence
"""

from __future__ import annotations

import copy
import json
from pathlib import Path

CONV_A = {
    "sample_id": "fx-a",
    "conversation": {
        "speaker_a": "Orla",
        "speaker_b": "Bram",
        # session_10 comes first, so a string sort would put it before session_2.
        "session_10": [
            {"speaker": "Orla", "dia_id": "D10:1", "text": "The kiln finally reached cone six last night."},
            {"speaker": "Bram", "dia_id": "D10:2", "text": "Did the teal glaze survive the firing?"},
        ],
        "session_10_date_time": "9:00 am on 3 March, 2031",
        "session_1": [
            {"speaker": "Orla", "dia_id": "D1:1", "text": "I signed up for a pottery workshop downtown."},
            {"speaker": "Bram", "dia_id": "D1:2", "text": "Nice. I started a chess club at the library."},
            {"speaker": "Orla", "dia_id": "D1:3", "text": "Here is my first bowl.",
             "img_url": ["https://example.invalid/bowl.jpg"],
             "blip_caption": "a photo of a lopsided bowl",
             "query": "lopsided clay bowl"},
        ],
        "session_1_date_time": "4:10 pm on 2 January, 2031",
        # No session_3 to 9: numbers may be skipped.
        "session_2": [
            {"speaker": "Bram", "dia_id": "D2:1", "text": "The chess club meets every Thursday now."},
            {"speaker": "Orla", "dia_id": "D2:2", "text": "My workshop teacher loves teal glaze."},
        ],
        "session_2_date_time": "7:45 pm on 20 January, 2031",
    },
    # Fields the harness never uses.
    "event_summary": {"events_session_1": {"Orla": ["Joins a workshop."]}},
    "observation": {"session_1_observation": {"Orla": [["Likes clay.", "D1:1"]]}},
    "session_summary": {"session_1_summary": "Orla and Bram talk hobbies."},
    "qa": [
        {"question": "Where did Orla sign up for a pottery workshop?", "answer": "downtown",
         "evidence": ["D1:1"], "category": 4},
        {"question": "When does the chess club meet?", "answer": "Thursdays",
         "evidence": "['D2:1']", "category": "2"},
        {"question": "What glaze did Orla's teacher like and did it survive?", "answer": "teal, yes",
         "evidence": ["D2:2", "D10:2", "D2:2"], "category": 1},
        {"question": "What did Bram name his kiln?", "adversarial_answer": "Blaze",
         "evidence": ["D10:1"], "category": 5},
        {"question": "Would Orla enjoy a ceramics fair?", "answer": "Likely yes",
         "evidence": [], "category": 3},
    ],
}

CONV_B = {
    "sample_id": "fx-b",
    "conversation": {
        "speaker_a": "Tamsin",
        "speaker_b": "Ezra",
        "session_1": [
            {"speaker": "Tamsin", "dia_id": "D1:1", "text": "We adopted a greyhound named Pickle."},
            {"speaker": "Ezra", "dia_id": "D1:2", "text": "Pickle sounds like a fast friend."},
        ],
        "session_1_date_time": "11:00 am on 5 May, 2032",
    },
    "qa": [
        {"question": "What breed is Pickle?", "answer": "greyhound",
         "evidence": "['D1:1', 'D1:2']", "category": "3"},
        {"question": "Did Ezra adopt a cat?", "evidence": "[]", "category": "5"},
        {"question": "How old is Pickle?", "answer": "unknown", "evidence": "[]", "category": 1},
    ],
}

SAMPLES = [CONV_A, CONV_B]


def samples() -> list[dict]:
    """A deep copy, so a test can change it freely."""
    return copy.deepcopy(SAMPLES)


def write(path: Path, data: list[dict] | None = None) -> Path:
    """Write the fixture, or `data`, as JSON to `path`."""
    path.write_text(json.dumps(samples() if data is None else data), encoding="utf-8")
    return path
