# File refs and real-transcript load: implementation plan

> For agentic workers: use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** agents can save a reference to a local file and find the exact version again fast. A bench fills AtticDB with real transcripts and measures fill, reload, reads and compression.

**Architecture:** a file ref is a fact under `file/<absolute path>`. The facts B-tree gives O(log n) exact lookup, prefix listing by folder, and 16 kept revisions per path. So versions of the same path need no new index and no format change. Hashing, stat and staleness checks run in the client, on the user's machine. The bench is 1 stdlib Python script that talks HTTP through the existing Python client.

**Tech stack:** Rust (crate `atticdb`, existing deps only: blake3, serde_json), Python 3 stdlib.

**Spec:** owner decision `repo/atticdb/decisions/file-refs` (2026-10-31), this plan, and Track C of [0004](../design/0004-local-agents-and-objects.md).

## Global constraints

- No new crates. The 6 direct deps stay.
- No server, engine or tape format change.
- The client module owns the logic. MCP and shell verbs wrap it.
- The bench never writes transcript text outside its temp dir. Results hold numbers only.
- Writing style: no em dashes, short sentences, numbers as digits.

## Design

### File ref record

- Key: `file/` + the absolute path, `/` separators. Existing files are canonicalized, so symlinks resolve. On Windows: `file/C:/Users/...`.
- Content: the `about` text, trimmed and non-empty. This is what search matches.
- Meta: `{"file": {"bytes": u64, "modified_ns": u64, "blake3": "<64 hex>", "mime": "<type>"}}`.
- Version: every fact revision is a version. The server keeps the newest 16.

### Operations (client.rs)

`file_ref {path, about}`:
1. Resolve the path. It must exist and be a regular file.
2. Stat it and blake3-hash it, streamed.
3. Read the current ref. If blake3 and about both match, write nothing and return `unchanged: true`.
4. Otherwise `fact_set` with the record above. Return `{committed, lsn, key, blake3, unchanged: false}`.

`file_get {path, versions?}`:
- If `path` is a folder, list refs under `file/<folder>/`, up to 200, with a status per ref from stat only.
- Otherwise read the ref. Status:
  - `missing`: the file is gone.
  - `ok`: bytes and modified_ns match, or they differ but blake3 matches.
  - `changed`: blake3 differs.
- With `versions: true`, return each kept revision: lsn, commit time, about, bytes, blake3. Also return `on_disk`: the lsn of the revision whose blake3 matches the file now, or null.
- A path that no longer exists is looked up as given, made absolute. So a deleted file's history still reads.

MIME comes from the extension: png, jpg/jpeg, gif, webp, svg, pdf, mp4, mov, webm, mkv, mp3, wav, m4a, txt, md, json, csv, log, zip. Anything else is `application/octet-stream`.

### Surfaces

- MCP: tools `file_ref` (write, not destructive) and `file_get` (read-only). 8 tools total.
- Shell: `atticdb file save --path <p> --about <text>` and `atticdb file get --path <p> [--versions]`.
- INSTRUCTIONS gets 1 sentence on when to call `file_ref`.

## Review focus

1. Path with spaces or non-ASCII, such as `Application Support`. The key keeps the path as is.
2. A file saved, edited, then saved again. Both versions are listed, and `on_disk` names the second.
3. A file touched but not edited. Status stays `ok` through the hash fallback.
4. A file deleted after saving. `file_get` still returns the ref and its history, with status `missing`.
5. A key over 1024 bytes. The tool returns a clear error, and nothing is written.

---

### Task 1: file refs in the client, MCP and shell

**Files:**
- Modify: `crates/atticdb/src/client.rs` (ops `file_ref`, `file_get`, shell flags `--path`, `--about`, `--versions`)
- Modify: `crates/atticdb/src/mcp.rs` (schemas, annotations, INSTRUCTIONS, module doc)
- Modify: `crates/atticdb/src/main.rs` (verb `file`, HOME_COMMANDS, USAGE)
- Test: `crates/atticdb/tests/it/mcp.rs`

- [ ] Step 1: update `tool_calls_over_stdio` for 8 tools. Names end with `file_ref`, `file_get`. read_only is `[false, true, false, true, true, true, false, true]`.
- [ ] Step 2: add test `file_refs_keep_versions` in `tests/it/mcp.rs`. It drives one scenario over MCP:
  - Save `dir/shot 1.png` (a space in the name). Assert committed and key `file/<canonical path>`.
  - Save again with no change. Assert `unchanged: true`.
  - Touch the file. `file_get` status is `ok`.
  - Rewrite its bytes and save with a new about. `file_get versions:true` returns 2 versions, and `on_disk` is the newest lsn.
  - `memory_search` on a word from the about finds the fact.
  - `file_get` on the folder lists the ref.
  - Delete the file. Status is `missing`, and history still reads.
  - Error table, nothing written: missing path, a folder for `file_ref`, empty about, key over 1024 bytes.
- [ ] Step 3: run `cargo test -p atticdb --test it mcp::` and confirm the new test fails.
- [ ] Step 4: implement it. Add `Client::file_ref(&mut self, args: &Value) -> Value` and `Client::file_get(&mut self, args: &Value) -> Value`, wired in `call`. Reuse `fact_set`, `fact_get` and `read` for HTTP. History uses `GET /v1/facts/history?key=..&max_bytes=..`.
- [ ] Step 5: run `cargo test -p atticdb --test it mcp::`, `cargo clippy -p atticdb --all-targets -- -D warnings` and `cargo fmt --check`. All pass.
- [ ] Step 6: commit `feat: file refs with versions on the facts index`.

### Task 2: Python client search and the real-transcript bench

**Files:**
- Modify: `clients/python/atticdb_client.py` (add `search(query, limit=None, before=None, max_bytes=None)`, GET `/v1/search`)
- Test: `clients/python/tests/test_client.py` (one parametrized case through the existing fixtures)
- Create: `bench/realload.py`

The bench takes `--source <glob>` (repeatable), `--target-mib` (default 256), `--writers` (default 8), `--batch` (default 16), `--refs` (default 20000), `--atticdb` and `--out <json>`. It runs 7 phases:
1. Corpus: read the JSONL transcripts. Claude Code lines with `message` map to user, assistant and tool turns. Other lines fall back to string `content` and `text` fields. Repeat sessions under new names until the target size.
2. Fill: `atticdb init` and `serve` on a 0700 temp dir over a Unix socket. N writers append batches. Report turns/s, MiB/s, and batch p50 and p99.
3. Disk: allocated bytes vs logical content bytes.
4. Reads: window (last 20) on 200 random sessions, and search on 100 words drawn from the corpus. Report p50 and p99.
5. File refs: put `--refs` facts under `file/<real paths from a walk of the home>`. Time fact_get on 1,000 random keys and fact_list on 200 folder prefixes.
6. Reload: SIGKILL, then restart. Time to the first answered read. Then a clean stop and restart, timed the same way.
7. Compression estimate, stdlib only: zlib level 6 per turn, and zlib 6 and lzma per 64 KiB block over the first 64 MiB. Report % saved and the 0004 Track D gate (30%).

- [ ] Step 1: add the search test, run `python -m pytest clients/python/tests -q`, see it fail, implement, see it pass.
- [ ] Step 2: write `bench/realload.py`. Run a smoke with `--target-mib 8 --refs 500`. It prints every metric and deletes its temp dir.
- [ ] Step 3: commit `bench: real-transcript fill, reload and compression estimate`.

### Task 3: run it and write it down

- [ ] Step 1: build release. Run `bench/realload.py --target-mib 1024 --out /tmp/realload.json` on this Mac.
- [ ] Step 2: add a "Real transcripts" section to `bench/RESULTS.md` with the table, the machine, the corpus size and the gate verdict.
- [ ] Step 3: update 0004 Track C: C-objects is replaced by file refs (owner 2026-10-31). Add D94 to 0002. Update README tools and CLI.
- [ ] Step 4: commit `docs: file refs replace the object store; real-load results`.
