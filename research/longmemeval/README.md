# LongMemEval-S QA comparison

The runner gives AtticDB, SQLite FTS5, and Mem0 OSS the same dated text chunks and question.
Each arm gives the same reader at most 2,048 context tokens. No-memory and labeled-session controls use the same reader.
The scorer reports answer correctness separately from labeled-session recall.
Finding a chunk from a labeled session does not mean the answer was in that chunk.

## Run locally

Use Python 3.12 and the packages pinned in [requirements.txt](requirements.txt).
Install the `qwen2.5:3b` and `nomic-embed-text:v1.5` models in your local Ollama server.
The runner checks both file hashes before loading the [cleaned dataset][dataset] and [tokenizer][tokenizer].

```sh
cargo build --release --locked -p atticdb
TMPDIR=/tmp python -m research.longmemeval.run \
  --atticdb target/release/atticdb \
  --dataset /path/to/longmemeval_s_cleaned.json \
  --tokenizer /path/to/tokenizer.json \
  --out /tmp/atticdb-longmemeval-pilot --per-slice 2
```

Use a fresh output directory. Repeat `--question-id` to rerun selected cases.
Results include model digests, settings, question IDs, timings, and scores.
`audit.jsonl` contains model responses. Keep it local.

## 6-question pilot

Seed 19 selected 2 questions each for abstention, knowledge updates, and temporal reasoning.
An initial run timed out after 4 questions. A second run completed the 2 missing questions.
All 30 paired rows were scored, with no invalid judge labels.

| Arm | Correct answers | Mean labeled-session recall |
|---|---:|---:|
| AtticDB | 1/6 | 55.6% |
| SQLite FTS5 | 4/6 | 88.9% |
| Mem0 OSS raw | 2/6 | 83.3% |
| No memory | 3/6 | 0.0% |
| Labeled-session prefix | 3/6 | 52.8% |

These are smoke-test results, not a leaderboard score.
Mem0 2.2.1 used raw ingest, local embeddings and Qdrant. Extraction, keyword search and reranking were off.
The labeled-session control fills the same budget in dataset order. It may omit the passage needed to answer.
Qwen2.5 3B answered and judged. The judge was not independent and did not use the official evaluator.
The first run did not record the AtticDB binary hash. Later runs record it.

## Where retrieval lost evidence

On the 4 answerable pilot questions, AtticDB's top 20 covered 10 of 11 labeled turns.
Only 2 of those turns made it into the 2,048-token context.
The fixed chunks could split a turn, so finding 1 chunk did not always recover the whole turn.

The [retrieval study](retrieval.py) compares fixed chunks, turn-aligned chunks, and a BM25 rerank.
It uses 2 disjoint samples of 60 questions and excludes the initial QA pilot.
Reranking uses the first 100 AtticDB hits. Every arm packs at most 20 hits into the same context budget.
Recall counts a labeled turn only when every chunk containing that turn fits.
The labels never enter the index or the reranker.

| Arm | Development recall | Confirmation recall |
|---|---:|---:|
| AtticDB, fixed chunks | 60.1% | 68.7% |
| AtticDB, turn-aligned chunks | 63.3% | 71.6% |
| AtticDB, turn-aligned chunks + rerank | 72.1% | 77.4% |
| SQLite FTS5, turn-aligned chunks | 75.4% | 79.2% |

These are packed evidence scores, not answer accuracy. The rerank runs only in the research harness.
To reproduce, use the dataset, tokenizer and binary from the QA run:

```sh
TMPDIR=/tmp python -m research.longmemeval.retrieval \
  --atticdb target/release/atticdb \
  --dataset /path/to/longmemeval_s_cleaned.json \
  --tokenizer /path/to/tokenizer.json
```

[dataset]: https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/98d7416c24c778c2fee6e6f3006e7a073259d48f/longmemeval_s_cleaned.json
[tokenizer]: https://huggingface.co/Qwen/Qwen2.5-3B-Instruct/resolve/aa8e72537993ba99e69dfaafa59ed015b17504d1/tokenizer.json
