# LoCoMo retrieval benchmark

Does AtticDB's search find the turns that answer a question?
This harness scores that on LoCoMo, against a lexical baseline and a chance control.
It is stdlib only, and no answering model is involved.

## What it measures

- LoCoMo has 10 long conversations, each with questions labelled by their evidence turns (`dia_id`s).
- Each conversation goes into its own fresh AtticDB store, 1 note per turn.
- Every scored question runs through 5 arms. Each arm returns a ranked list of `dia_id`s.
- Each list is scored against LoCoMo's own `evidence` labels, for k = 5, 10 and 20:
  - `recall@k`: the share of a question's evidence ids in the top k, averaged over questions.
  - `hit@k`: the share of questions with at least 1 evidence id in the top k.
  - `tokens@k`: the bytes of the top-k turn contents divided by 4, averaged. This is "≈ tokens (bytes/4)".
- Latency is the median and p95 wall time of the search call, in ms.
  - Ingestion and server start are excluded.
  - p50 and p95 cover only questions where the arm ran a search. An arm with no content words runs none.
  - Arms 1 to 3 go over the Unix socket. FTS5 runs in process, so it skips the IPC cost.
- Full context is the average bytes/4 of a whole conversation's turn contents.

## What it doesn't measure

- No QA accuracy. Nothing answers the questions, and nothing judges answers.
- No answering model, embeddings or reranking.
- The harness never reads a question's `answer`.
- Finding evidence is necessary for a good answer, but it is not enough.

## License

LoCoMo is CC BY-NC 4.0. None of it is in this repo, and none of it may be committed.
- The run downloads it at run time and checks its SHA-256. A mismatch stops the run.
- The tests use a hand-written fixture, `tests/fixture.py`. Its speakers and sentences are invented.
- `results.csv` stays in `--out` and is never committed. Its `query` column holds question words.
- `summary.md` holds numbers and ids only, never question or turn text.

## How to run it

```sh
cargo build --release --locked -p atticdb
python3 -m research.locomo_retrieval --atticdb target/release/atticdb --out /tmp/locomo-out
```

Options:
- `--cache-dir D`: where the checked dataset is cached. The default is `~/.cache/atticdb-locomo`.
- `--conversations N`: run only the first N conversations.
- `--seed S`: the random arm's seed. The default is 0.
- `--dataset PATH`: a local LoCoMo-shaped file instead of the download. Its hash is reported, not checked.

Each store's socket must fit in 104 bytes. If the temp dir is too long, set `TMPDIR=/tmp`.

Outputs, both in `--out` and written only when the whole run succeeds:
- `results.csv`: 1 row per question and arm. Columns are `sample_id`, `q_index`, `category`, `arm`, `query`, `retrieved` (the top 20 `dia_id`s), `n_evidence`, `recall@5/10/20`, `hit@5/10/20`, `tokens@5/10/20` and `ms`.
- `summary.md`: tables per arm and k, and per arm and category at k = 10.
  It also has latency, the counts and the run's provenance.

Tests:

```sh
cargo build --locked -p atticdb
ATTICDB_BIN=$PWD/target/debug/atticdb PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 \
  python3 -m pytest research/locomo_retrieval/tests -q
```

Tests that start a server skip when `ATTICDB_BIN` is unset.

## The stores

- Each conversation gets a fresh temp dir with its own `ATTICDB_HOME`.
- The harness runs `atticdb init`, then `atticdb serve --listen none --unix-socket <dir>/s`.
- The client is `clients/python/atticdb_client.py`, with the token from the store's config.
- Each turn becomes 1 note: content `"<speaker>: <text>"`, meta `{"dia_id": ...}`, agent `locomo`, session `<sample_id>`.
- Turns go in session order, then turn order, in batches of up to 1,000.
- Session dates, summaries, observations and image captions are never used.
- The server is stopped and the dir deleted after each conversation, on errors too.

A failed search, a dead server or an unscorable reply stops the run with exit code 1.
It is never scored as 0 recall.

## The arms

The arms, the stopword list and k were fixed in the plan before any run.

1. `atticdb-question`: the whole question as the search query, `limit` 100.
2. `atticdb-1word`: the question's rarest content word in this conversation.
3. `atticdb-2words`: the question's 2 rarest content words.
4. `fts5-bm25`: an in-memory SQLite FTS5 table over the same note contents.
   - The query is every content word, double-quoted and joined by ` OR `.
   - Results are ordered by `bm25()`, then rowid, and cut to 20.
5. `random`: a shuffle of every turn, seeded with `seed * 1_000_003 + question index`, cut to 20.
   - Top 5 is a prefix of top 20. It is the chance floor: recall@k is near k / turns.

Notes on arms 1 to 3:
- AtticDB search returns turns holding every query word, newest first.
- It matches each whitespace-separated word as a lowercase substring.
  - Every word must be present, filler words included. So a whole question almost never matches.
  - Arm 1 shows what happens when an agent passes the question as it is.
- A content word is a lowercase `[a-z0-9]+` token of 3 or more characters, not in `words.STOPWORDS`.
- "Rarest" means the fewest turns in the conversation hold it. Ties break alphabetically.
- Words the conversation never holds are skipped. With none left, the arm runs no search and returns nothing.
- The hits come in returned order. The first 20 are kept, and each k is scored from them.

## Questions and normalization

- LoCoMo has 1,986 questions. 446 are category 5, adversarial with no answer, and are excluded.
- That leaves 1,540 in categories 1 to 4: multi-hop, temporal, open-domain and single-hop.
- 4 open-domain questions have no evidence and are excluded. So 1,536 are scored.
- Evidence ids are normalized once at load, the same for every arm:
  - Each entry is split on `;`, `,` and whitespace.
  - `D<int>:<int>` loses leading zeros, so `D30:05` becomes `D30:5`.
  - Ids the conversation has no turn for are dropped. Duplicates are removed.
- On the real file this normalizes 10 questions and drops 4 ids. No question becomes unscorable.
- `summary.md` reports `category_5`, `empty_evidence`, `unscorable_evidence`, `normalized_questions` and `dropped_ids`.

## Reading the numbers next to Living Brain's

Living Brain's LoCoMo page reports 92.2% with GPT-5: https://docs.livingbrain.com/docs/benchmarks
- That is QA accuracy. A model reads retrieved memory and answers, and the answers are graded.
- These numbers are retrieval recall. No model answers anything here.
- So the 2 are different measures. Never put them side by side as scores.

What can't be compared:
- Latency. Their 592 ms covers search plus page reads in a hosted service. This run's p50 is a local substring scan.
- Context size. Their 9.4K tokens buy 92% QA accuracy. `tokens@k` here is for a far lower hit rate.
- Token units. Living Brain counts about 26K tokens for full context. This harness's bytes/4 gives about 19.2K for the same conversations.

High recall at small k is what any answering model needs first. Low recall caps what it can answer.
