# 0004: Local agents and rich context

Status: proposed, 2026-10-26. Nothing here is built. Each phase starts only after its open questions are answered.
Tracking: uber issues #71 (P1), #72 (P2), #73 (P3) and #74 (P4).

## Understanding

- **What:** make AtticDB the memory a developer's local agent harness connects to with 1 command, and let that memory hold more than text. Links, images, audio, video and files, compressed when it pays.
- **Why:** developers are moving agent work to local setups for speed and control. Today, connecting AtticDB takes about 5 manual steps. You build from source, run `init`, start `serve` with absolute paths, copy a token, and hand-edit the harness's MCP config. And a turn holds only UTF-8 text, up to 4 MiB. A screenshot or a recording can't be stored at all.
- **Who:** 1 developer on a macOS or Linux laptop or workstation, running 1 to 3 harnesses, such as Claude Code, Codex, Cursor and Claude Desktop. Not a shared server.
- **Constraints:**
  - Every ack stays durable: synced before the reply.
  - No new outbound network calls. AtticDB makes none today.
  - The data directory stays on a local disk.
  - AtticDB is Unix-only today, and no release binaries exist yet.
  - Each new dependency has to earn its place.
- **Non-goals:** an S3 protocol server, tiering to remote object storage, fetching the contents of links, vector search, Windows, and sharing memory across machines.

## Assumptions

Each assumption is marked, so it can be confirmed or corrected before its phase starts.

- **A1:** macOS (arm64, x86_64) and Linux (x86_64, arm64) cover the first users. Windows waits for a user who needs it.
- **A2:** Per developer, expect up to a few million turns and tens of GB of objects. The largest single object is a screen recording, up to about 1 GiB.
- **A3:** 1 tenant per user, named `me` by default. Sessions are per project directory, so 2 repos don't share a window unless asked.
- **A4:** Claude Code comes first, because it has both MCP and hooks. Codex, Cursor and Claude Desktop get MCP registration only, until they expose comparable hooks.
- **A5:** Automatic capture writes transcripts verbatim, and transcripts can hold secrets. So capture is opt-in at connect time, never a silent default.
- **A6:** "Something like S3" means storing objects locally. It doesn't mean speaking the S3 protocol, or reaching an existing S3 or MinIO.
- **A7:** Disk use isn't yet a complaint. Compression waits for dogfood numbers (#46) from the payload metrics in #36.

## Open questions

- **Q1 (A4):** which harness is first? Claude Code is assumed.
- **Q2 (A5):** should capture redact anything, such as env-var-shaped secrets, before writing? The default plan is no redaction, documented loudly, with capture off by default.
- **Q3 (A6):** does anyone need AtticDB to talk to their own S3 or MinIO, or to serve S3 clients? If yes, that's a separate track, with its own evidence.
- **Q4 (A2):** is 1 GiB per object enough, or is long video a real use?
- **Q5:** identical bytes stored by 2 tenants share 1 file, each with its own ref (C-objects). Is that acceptable, or should tenants never share files, at the cost of duplicate bytes?

## What exists today

- `atticdb mcp` starts the server if none is running, through the Unix socket in `LOCK`. It needs a token: `ATTICDB_TOKEN`, `--token-file`, or `--config` with `--tenant <name>`. It also needs a place to find the server: `--url`, `--data-dir`, or a config with `data_dir`. Relative paths are resolved against the working directory. The README asks for absolute paths in harness configs, because a harness can start `atticdb mcp` from any directory.
- Turns: `content` is a JSON string, so UTF-8 only. Per turn, content is at most 4 MiB and `meta` at most 64 KiB.
- `/metrics` already has `turn_content_bytes` (a size histogram) and `turn_repeats` (#36).
- Storage tested: local NVMe on Linux ext4, and Azure Premium SSD v2 (network block SSD) on ext4. macOS APFS for development. Never tested: HDD, NFS, SMB, FUSE, cloud-synced folders.

## Design

### Track A: connect with 1 command

- **A-install:** GitHub Release binaries for the 4 targets in A1, an `install.sh` that checks the SHA-256, and `cargo install` from the repo. Homebrew later.
- **A-home:** with no flags, AtticDB uses a per-user home:
  - on Linux, `$XDG_DATA_HOME/atticdb` when that variable is set and not empty, else `$HOME/.local/share/atticdb`, as the XDG spec says;
  - on macOS, `~/Library/Application Support/atticdb`;
  - inside it, a `0600` config with tenant `me` and the data directory.

  Every existing flag still overrides. `atticdb mcp` with no flags then just works.
- **A-connect:** `atticdb connect <claude-code|codex|cursor|claude-desktop> [--project]`. It creates the home if it's missing, then registers `atticdb mcp` with the harness.
  - Where the harness has its own command, use it: `claude mcp add`, `codex mcp add`.
  - Otherwise, merge into its JSON config with a timestamped backup.
  - Running it twice changes nothing. It prints exactly what it changed.
  - `atticdb disconnect <harness>` removes only what AtticDB added: its MCP entry and, with capture, its hooks, found by name. Other settings and servers, including ones added after `connect`, stay. The backup is kept for manual recovery and never restored automatically.
  - The token stays in AtticDB's own config and is never written into a harness file. `atticdb mcp` reads the token from AtticDB's config, so the harness config holds no secret.
- **A-doctor:** `atticdb doctor` checks:
  - the binary version and the home's permissions;
  - that the server is reachable, and each harness registration;
  - free space against the floor;
  - the data directory's filesystem. It warns on NFS, SMB, FUSE, or a path inside iCloud Drive, Dropbox, OneDrive or Google Drive.
- **A-sessions:** default agent is the harness name. Default session is `<name>-<hash>`: the project directory's name, cut to at most 229 bytes on a UTF-8 character boundary, then `-` and 8 hex digits of a hash of its full path. That leaves room for capture's `/<16 hex>` suffix within the 255-byte session limit. This `<name>-<hash>` is the **project id**. Everything per project is keyed by it, never by the bare directory name, so 2 repos both named `app` stay apart. MCP tools still take explicit names.
- **A-readme:** an Install section that says where the data directory should live:
  - Tested: local NVMe and network block SSD on Linux ext4; macOS APFS for development.
  - Works but untested: SATA SSD and HDD. Each ack waits for a device sync, which is milliseconds on a spinning disk.
  - Don't use: NFS, SMB, FUSE mounts, or cloud-synced folders. Their `fsync` and `flock` guarantees vary, and sync clients copy files mid-write.

### Track B: automatic capture (Claude Code)

`atticdb connect claude-code --capture` also installs hooks.
- **SessionStart:** injects the last N turns of the project's newest capture session, plus the facts under `project/<project id>/`, as extra context within a byte budget.
- **UserPromptSubmit and Stop:** append the user prompt and the assistant reply. Each harness session captures into its own AtticDB session, `<project session>/<16 hex>`, where the 16 hex digits are a hash of the harness session id. That keeps it within 255 bytes (A-sessions). The registration file and cursor fact store the full harness session id. So progress for one transcript never depends on how busy the rest of the project is.
  - Each transcript entry is its own append of 1 turn, with an `idem_key` of the harness session id plus the entry's position. So a retried key always carries the same content, however much the transcript grows in between. Hooks run a few times per prompt, and an append costs about 1 device sync, so this costs little.
  - Each captured turn's `meta` records its position in `transcript_path`.
  - Before any network call, a hook registers its session: it writes `<home>/capture/<harness session id>.json` with the `transcript_path`. The first time, it creates `<home>/capture/` and `fsync`s `<home>`, so the directory's own entry is durable before any registration goes in it. It writes a temp file, `fsync`s it, renames it into place, then `fsync`s the directory. It reuses an existing registration only if it parses and names the same `transcript_path`, and otherwise rewrites it the same way. So a session stays discoverable even if AtticDB is down for all of it, and a half-written file can't hide it.
  - A fact `capture/<harness session id>` holds the cursor: the last captured position, the AtticDB seq of its turn, and the `transcript_path`. Facts are never evicted, so the cursor survives the bounded dedup window (I6) and restarts.
  - A hook reads the cursor, then pages through its own session's turns after that seq until the window reports nothing left. It uses `since_seq` and follows `truncated`. Each read uses the maximum budget, 8 MiB. A turn's `content` plus `meta` is at most 4 MiB plus 64 KiB, so every page returns at least 1 turn, and reconciliation always makes progress. If a page still comes back empty and truncated, the hook stops and exits 0 without appending anything. No backlog is appended until reconciliation completes. Only this harness session writes there, and the cursor moves after every 16 entries. So the pages hold at most 16 turns. The highest position found wins. This covers a crash between appending and moving the cursor. Facts can't share a batch with turns (D76), so the 2 writes can't be atomic.
  - Then the hook drains the backlog in order, 1 entry per append, moving the cursor with a conditional put (`if_lsn`) every 16 entries and at the end. It stops when the transcript is caught up, or when its timeout is near.
  - The whole cycle runs under an exclusive `flock` on `<home>/capture/<harness session id>.lock`: read the cursor, reconcile, append, move the cursor. So 2 hooks for the same harness session never overlap. Hooks run on the harness's machine, the same one as AtticDB's home. A hook that can't get the lock within its timeout exits 0. Nothing is lost, because the next hook resumes from the cursor.
  - The `idem_key` from the harness session id and position, and the conditional put (`if_lsn`), stay as a second guard.
  - **The stated bound.** A killed hook's append can still be in flight when the lock is released. If the next hook reconciles before it commits, both carry the same `idem_key`. Whichever is prepared second returns the first's result, so no duplicate is written. A duplicate needs the first one's key evicted between those 2 prepares. That takes the tenant writing more than its dedup window, 1M keys by default, within about 1 hook timeout. The guarantee is therefore exactly once, unless a tenant writes over 1M keys during a hook's few-second timeout. We accept and document that, rather than add a server-side fence.
  - A fact `project/<project id>/latest-capture` names the newest capture session. SessionStart reads it to find the turns to inject.
  - **Large entries.** A whole transaction, with frame headers, turn fields, `meta` and footer, must fit in `MAX_FRAME_SIZE`, 4 MiB. So a turn's content can't be a full 4 MiB. The HTTP request also has a body limit, 8 MiB, and JSON escaping can grow a control character to 6 bytes (`\u001b`). So capture cuts each chunk where adding the next character would take either its raw size past 3 MiB or its JSON-escaped size past 7 MiB. That leaves room for `meta` (at most 64 KiB) and encoding under both limits. Escaping is deterministic, so the same entry always splits at the same UTF-8 character boundaries. Every chunk carries at least 7/6 MiB of raw content. Each chunk is its own turn, with `idem_key` `<harness session id>:<position>:<chunk>`. Its `meta` records the position and `chunk i of n`. An entry is stored up to 64 chunks. Since every chunk carries at least 7/6 MiB, that covers at least 74 MiB of any content. Beyond the 64th chunk the rest is dropped, and the last chunk's `meta` says `truncated: true`. Completeness covers entries up to 64 MiB.
  - **Resuming a partial entry.** The cursor holds a position and a next chunk. Reconciliation takes the newest captured turn: if it is chunk `i` of `n` with `i < n`, capture resumes that entry at chunk `i + 1`. Otherwise it resumes at the next position. The cursor's position moves past an entry only once its last chunk is stored. So a hook killed after chunk 1 of 3 resumes at chunk 2, never re-sends chunk 1, and never skips chunk 3.
  - **Forget wins over capture.** A forgotten turn reads back as `{seq, lsn, redacted: true}` (I5), with no position. So reconciliation can't tell which transcript entry it was. If reconciliation meets a redacted turn after the cursor, the hook retires the session: it sets the cursor fact to `retired` and appends nothing. Hooks and `capture sync` never write to a retired session, and `doctor` lists it. So a forgotten turn can't come back through replay, at the cost of no further capture for that harness session.
  - To forget a whole capture session, use `atticdb capture forget <session>`. It takes the session's capture lock, so it waits for a running hook to finish. It sets the cursor to `retired` first, then forgets the session. A hook checks the cursor while holding the lock, so no new hook appends after that. A hook killed with 1 append in flight can still land that 1 turn after the forget. So `capture forget` repeats the forget once after the hook timeout has passed. A plain `forget` through the API doesn't stop a running capture, and the docs say so.
- **PreCompact:** drains the backlog the same way, as far as its timeout allows, before the harness compacts its context. The transcript file stays on disk, so anything left over is still captured later.
- **Completeness, stated plainly:** an entry is captured by the first hook run that reaches it, or by `atticdb capture sync`. A session whose last hook times out or fails keeps an uncaptured tail until `atticdb capture sync` runs. That command walks the local registration files, so it also finds sessions that never reached AtticDB. It drains every transcript that is ahead of its cursor, or has no cursor yet. `atticdb doctor` reports those sessions. The one gap is a hook that dies before writing its registration file. That session is captured only if a later hook of it runs.

Each hook is a short `atticdb` subcommand with a hard timeout. If AtticDB is down, the hook exits 0 and the session goes on without memory. Capture never blocks the agent.
Other harnesses use the MCP tools only, until they have equivalent hooks.

### Track C: links and objects

- **C-links:** a turn or a fact can carry `refs`: a list of `{url, title?, mime?}` in `meta`. AtticDB stores them and returns them. It never fetches them. No format change.
- **C-files (shipped, D94):** a file ref is the fact `file<absolute path>`. Content is a 1-line `about` written by the agent. Meta holds `{"file": {bytes, modified_ns, blake3, mime}}`. The file stays where it is.
  - **Lookup:** the facts B-tree gives exact lookup by path and folder listing by key prefix. No new index.
  - **Versions:** each save of a changed file is a fact revision. The server keeps the newest 16 per path. `file_get versions: true` lists them and names the one whose blake3 is on disk now.
  - **Status:** `ok`, `changed` or `missing`, checked by the client at read time. Size and mtime first. The hash decides when they differ, so a touched file stays `ok`.
  - **Unchanged saves:** the same bytes and about as the current ref write nothing.
  - **Limits:** keys are at most 1024 bytes, and a tenant holds up to 100,000 facts. Hashing and stat run in the client, as the user. The model sends a path, never bytes.
  - **Surfaces:** MCP `file_ref {path, about}` and `file_get {path, versions?}`. Shell `atticdb file save` and `atticdb file get`.
- **C-objects (dropped 2026-10-31, D94):** a local object store that copies bytes into the data directory. Local users keep their files where they are and need references, not copies. The design is in git history at 122adfe if a remote or shared store ever needs it.

## Phases

| Phase | Scope | Gate to start | Format change |
|---|---|---|---|
| P1 (#71) | Track A: install, home, `connect`, `doctor`, README storage guidance | Q1 answered | No |
| P2 (#72) | Track B: Claude Code capture, A-sessions | P1 shipped, Q2 answered | No |
| P3 (#73) | Track C: file refs (shipped). Objects dropped for local users (D94) | None | No |
| P4 (#74) | Track D: compression | Dogfood numbers from #46 meet the gate | Yes, a frame flag |

Deferred, with the evidence that would reopen each:
- **Remote object tiering:** a user's object store outgrows a local disk.
- **S3-compatible API:** a user wants an S3 client to read or write AtticDB.
- **Windows:** a user asks.
- **Fetching and snapshotting link contents:** dogfood shows dead links lose context that mattered.
- **KV cache tier and folders:** see 0005. KV waits for a self-hosted-model user. Folders follow objects.

## Testing

- **connect and doctor:** run against a temporary `HOME`, with stub `claude` and `codex` binaries on `PATH`. Assert the exact config diff and that a second run changes nothing. Between `connect` and `disconnect`, add another MCP server and change a setting. Assert that `disconnect` removes only AtticDB's entries and keeps both changes.
- **Capture hooks:** feed recorded hook JSON into the subcommands. Assert that no turn is written twice across repeated runs, including after the dedup window has evicted their keys (a test with a tiny window). Interleave 2 harness sessions and a manual MCP append in 1 project, and assert each transcript is captured exactly once into its own session. Fill the project session with 10,000 other turns between 2 hooks, and assert the hook still finishes within its timeout. Crash after appending 15 turns of 256 KiB each, before the cursor moves, then rerun with a tiny dedup window and a 1 MiB read budget, so reconciliation spans several pages. Assert no duplicate. Grow the transcript while a killed hook's append is in flight, then rerun. Assert no `IdempotencyConflict` and no duplicate. Leave 100 entries pending after the last hook. Assert `doctor` reports the lag and `capture sync` drains it. Keep AtticDB down for a whole new session, then start it. Assert `capture sync` finds and captures that session. Commit a turn, kill the hook before the cursor moves, forget that turn, evict its key with a tiny dedup window, then run `capture sync`. Assert the session is retired and the forgotten text is not written again. Run `capture forget` while a hook is mid-drain, and while a killed hook's append is in flight. Assert no captured turn is readable after it returns, and no hook appends afterwards. Use a 255-byte project directory name with multibyte characters, and assert the generated session names fit. Capture in 2 projects both named `app` in different parents, and assert each SessionStart injects only its own turns and facts. Capture entries of exactly 3 MiB, 3 MiB plus 1 byte, just under 4 MiB, 10 MiB and 100 MiB, a 64 MiB entry made only of `U+001B`, and a 100 MiB entry made only of `U+001B`. Kill the hook after chunk 1 of 3, evict its key with a tiny dedup window, and rerun. Assert each chunk is stored once, in order, that none is skipped, that no append fails with `BatchTooLarge`, that no request fails with HTTP 413, that the 64 MiB escape-heavy entry is stored whole, that the 100 MiB escape-heavy entry fills all 64 chunks and is marked `truncated`, and that the plain 100 MiB entry is stored whole in 34 chunks. Kill the hook between the append and the cursor update, then rerun. Run 2 hooks for the same harness session at once, with a tiny dedup window, and assert no duplicate. Kill a hook while its append is in flight, run the next hook at once with the default window, and assert no duplicate. Assert that AtticDB being down exits 0 within the timeout.
- **Objects:**
  - a sim case for a crash at every step of the put, including the first-use directory setup;
  - a power-loss case in `tests/crash/power_loss.py`, starting from a data directory with no `objects/` yet;
  - forget hiding the object from every read path, after 2 independent puts of the same bytes, and a put after the forget being visible again;
  - free-space floor rejection before any bytes are written;
  - `object_put` and `object_get` refusing paths outside the allowed roots, including through a symlink that points out, and refusing everything when no root is set.

## Decision log (proposed)

| Decision | Alternatives | Why |
|---|---|---|
| Register through the harness's own command where it exists, else merge its JSON with a backup | Only print instructions, or always edit files | Its own command tracks format changes. Printing leaves the 5-step problem |
| Keep the token out of harness configs | Pass `ATTICDB_TOKEN` in the MCP `env` block | Harness configs get shared and committed. `atticdb mcp` can read the token itself |
| Capture is opt-in | Capture by default | Transcripts hold secrets. Users must choose to store them |
| Objects live in a content-addressed store beside the tape, with refs on the tape | Objects as tape records, or an S3 protocol server | Frame cap, recovery cost and streaming rule out the tape. S3 is a large surface nobody has asked for |
| Hide, not erase, for objects | Reference counting with delete now | Matches I10 and the deferred compaction. Erase needs the same GC compaction will |
| Object puts carry an `idem_key` | Dedup by digest only | A digest can't tell a retry from a new put after a forget. The `idem_key` keeps a lost reply from restoring forgotten content |
| Each harness session captures into its own AtticDB session, with a cursor fact, reconciled under a per-session file lock | Rely on `idem_key` only, or share the project session and scan it | The dedup window is bounded (I6). A shared session makes reconciliation scan other sessions' turns, without a bound. A session of its own bounds it to 1 page. Without the lock, 2 overlapping hooks can both append once a key is evicted |
| Object forget hides every ref the tenant holds to the digest, up to the forget's LSN | Hide only 1 ref | 2 puts of the same bytes would leave the object readable through the other ref |
| Capture appends 1 transcript entry per write | Batches of several entries | A batch key covers its whole content, so a transcript that grows during a retry would change the batch. 1 entry per key never changes |
| A hook registers its session in a local file before any network call | Rely on cursor facts in AtticDB | A session with AtticDB down from the start would have no cursor and could never be found |
| A redacted turn after the cursor retires the capture session | Skip it and continue | Forget removes the position, so continuing could append the forgotten entry again once its key is evicted |
| Per-project keys use the path-hashed project id | The directory name | 2 repos with the same name would overwrite each other's pointer and facts |
| Object file tools work only inside configured roots, after resolving symlinks | Any path the model sends | AtticDB runs beside the trusted harness (D79). A model-supplied path could read credentials or overwrite files |
| Entries are chunked deterministically by raw size (3 MiB) and JSON-escaped size (7 MiB), up to 64 chunks, and resume at the first missing chunk | Skip them, chunk at 4 MiB, or raise the HTTP limit | Skipping stalls the in-order drain behind 1 entry forever. A 4 MiB chunk overflows the 4 MiB transaction limit, and an escape-heavy 3 MiB chunk overflows the 8 MiB HTTP body. Chunking keeps both limits as they are |
| A killed hook's in-flight append relies on the dedup window, with the bound stated | A server-side capture fence | A duplicate needs over 1M writes from 1 tenant within a few-second hook timeout. A fence is a new server mechanism for a case the window already bounds |
| Object directories are created and synced once, before any ref | Create shards on first use | A put that creates a shard would also have to sync its parent. Doing it once removes the case |
| `disconnect` removes only AtticDB's entries | Restore the backup | A restore would drop anything the user changed after `connect` |
| No outbound network calls | Fetch link contents, or call remote stores | Keeps AtticDB local and predictable. Links stay references |
| Compression only on dogfood evidence | Ship zstd now | #36 chose to measure first. A frame flag is a format commitment |
