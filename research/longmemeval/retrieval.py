"""Compare chunk boundaries and reranking without generating or judging answers."""

import argparse
import json
import math
import random
import re
import statistics
from collections import Counter, defaultdict
from pathlib import Path

from research.locomo_retrieval.words import content_words
from research.longmemeval import run

SLICES = ("knowledge-update", "temporal-reasoning", "multi-session")
PER_SLICE = 20


def evidence_recall(ids, evidence):
    if not evidence:
        return None
    kept = set(ids)
    return sum(chunks <= kept for chunks in evidence) / len(evidence)


def rerank(ids, docs, query):
    if not ids:
        return []
    terms = content_words(query)
    counts = {key: Counter(re.findall(r"[^\W_]+", docs[key].lower())) for key in ids}
    sizes = {key: sum(tokens.values()) for key, tokens in counts.items()}
    mean_size = statistics.fmean(sizes.values()) or 1
    df = Counter(term for tokens in counts.values() for term in terms if term in tokens)
    weights = {term: math.log1p((len(ids) - freq + 0.5) / (freq + 0.5))
               for term, freq in df.items()}

    def bm25(key):
        norm = 1.2 * (0.25 + 0.75 * sizes[key] / mean_size)
        return sum(weight * tf * 2.2 / (tf + norm)
                   for term, weight in weights.items() if (tf := counts[key][term]))

    return sorted(ids, key=bm25, reverse=True)


def study_samples(records):
    # Freeze disjoint development and confirmation samples. Neither includes the QA pilot.
    seen = {record["question_id"] for record in run.select(records, 2)}
    for seed in (67, 68):
        rng = random.Random(seed)
        cohort = []
        for kind in SLICES:
            eligible = sorted((record for record in records
                               if run.category(record) == kind and record["question_id"] not in seen),
                              key=lambda record: record["question_id"])
            cohort.extend(rng.sample(eligible, PER_SLICE))
        seen.update(record["question_id"] for record in cohort)
        yield cohort


def study(records, binary, count):
    scores = defaultdict(list)
    for record in records:
        query = record["question"]
        for align in (False, True):
            docs, _, evidence = run.chunk_spans(record, align=align)
            if not evidence:
                raise ValueError(f"{record['question_id']} has no labeled turns")
            pool, _, _ = run.atticdb_rank(binary, docs, query, limit=100, max_bytes=1 << 20)
            fts, _, _ = run.fts_rank(docs, query)
            arms = {"AtticDB": pool, "FTS5": fts}
            if align:
                arms["AtticDB rerank"] = rerank(pool, docs, query)
            for name, ids in arms.items():
                ranked = ids[:run.TOP]
                kept, context = run.context_prefix(ranked, docs, count, run.BUDGET)
                key = ("aligned" if align else "fixed", name)
                scores[key].append((evidence_recall(ranked, evidence), evidence_recall(kept, evidence),
                                    count(context)))
    return scores


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("dataset", "tokenizer", "atticdb"):
        parser.add_argument(f"--{flag}", required=True, type=Path)
    args = parser.parse_args()
    from tokenizers import Tokenizer

    tokenizer = Tokenizer.from_file(str(run.checked(args.tokenizer, run.TOKENIZER_SHA)))
    records = json.loads(run.checked(args.dataset, run.DATA_SHA).read_bytes())
    binary = str(args.atticdb.resolve(strict=True))

    def count(text):
        return len(tokenizer.encode(text, add_special_tokens=False).ids)

    for label, cohort in zip(("development", "confirmation"), study_samples(records), strict=True):
        print(f"{label}: {len(cohort)} questions", flush=True)
        print("chunks | arm | rank recall | packed recall | mean context tokens", flush=True)
        for (strategy, arm), rows in study(cohort, binary, count).items():
            ranked, packed, tokens = (statistics.fmean(col) for col in zip(*rows, strict=True))
            print(f"{strategy} | {arm} | {ranked:.1%} | {packed:.1%} | {tokens:.0f}", flush=True)


if __name__ == "__main__":
    main()
