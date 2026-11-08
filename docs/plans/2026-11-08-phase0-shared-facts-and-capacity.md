# Phase 0: shared facts and tenants per VM. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agents sharing 1 store never lose a fact write silently and can see a fact's history. We also get a measured tenants-per-VM number to price the cloud.

**Architecture:**
- `atticdb mcp` remembers the LSN it last saw per fact key and sends it as `if_lsn` on `fact_set`. The engine already rejects a stale write with HTTP 409 `conflict` (D76).
- A 9th MCP tool, `fact_history`, wraps the existing `GET /v1/facts/history`.
- A Python bench, `bench/tenants.py`, measures 2 hosting layouts (1 process per tenant, or 1 shared process) at growing tenant counts.

**Tech Stack:** Rust (crates/atticdb), Python 3 stdlib (bench). No new dependencies.

**Spec:** `docs/design/0009-default-memory-layer.md`, Phase 0 and "Calls made after the builder check".

## Global Constraints

- The model never sees `if_lsn` (D76). It is never a tool parameter or a field in a tool result.
- The conditional write is on only in `atticdb mcp`. The shell verbs `atticdb fact set` and `atticdb file save` stay last-write-wins.
- A key this process never saw is sent with `if_lsn: 0`, meaning "must not exist yet".
- `INSTRUCTIONS` stays within 270 words (`instructions_stay_within_word_budget`).
- 0c pass bar: p99 at most 50 ms for `fact_set` and `memory_append`. Headline at 5% of tenants active, with 1% and 20% also reported.
- 0c tenant stores use `segment_mib: 16`.
- 0c process per tenant runs always on. Cold start is measured separately.
- Azure: zdb-g1 (Standard_L8s_v3, Central India, $0.792/hr Linux pay-as-you-go). Use it only after the owner approves a spend cap. Deallocate after the run, and report hours and cost.
- Prose: no em dashes, sentences under 20 words, numbers as digits. Commits: `git commit -s`, no Co-authored-by.

## Review Focus

1. **Two `mcp` processes edit 1 key.** Process A gets the key, process B sets it, then A sets it. A's set must conflict and write nothing. A's retry must then succeed. Test in Task 1.
2. **A first `fact_set` on a key that exists, written by another process.** It conflicts, the error shows the current value, and the immediate retry succeeds. Test in Task 1.
3. **Delete, then set again in the same process.** After its own delete, the process remembers 0, so a set of a new value succeeds. Test in Task 1.
4. **The conflict wording must not read as a failure to give up on.** It says nothing was written and to merge and set again. Asserted in Task 1.
5. **`fact_history` on a key that doesn't exist, or holds only a tombstone.** It returns no revisions, or the deleted marker, not an error. Test in Task 2.

---

### Task 1: Conditional `fact_set` in `atticdb mcp`

**Files:**
- Modify: `crates/atticdb/src/client.rs`: the `Client` struct (line 38), `open` (around 384), `fact_set` (456), `fact_get` (482), `fact_list` (492).
- Modify: `crates/atticdb/src/mcp.rs`: the line 110 constructor, and the `fact_set` description (around 301).
- Modify: `docs/design/0002-decision-log.md` (new row D107).
- Test: `crates/atticdb/tests/it/mcp.rs`.

**Interfaces:**
- Produces: `Client::guard_facts(&mut self)`. It turns on the LSN memory, `seen: Option<HashMap<String, u64>>`, which is `None` by default. `mcp.rs` calls it right after `Client::new`.
- Produces: a private method on `Client` that records the LSN of a key. Task 2 doesn't use it.

- [ ] **Step 1: Write the failing tests** in `crates/atticdb/tests/it/mcp.rs`. Use the setup in `tool_calls_over_stdio`: `support::serve`, then `Mcp::start` twice with the same `--url` and token, agents `a` and `b`.

```rust
#[test]
fn fact_set_never_overwrites_a_value_it_has_not_seen() {
    // a: fact_set repo/k "one" -> committed (a never saw it, if_lsn 0, key absent)
    // b: fact_set repo/k "two" -> isError; text starts with "conflict:" and contains "one"
    //    and contains "Nothing was written" and "call fact_set again"
    // b: fact_set repo/k "two" again -> committed (b now remembers a's LSN)
    // a: fact_set repo/k "three" -> isError conflict, text contains "two"
    // a: fact_get repo/k -> "two"; a: fact_set repo/k "three" -> committed
    // a: fact_set repo/k delete -> committed; a: fact_set repo/k "four" -> committed
    // b: fact_get repo/k -> "four"
}

#[test]
fn shell_fact_set_stays_last_write_wins() {
    // mcp a sets repo/k "one"; `atticdb fact set --key repo/k --content two` exits 0;
    // a second shell set with "three" exits 0; fact get shows "three".
}

#[test]
fn file_ref_after_another_writer_still_saves() {
    // mcp b sets file/<abs path> directly; mcp a calls file_ref on that path:
    // committed, because file_ref reads the key before it writes.
}
```

The assertions follow the comments. Each test checks `structuredContent.committed == true` or `isError == true`.

- [ ] **Step 2: Run, expect FAIL.**
  - Run: `cargo test -p atticdb --test it mcp::fact_set_never_overwrites -- --nocapture`
  - Expected: FAIL, because the second `fact_set` from b commits.

- [ ] **Step 3: Implement in `client.rs`.**
  - `fact_get`: on success, when `seen` is `Some`, record `key -> fact.lsn`. A null fact or a missing `lsn` records 0.
  - `fact_list`: record every listed fact's `key -> lsn`.
  - `fact_set` (put and delete): when `seen` is `Some`, add `"if_lsn": seen.get(key).unwrap_or(0)` to the body.
    - On commit, record the returned `lsn` for a put, or 0 for a delete.
    - On a reply whose `error` is `"conflict"`, call `self.fact_get` for the key, which records the new LSN.
    - Then return `tool_error` with this exact first line, followed by the current content, or `no current value`, and `by <agent>`:
      `conflict: this fact changed, or you haven't read it in this chat yet. Nothing was written. Its current value is below. Merge it with your change and call fact_set again.`
  - Detecting the conflict needs the HTTP `error` field. `write` folds it into a string today. Extend `write`'s judge, or add a variant, so `fact_set` can see `error == "conflict"` without matching on the text.
  - A retry key already returns the first receipt before the condition is checked (D76). Keep 1 retry key per call, as `write` does now.

- [ ] **Step 4: Update the MCP surface.**
  - `mcp.rs` calls `client.guard_facts()` after `Client::new(flags)?`.
  - Add 1 sentence to the `fact_set` description: `If the key changed since you last read it, or you haven't read it yet, nothing is written and you get its current value: merge and set again.`
  - Leave `INSTRUCTIONS` as it is.

- [ ] **Step 5: Add D107** to `docs/design/0002-decision-log.md`: "`atticdb mcp` makes `fact_set` conditional on the LSN it last saw per key, or 0 for an unseen key. A conflict returns the current value. The shell verbs stay last-write-wins. The model never sees `if_lsn`. Why: 2 agents sharing 1 tenant must not lose a write silently (0009)."

- [ ] **Step 6: Run the full checks, expect all PASS.**
  - Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  - Then the Python suites: `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTHONDONTWRITEBYTECODE=1 python3 -m pytest research -q`
  - Fix any existing test that sets a key written by another process. It must read first, or expect the conflict.

- [ ] **Step 7: Commit:** `git commit -s -m "feat(mcp): fact_set never overwrites a value it has not seen"`

### Task 2: `fact_history` MCP tool

**Files:**
- Modify: `crates/atticdb/src/client.rs`: `call` (line 420), plus a new `fact_history` method.
- Modify: `crates/atticdb/src/mcp.rs`: the tools list, annotations (read-only), and the header comment "8 tools" changed to "9 tools".
- Modify: `crates/atticdb/src/main.rs`: usage gets `atticdb fact history <connect> --key <key> [--max-bytes <n>]`, wired like `fact get`.
- Modify: `crates/atticdb/README.md` line 499, "The same 8 tools" changed to 9. Add a `fact_history` line wherever the tools are listed.
- Test: `crates/atticdb/tests/it/mcp.rs`.

**Interfaces:**
- Consumes: Task 1's `Client`. `fact_history` doesn't touch `seen`.
- Produces: the MCP tool `fact_history`, args `{key: string (required), max_bytes: integer, default 65536}`. It returns:
  - `structuredContent: {"revisions": [...], "truncated": bool}`, revisions newest first, in the shape of `GET /v1/facts/history`
  - text with 1 line per revision: `lsn <lsn> by <agent>: <content>`. A tombstone shows `(deleted)` and a forgotten revision `(forgotten)`.

- [ ] **Step 1: Write the failing tests.**
  - Set `repo/h` to "one", "two", then delete. `fact_history` returns 3 revisions, newest first: `deleted: true`, then "two", then "one".
  - `fact_history` on `repo/none` returns `revisions: []` with text `no revisions`, and no error.
  - `tools/list` holds 9 tools. `fact_history` has `readOnlyHint: true`.
  - Update the existing tool-count asserts.
- [ ] **Step 2: Run, expect FAIL:** `cargo test -p atticdb --test it mcp:: -- fact_history`
- [ ] **Step 3: Implement.** Build it like `fact_get`. Use `read("fact_history", "/v1/facts/history?key=..&max_bytes=..", "revisions", render)`.
- [ ] **Step 4: Run the full checks from Task 1, Step 6. Expect PASS.**
- [ ] **Step 5: Commit:** `git commit -s -m "feat(mcp): fact_history shows who changed a fact and when"`

### Task 3: Tenants-per-VM bench, local run

**Files:**
- Create: `bench/tenants.py`, Python 3 stdlib only, modeled on `bench/h2h_paced.py`.
- Create: `bench/TENANTS.md`, covering the runbook and the local results.
- Test: `bench/test_tenants.py`, pure functions only.

**Interfaces:**
- Produces: `python3 bench/tenants.py --layout per-process|shared --tenants N --active-pct P --duration S --think-ms T --out <tsv>`. It writes 1 TSV row per run with these columns: `layout tenants active_pct ops p50_ms p99_ms rss_mib_idle rss_mib_load fds threads disk_mib_per_tenant cold_start_ms`.

- [ ] **Step 1: Write failing tests** for the pure parts in `bench/test_tenants.py`:
  - `config_json(tenants: list[tuple[str, str]], segment_mib: int) -> dict` returns `{"tenants": [{"name", "token"}...], "segment_mib": 16}`.
  - `pick_active(n: int, pct: float, seed: int) -> list[int]` is deterministic per seed and returns `max(1, round(n*pct/100))` indexes.
  - `percentile(samples: list[float], q: float) -> float`, with the nearest-rank method on a fixed sample.
- [ ] **Step 2: Run, expect FAIL:** `python3 -m pytest bench/test_tenants.py -q`
- [ ] **Step 3: Implement `bench/tenants.py`.**
  - Setup: each tenant gets a random 32-hex token. The config is written mode 0600.
    - per-process: N data dirs, N configs and N `atticdb serve --listen none --unix-socket <dir>/s`.
    - shared: 1 dir and 1 config listing N tenants.
  - Wait for every socket to answer `GET /v1/facts/get?key=x` with 200.
  - Idle: sample after 5 s, summing `ps -o rss=` and the fd count (`lsof -p`, or `/proc/<pid>/fd` on Linux) and threads across all `serve` pids.
  - Load: each active tenant runs 1 client process with 1 request in flight. Before each op it sleeps a seeded uniform 0 to 2x think. Ops alternate `POST /v1/append` (256 bytes) and `POST /v1/facts/put` (256 bytes, 1 of 16 keys) with an idem key. It runs for `--duration`. Latency is the client-side round trip on the same host.
  - Sample RSS, fds and threads again at the end of the load.
  - Disk: `du -sk` of all data dirs divided by N.
  - Cold start, per-process only: stop 1 tenant's `serve`. Time from spawning it again to its first 200.
  - Teardown kills every `serve` it started, by pid.
- [ ] **Step 4: Run tests, expect PASS.** Then do a local smoke run with the dev binary: `ATTICDB_BIN=target/release/atticdb python3 bench/tenants.py --layout shared --tenants 10 --active-pct 20 --duration 20 --think-ms 50 --out /tmp/t.tsv`. Expected: 1 row, p99 a number, no errors.
- [ ] **Step 5: Local sweep on the Mac.** Both layouts, N = 1, 10 and 100, active 1, 5 and 20%, think 50 ms, 60 s each. Record it in `bench/TENANTS.md` as "local shape check, not the verdict". The Mac isn't the target VM.
- [ ] **Step 6: Commit:** `git commit -s -m "bench: tenants per VM, local shape run"`

### Task 4: Azure run and the verdict (gated on the owner's spend cap)

**Files:**
- Modify: `bench/TENANTS.md` (results, verdict, cost).
- Create: `bench/tenants-l8s-v3.tsv`.
- Modify: `docs/design/0009-default-memory-layer.md` (the 0c result line and the chosen layout).

- [ ] **Step 1:** Confirm the owner's spend cap is written in the ledger. Start zdb-g1 and copy the release binary and the bench. Check the VM auto-shuts at 18:00 UTC.
- [ ] **Step 2:** Run both layouts at N = 1, 10, 100 and 1,000, active 1, 5 and 20%, think 50 ms, 120 s each, on the local NVMe. Raise `ulimit -n` to 65536 first.
- [ ] **Step 3: Deallocate zdb-g1.**
  - Run: `az vm deallocate -g ZERODB-BENCH-RG -n zdb-g1`
  - Expected: `az vm show -d` shows `VM deallocated`.
  - Report the hours and the cost at $0.792/hr.
- [ ] **Step 4: Write the verdict.**
  - For each layout, the largest N with p99 at most 50 ms at 5% active.
  - Cost per tenant per month at 730 hours.
  - Whether the 2x rule switches us to shared.
  - Write the result into 0009's 0c section.
- [ ] **Step 5: Commit:** `git commit -s -m "bench: tenants per VM on L8s_v3, verdict"`
