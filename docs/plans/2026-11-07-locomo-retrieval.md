# LoCoMo retrieval benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure how well AtticDB's search finds the evidence turns for LoCoMo's 1,540 questions, against a lexical baseline and a chance control. No answering model is involved.

**Architecture:** A stdlib-only harness, `research/locomo_retrieval`.
- It downloads LoCoMo at run time, pinned by SHA-256.
- It loads each conversation into its own fresh AtticDB store, 1 turn per note, with the `dia_id` in meta.
- It runs every question through 5 arms and scores each arm's top-k against LoCoMo's own `evidence` labels.
- It writes a CSV and a markdown summary.

**Tech Stack:**
- Python 3 stdlib, including `sqlite3` with FTS5.
- The AtticDB Python client `clients/python/atticdb_client.py`, over the Unix socket.
- An `atticdb` binary given by `--atticdb`.
- Tests use pytest and a 3-turn fixture. They never download the dataset.

**Spec:**
- The owner asked for "more benchmarking that is new and valid" on 2026-11-07.
- Comparison point: Living Brain's LoCoMo page, https://docs.livingbrain.com/docs/benchmarks. It reports 1,540 questions, about 9.4K memory tokens per question and a 592 ms median retrieval. Those are QA numbers, not retrieval.
- Dataset: https://github.com/snap-research/locomo, `data/locomo10.json`.

## Global Constraints

- **Dataset:** `https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json`, with SHA-256 `79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4`.
  - A mismatch stops the run. Cache it under `--cache-dir`, default `~/.cache/atticdb-locomo`.
  - It is CC BY-NC 4.0. Never commit it or any part of it, including tests, fixtures and results. Results hold only ids, numbers and the arm's query text. Tests use a hand-written fixture.
- **Questions:** 1,540 questions in categories 1 to 4. 4 have no evidence and are excluded and counted, so 1,536 are scored.
  - Category 5, adversarial with no answer, is excluded and its count is reported.
  - Questions with an empty `evidence` list are excluded and counted.
  - LoCoMo's category names, as Living Brain also uses them: 1 multi-hop (282), 2 temporal (321), 3 open-domain (96, 92 scored), 4 single-hop (841).
  - `evidence` is a string like `"['D1:3']"` or a list. Parse both with `ast.literal_eval`. Any parse failure stops the run.
  - Evidence is then normalized once at load, the same for every arm, without reading answers:
    - Split each entry on `;`, `,` and whitespace, and strip each piece.
    - Rewrite `D<int>:<int>` without leading zeros, so `D30:05` becomes `D30:5`.
    - Drop ids the conversation has no turn for. Remove duplicates, keeping order.
    - A question with no valid id left is excluded and counted as `unscorable_evidence`.
    - Report `category_5`, `empty_evidence`, `unscorable_evidence`, `normalized_questions` and `dropped_ids`.
    - On the real file: 10 questions normalized, 4 ids dropped, 0 unscorable.
- **Corpus:**
  - Each turn becomes 1 AtticDB note with content `"<speaker>: <text>"`, agent `locomo`, session `<sample_id>`, and meta `{"dia_id": <dia_id>}`.
  - Turns go in in session order, then turn order. Session dates are not added to the content.
  - The fields `event_summary`, `observation` and `session_summary` are never used.
  - Image captions (`blip_caption`) are not used.
- **Store per conversation:** a fresh temp `ATTICDB_HOME` per conversation, so search can't cross conversations. Stop the server and delete the home after each conversation.
- **Arms, fixed before any run. Each returns an ordered list of `dia_id`s:**
  1. `atticdb-question`: `memory_search` with the whole question as the query, `limit` 100. AtticDB returns newest first.
  2. `atticdb-1word`: `memory_search` with the rarest content word of the question.
  3. `atticdb-2words`: `memory_search` with the 2 rarest content words.
     - For arms 2 and 3, a "content word" is a lowercase `[a-z0-9]+` token, at least 3 characters, not in the fixed stopword list in `words.py`.
     - "Rarest" means the fewest turns in this conversation contain the word, with ties broken alphabetically.
     - Words the conversation never contains are skipped.
     - With no content words left, the arm returns an empty list.
  4. `fts5-bm25`: an in-memory SQLite FTS5 table over the same turn contents. The query is the question's content words joined with OR, each quoted. Results are ordered by `bm25()`, best first.
  5. `random`: `k` turns sampled without replacement from the conversation with `random.Random(<seed>)`. The seed is `--seed`, default 0, and it changes per question as `seed * 1_000_003 + question index`.
- **Metrics per arm, for k in {5, 10, 20} and per category:**
  - `recall@k`: the share of a question's evidence `dia_id`s found in the top k, averaged over questions.
  - `hit@k`: the share of questions with at least 1 evidence id in the top k.
  - `tokens@k`: bytes of the top-k turn contents divided by 4, averaged. It's labelled "≈ tokens (bytes/4)".
  - `latency`: median and p95 wall time per question, in ms, for arms 1 to 4. It covers the search calls only. Ingestion and process start are excluded.
  - Also report `full context`: the average bytes/4 of the whole conversation, which is the size reference Living Brain uses.
- **No tuning on answers:** the stopword list, the arms and k are fixed in this plan. The harness never reads `answer`.
- **Writing:** no em dashes, sentences under 20 words. Commits use `git commit -s`, no Co-authored-by.
- **Verify:**
  - `PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest research/locomo_retrieval/tests -q`
  - The existing research suites still pass: `research/handoff_eval/tests` and `research/setup_eval/tests` with `ATTICDB_BIN` set.

## Review Focus

1. **The dataset never lands in the repo.** No fixture, results file or log may hold a LoCoMo sentence. Test: `test_results_hold_no_text` (Task 2). It asserts that the CSV and markdown hold no turn text from a fixture conversation.
2. **Scoring is correct when evidence has several ids, and when an id appears twice.** Test: `test_recall_counts_unique_evidence` (Task 1).
3. **Stores are isolated per conversation.** A word in conversation A never returns a hit from conversation B. Test: `test_stores_are_isolated` (Task 2).
4. **A server failure mid-run fails loudly.** It is never scored as 0 recall. Test: `test_search_error_stops_the_run` (Task 2).
5. **A checksum mismatch stops before any work.** Test: `test_bad_checksum_stops` (Task 2).

---

### Task 1: data, words and scoring (pure functions)

**Files:**
- Create: `research/locomo_retrieval/__init__.py`, `data.py`, `words.py`, `score.py`
- Create: `tests/__init__.py`, `tests/fixture.py` (a hand-written 2-conversation fixture in LoCoMo's shape), `tests/test_data.py`, `tests/test_words.py`, `tests/test_score.py`

**Interfaces:**
- `data.load(path) -> list[Conversation]`. `Conversation` has `sample_id`, `turns: list[Turn(dia_id, speaker, text)]` in order, and `questions: list[Question(index, category, question, evidence: tuple[str, ...])]`. Only categories 1 to 4 and non-empty evidence are kept. `data.excluded(path) -> dict` counts category 5 and the empty-evidence questions.
- `data.fetch(url, sha256, cache_dir) -> Path`. It downloads with `urllib`, checks the hash, and raises `ValueError` on a mismatch.
- `words.STOPWORDS: frozenset[str]`, about 150 common English function words, written into the file.
- `words.content_words(text) -> list[str]`, unique and in order.
- `words.rarest(words, doc_freq, n) -> list[str]`.
- `score.recall(retrieved, evidence, k) -> float`, `score.hit(...) -> bool`, and `score.summarize(rows) -> dict`.

- [ ] Write the failing tests:
  - Load the fixture. Check the category filter and the evidence parse for both string and list forms.
  - `content_words("When did Marisol go to the harbor rowing club?")` returns `["marisol", "harbor", "rowing", "club"]`, given the stopword list.
  - `rarest` breaks ties alphabetically.
  - Recall with duplicate evidence ids: `recall(["D1:3"], ("D1:3", "D1:3"), 5) == 1.0`.
  - Recall with 2 ids, 1 of them found, is `0.5`.
  - `hit` is true when any id is found.
  - k cuts the list.
  - `fetch` with a wrong hash raises, using a local file through a `file://` URL or a patched opener.
- [ ] Run them, and see them fail.
- [ ] Implement.
- [ ] Run the tests again.
- [ ] Commit: `research(locomo): data, words and scoring`.

### Task 2: the runner and the 5 arms

**Files:**
- Create: `research/locomo_retrieval/arms.py`, `run.py`, `cli.py`, `__main__.py`, `README.md`
- Create: `tests/test_run.py`
- The run's outputs go only to `--out`. Never commit dataset-derived text.

**Interfaces:**
- CLI: `python3 -m research.locomo_retrieval --atticdb <bin> --out <dir> [--cache-dir D] [--conversations N] [--seed 0] [--dataset <path>]`.
  - `--dataset` skips the download, for tests.
  - Outputs:
    - `<out>/results.csv`, 1 row per question × arm, with: `sample_id`, `q_index`, `category`, `arm`, `query`, `retrieved` (the top 20 `dia_id`s joined by space), `n_evidence`, `recall@5/10/20`, `hit@5/10/20`, `tokens@5/10/20`, `ms`.
    - `<out>/summary.md`: tables per arm × k, and per arm × category at k = 10, plus the full-context size, the excluded counts, the dataset hash, the atticdb version and the machine (`platform.platform()`, CPU count).
- `arms.py` holds 1 function per arm. Each returns `(dia_ids: list[str], query: str, ms: float | None)`.
- The AtticDB arms use the Python client over the store's Unix socket, with the token from the store's config. Start the store the way `research/setup_eval` or the Python client tests do. Reuse their helpers, and don't copy them.

- [ ] Write the failing tests, using the fixture and a real `atticdb` binary from `ATTICDB_BIN`. Skip them when it's unset, as other research suites do.
  - `test_end_to_end_fixture`: 2 conversations, every arm, a CSV with the expected row count, and recall exactly as hand-computed for 2 fixture questions.
  - `test_stores_are_isolated`.
  - `test_results_hold_no_text`.
  - `test_search_error_stops_the_run`: stop the server mid-run. The run exits non-zero with a clear message.
  - `test_bad_checksum_stops`.
  - `test_random_is_seeded`: the same seed gives the same result.
- [ ] Run them, and see them fail.
- [ ] Implement.
- [ ] Run the tests and the verify list.
- [ ] Write `README.md`:
  - what it measures, and what it doesn't (QA accuracy, answering models)
  - the license note
  - how to run it
  - the arms
  - how to read the numbers next to Living Brain's
- [ ] Commit: `research(locomo): runner, 5 arms, summary`.

### Task 3: the run and results (controller, not a subagent)

- [ ] Build `atticdb` from main in release mode.
- [ ] Run all 10 conversations with seed 0.
- [ ] Check the summary by hand: 1,536 questions are scored (1,540 minus 4 with empty evidence, and 0 unscorable), every arm has rows for every question, and random recall@k is close to k / turns.
- [ ] Commit `research/locomo_retrieval/results/2026-11-07.md` with numbers only, plus a short "what this says" section that cites the CSV columns. Don't commit `results.csv`, because its `query` column holds question words.
- [ ] Open the PR.

## What this plan does not do

- No answering model, no judge, no QA accuracy.
- No embeddings. That's #117.
- No change to AtticDB's search. Findings go into issues.
- No other datasets yet, such as LongMemEval.
