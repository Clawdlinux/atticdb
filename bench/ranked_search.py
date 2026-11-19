"""Compare ranked search across 2 release binaries on the same synthetic stores."""

import argparse
import hashlib
import statistics
import time
from contextlib import ExitStack
from pathlib import Path

from research.locomo_retrieval.run import Store
from research.locomo_retrieval.score import percentile


CASES = (
    ("common", "build", 10),
    ("mixed scores", "build cache deploy", 10),
    ("selective", "sentinel", 10),
    ("large reply", "build cache deploy", 100),
)


def seed(client, n):
    for start in range(0, n, 1000):
        batch = []
        for i in range(start, min(start + 1000, n)):
            text = f"build result {i:06}"
            if i % 2 == 0:
                text += " cache"
            if i % 13 == 0:
                text += " deploy"
            if i % 10000 == 0:
                text += " sentinel"
            batch.append({"role": "assistant", "content": text})
        if len(client.append("bench", "rank", batch)["seqs"]) != len(batch):
            raise RuntimeError("incomplete append")
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        stats = client._read("/v1/stats", (200,))
        if stats["search_index_lsn"] == stats["durable_lsn"]:
            return
        time.sleep(0.05)
    raise RuntimeError("search index did not catch up")


def search(store, query, limit):
    start = time.perf_counter_ns()
    result = store.client.search(query, limit=limit, order="relevance")
    ms = (time.perf_counter_ns() - start) / 1e6
    for hit in result["hits"]:
        # Fresh stores do not share wall-clock times.
        del hit["commit_time_ns"]
    return result, ms


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--turns", type=int, default=100_000)
    parser.add_argument("--reps", type=int, default=50)
    args = parser.parse_args()
    if args.turns < 1 or args.reps < 1:
        parser.error("--turns and --reps must be positive")
    print(f"{args.turns} turns per store, {args.reps} timed paired calls per query", flush=True)
    with ExitStack() as stack:
        stores = []
        for label, path in (("baseline", args.baseline), ("candidate", args.candidate)):
            binary = path.resolve(strict=True)
            with binary.open("rb") as f:
                print(label, hashlib.file_digest(f, "sha256").hexdigest(), flush=True)
            store = stack.enter_context(Store(str(binary)))
            seed(store.client, args.turns)
            stores.append(store)
        print("query | baseline p50/p95 ms | candidate p50/p95 ms", flush=True)
        for label, query, limit in CASES:
            samples = [[], []]
            for rep in range(-5, args.reps):
                order = (rep % 2, 1 - rep % 2)
                pairs = [search(stores[i], query, limit) for i in order]
                if pairs[0][0] != pairs[1][0]:
                    raise RuntimeError(f"search result mismatch: {label}, call {rep}")
                if not pairs[0][0]["hits"]:
                    raise RuntimeError(f"empty result: {label}, call {rep}")
                if rep >= 0:
                    for i, (_, ms) in zip(order, pairs, strict=True):
                        samples[i].append(ms)
            print(label + " | " + " | ".join(
                f"{statistics.median(s):.3f}/{percentile(s, 95):.3f}" for s in samples
            ), flush=True)


if __name__ == "__main__":
    main()
