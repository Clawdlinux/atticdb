# 0004: Local agents and rich context

Status: proposed, 2026-10-26. Nothing here is built. Each phase starts only after its open questions are answered.
Tracking: uber issues #71 (P1), #72 (P2), #73 (P3) and #74 (P4).

## Understanding

- **What:** make zerodb the memory a developer's local agent harness connects to with 1 command, and let that memory hold more than text. Links, images, audio, video and files, compressed when it pays.
- **Why:** developers are moving agent work to local setups for speed and control. Today, connecting zerodb takes about 5 manual steps. You build from source, run `init`, start `serve` with absolute paths, copy a token, and hand-edit the harness's MCP config. And a turn holds only UTF-8 text, up to 4 MiB. A screenshot or a recording can't be stored at all.
- **Who:** 1 developer on a macOS or Linux laptop or workstation, running 1 to 3 harnesses, such as Claude Code, Codex, Cursor and Claude Desktop. Not a shared server.
- **Constraints:**
  - Every ack stays durable: synced before the reply.
  - No new outbound network calls. zerodb makes none today.
  - The data directory stays on a local disk.
  - zerodb is Unix-only today, and no release binaries exist yet.
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
- **Q3 (A6):** does anyone need zerodb to talk to their own S3 or MinIO, or to serve S3 clients? If yes, that's a separate track, with its own evidence.
- **Q4 (A2):** is 1 GiB per object enough, or is long video a real use?
- **Q5:** identical bytes stored by 2 tenants share 1 file, each with its own ref (C-objects). Is that acceptable, or should tenants never share files, at the cost of duplicate bytes?

## What exists today

- `zerodb mcp` starts the server if none is running, through the Unix socket in `LOCK`. It needs a token: `ZERODB_TOKEN`, `--token-file`, or `--config` with `--tenant <name>`. It also needs a place to find the server: `--url`, `--data-dir`, or a config with `data_dir`. Relative paths are resolved against the working directory. The README asks for absolute paths in harness configs, because a harness can start `zerodb mcp` from any directory.
- Turns: `content` is a JSON string, so UTF-8 only. Per turn, content is at most 4 MiB and `meta` at most 64 KiB.
- `/metrics` already has `turn_content_bytes` (a size histogram) and `turn_repeats` (#36).
- Storage tested: local NVMe on Linux ext4, and Azure Premium SSD v2 (network block SSD) on ext4. macOS APFS for development. Never tested: HDD, NFS, SMB, FUSE, cloud-synced folders.

## Design

### Track A: connect with 1 command

- **A-install:** GitHub Release binaries for the 4 targets in A1, an `install.sh` that checks the SHA-256, and `cargo install` from the repo. Homebrew later.
- **A-home:** with no flags, zerodb uses a per-user home:
  - on Linux, `$XDG_DATA_HOME/zerodb` when that variable is set and not empty, else `$HOME/.local/share/zerodb`, as the XDG spec says;
  - on macOS, `~/Library/Application Support/zerodb`;
  - inside it, a `0600` config with tenant `me` and the data directory.

  Every existing flag still overrides. `zerodb mcp` with no flags then just works.
- **A-connect:** `zerodb connect <claude-code|codex|cursor|claude-desktop> [--project]`. It creates the home if it's missing, then registers `zerodb mcp` with the harness.
  - Where the harness has its own command, use it: `claude mcp add`, `codex mcp add`.
  - Otherwise, merge into its JSON config with a timestamped backup.
  - Running it twice changes nothing. It prints exactly what it changed.
  - `zerodb disconnect <harness>` removes only what zerodb added: its MCP entry and, with capture, its hooks, found by name. Other settings and servers, including ones added after `connect`, stay. The backup is kept for manual recovery and never restored automatically.
  - The token stays in zerodb's own config and is never written into a harness file. `zerodb mcp` reads the token from zerodb's config, so the harness config holds no secret.
- **A-doctor:** `zerodb doctor` checks:
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

`zerodb connect claude-code --capture` also installs hooks.
- **SessionStart:** injects the last N turns of the project's newest capture session, plus the facts under `project/<project id>/`, as extra context within a byte budget.
- **UserPromptSubmit and Stop:** append the user prompt and the assistant reply. Each harness session captures into its own zerodb session, `<project session>/<16 hex>`, where the 16 hex digits are a hash of the harness session id. That keeps it within 255 bytes (A-sessions). The registration file and cursor fact store the full harness session id. So progress for one transcript never depends on how busy the rest of the project is.
  - Each transcript entry is its own append of 1 turn, with an `idem_key` of the harness session id plus the entry's position. So a retried key always carries the same content, however much the transcript grows in between. Hooks run a few times per prompt, and an append costs about 1 device sync, so this costs little.
  - Each captured turn's `meta` records its position in `transcript_path`.
  - Before any network call, a hook registers its session: it writes `<home>/capture/<harness session id>.json` with the `transcript_path`. The first time, it creates `<home>/capture/` and `fsync`s `<home>`, so the directory's own entry is durable before any registration goes in it. It writes a temp file, `fsync`s it, renames it into place, then `fsync`s the directory. It reuses an existing registration only if it parses and names the same `transcript_path`, and otherwise rewrites it the same way. So a session stays discoverable even if zerodb is down for all of it, and a half-written file can't hide it.
  - A fact `capture/<harness session id>` holds the cursor: the last captured position, the zerodb seq of its turn, and the `transcript_path`. Facts are never evicted, so the cursor survives the bounded dedup window (I6) and restarts.
  - A hook reads the cursor, then pages through its own session's turns after that seq until the window reports nothing left. It uses `since_seq` and follows `truncated`. Each read uses the maximum budget, 8 MiB. A turn's `content` plus `meta` is at most 4 MiB plus 64 KiB, so every page returns at least 1 turn, and reconciliation always makes progress. If a page still comes back empty and truncated, the hook stops and exits 0 without appending anything. No backlog is appended until reconciliation completes. Only this harness session writes there, and the cursor moves after every 16 entries. So the pages hold at most 16 turns. The highest position found wins. This covers a crash between appending and moving the cursor. Facts can't share a batch with turns (D76), so the 2 writes can't be atomic.
  - Then the hook drains the backlog in order, 1 entry per append, moving the cursor with a conditional put (`if_lsn`) every 16 entries and at the end. It stops when the transcript is caught up, or when its timeout is near.
  - The whole cycle runs under an exclusive `flock` on `<home>/capture/<harness session id>.lock`: read the cursor, reconcile, append, move the cursor. So 2 hooks for the same harness session never overlap. Hooks run on the harness's machine, the same one as zerodb's home. A hook that can't get the lock within its timeout exits 0. Nothing is lost, because the next hook resumes from the cursor.
  - The `idem_key` from the harness session id and position, and the conditional put (`if_lsn`), stay as a second guard.
  - **The stated bound.** A killed hook's append can still be in flight when the lock is released. If the next hook reconciles before it commits, both carry the same `idem_key`. Whichever is prepared second returns the first's result, so no duplicate is written. A duplicate needs the first one's key evicted between those 2 prepares. That takes the tenant writing more than its dedup window, 1M keys by default, within about 1 hook timeout. The guarantee is therefore exactly once, unless a tenant writes over 1M keys during a hook's few-second timeout. We accept and document that, rather than add a server-side fence.
  - A fact `project/<project id>/latest-capture` names the newest capture session. SessionStart reads it to find the turns to inject.
  - **Large entries.** A whole transaction, with frame headers, turn fields, `meta` and footer, must fit in `MAX_FRAME_SIZE`, 4 MiB. So a turn's content can't be a full 4 MiB. Capture budgets 3 MiB of content per turn, leaving at least 1 MiB for `meta` (at most 64 KiB) and encoding. An entry over 3 MiB, such as a big tool result, is split into chunks of at most 3 MiB. Cuts fall on UTF-8 character boundaries at fixed offsets, so the same entry always splits the same way. Each chunk is its own turn, with `idem_key` `<harness session id>:<position>:<chunk>`. Its `meta` records the position and `chunk i of n`. An entry is stored up to 22 chunks, 66 MiB. Beyond that the rest is dropped, and the last chunk's `meta` says `truncated: true`. Completeness covers entries up to 64 MiB.
  - **Resuming a partial entry.** The cursor holds a position and a next chunk. Reconciliation takes the newest captured turn: if it is chunk `i` of `n` with `i < n`, capture resumes that entry at chunk `i + 1`. Otherwise it resumes at the next position. The cursor's position moves past an entry only once its last chunk is stored. So a hook killed after chunk 1 of 3 resumes at chunk 2, never re-sends chunk 1, and never skips chunk 3.
  - **Forget wins over capture.** A forgotten turn reads back as `{seq, lsn, redacted: true}` (I5), with no position. So reconciliation can't tell which transcript entry it was. If reconciliation meets a redacted turn after the cursor, the hook retires the session: it sets the cursor fact to `retired` and appends nothing. Hooks and `capture sync` never write to a retired session, and `doctor` lists it. So a forgotten turn can't come back through replay, at the cost of no further capture for that harness session.
  - To forget a whole capture session, use `zerodb capture forget <session>`. It takes the session's capture lock, so it waits for a running hook to finish. It sets the cursor to `retired` first, then forgets the session. A hook checks the cursor while holding the lock, so no new hook appends after that. A hook killed with 1 append in flight can still land that 1 turn after the forget. So `capture forget` repeats the forget once after the hook timeout has passed. A plain `forget` through the API doesn't stop a running capture, and the docs say so.
- **PreCompact:** drains the backlog the same way, as far as its timeout allows, before the harness compacts its context. The transcript file stays on disk, so anything left over is still captured later.
- **Completeness, stated plainly:** an entry is captured by the first hook run that reaches it, or by `zerodb capture sync`. A session whose last hook times out or fails keeps an uncaptured tail until `zerodb capture sync` runs. That command walks the local registration files, so it also finds sessions that never reached zerodb. It drains every transcript that is ahead of its cursor, or has no cursor yet. `zerodb doctor` reports those sessions. The one gap is a hook that dies before writing its registration file. That session is captured only if a later hook of it runs.

Each hook is a short `zerodb` subcommand with a hard timeout. If zerodb is down, the hook exits 0 and the session goes on without memory. Capture never blocks the agent.
Other harnesses use the MCP tools only, until they have equivalent hooks.

### Track C: links and objects

- **C-links:** a turn or a fact can carry `refs`: a list of `{url, title?, mime?}` in `meta`. zerodb stores them and returns them. It never fetches them. No format change.
- **C-objects:** a local, content-addressed object store in the data directory.
  - **Layout:** the first time a data directory needs it, the server creates `objects/`, `objects/tmp/`, `objects/sha256/` and all 256 `objects/sha256/<2 hex>/` shards. It `fsync`s each new directory's parent, up to the data directory, before writing any ref. No put ever creates a directory. So a ref can't point into a directory whose entry was lost.
  - **Write:** `PUT /v1/objects` streams a body with a `Content-Type`. The server writes `objects/tmp/<random>`, hashes it while writing, then `fsync`s. It renames it to `objects/sha256/<2 hex>/<62 hex>` and `fsync`s that shard directory. Then it appends an `ObjectRef {tenant, idem_key, digest, bytes, mime}` record to the tape. The `idem_key` is on the tape so replay can rebuild the put's dedup entry, like every other write's (I6). A retry after a restart still returns the original result within the window. The ack comes after that record is durable. The reply is `{"object":"sha256:<hex>","bytes":N}`.
  - **Read:** `GET /v1/objects/sha256:<hex>` streams the bytes, only if the caller's tenant holds a live ref. Turns and facts point at objects through `refs`, using an `object:` URL.
  - **Retries:** a put carries an `idem_key`, like every other write (I6). Within the dedup window, a retry returns the original result and writes nothing, even if the ref was forgotten in between. So a lost reply can't bring back a forgotten object. After eviction a retry writes a new ref, as I6 allows for every write. The digest only dedups the file on disk.
  - **Limits:** 1 GiB per object by default, configurable. Objects count against the free-space floor, which is checked before writing.
  - **Forget (I10):** `forget` gets an object selector, `object: sha256:<hex>`. It hides every ref the tenant holds to that digest at or below the forget's LSN, so 2 earlier puts of the same bytes are both hidden. A put after the forget makes a new, visible ref, just as an append after a forget makes a new turn. Forgetting a turn that points at an object hides the turn, not the object. Bytes stay on disk until compaction exists, like turns today. Tenants never share a ref. Identical bytes stored by 2 tenants share 1 file, but each tenant's refs are separate (Q5).
  - **Crashes:** a crash after the rename but before the ref leaves an unreferenced file. It's invisible, and a later sweep removes it. A ref can never point at a missing file, because the file is synced first.
- **C-mcp:**
  - **File access is limited to allowed roots.** `object_put` and `object_get` take host paths, and a model could ask for `~/.ssh/id_rsa` or write over a dotfile. So each path is resolved with symlinks first, then must fall inside an allowed root. The roots are listed in zerodb's config under `object_roots`. `zerodb connect --project` adds that project directory. A user-level connect adds none. With no roots, both tools return an error that says how to add one, and `zerodb object` on the shell still works. A path outside every root is refused. The adapter never asks the model to approve it, because the user wasn't asked.
  - `object_put {path}`: the adapter streams a local file. The model never sends base64.
  - `object_get {object, path?}`: an image under a size cap comes back as MCP image content, and `path` is optional. Anything else needs `path`. zerodb writes the object there, refuses to overwrite an existing file, and returns the path. Without `path` it returns an error with the object's size and type, so the caller can choose where it goes. It never writes to a default location.
  - Shell: `zerodb object put <file>` and `zerodb object get <object> -o <file>`. The Python client gets matching calls.

Why objects live outside the tape:
- Frames cap at 4 MiB.
- Recovery scans, replay and checkpoints all pay for every byte on the tape. A single 1 GB recording would roll about 16 segments and slow every reopen.
- Objects need streaming, not JSON.

Why not the S3 protocol: SigV4 auth, multipart upload and bucket semantics are a large surface. No user has asked for S3 clients pointed at zerodb (Q3). A `refs` URL can already point at `s3://` or `https://` objects stored elsewhere.

### Track D: compression (evidence-gated)

- **Gate:** dogfood data (#46) shows text turns would save 30% or more of disk, and compression costs under 5% of append throughput at 16 and 64 writers on NVMe.
- **Candidate:** per-record zstd above a size threshold, marked by a frame flag. That's a format version bump plus 1 dependency.
- **Objects:** compressed by MIME type, skipping types that are already compressed: JPEG, PNG, MP4, ZIP.
- Without the data, nothing here gets built.

## Phases

| Phase | Scope | Gate to start | Format change |
|---|---|---|---|
| P1 (#71) | Track A: install, home, `connect`, `doctor`, README storage guidance | Q1 answered | No |
| P2 (#72) | Track B: Claude Code capture, A-sessions | P1 shipped, Q2 answered | No |
| P3 (#73) | Track C: links, then objects | S5 forget (#45) designed, so object refs follow I10. Q3 to Q5 answered | Yes, a new record kind |
| P4 (#74) | Track D: compression | Dogfood numbers from #46 meet the gate | Yes, a frame flag |

Deferred, with the evidence that would reopen each:
- **Remote object tiering:** a user's object store outgrows a local disk.
- **S3-compatible API:** a user wants an S3 client to read or write zerodb.
- **Windows:** a user asks.
- **Fetching and snapshotting link contents:** dogfood shows dead links lose context that mattered.

## Testing

- **connect and doctor:** run against a temporary `HOME`, with stub `claude` and `codex` binaries on `PATH`. Assert the exact config diff and that a second run changes nothing. Between `connect` and `disconnect`, add another MCP server and change a setting. Assert that `disconnect` removes only zerodb's entries and keeps both changes.
- **Capture hooks:** feed recorded hook JSON into the subcommands. Assert that no turn is written twice across repeated runs, including after the dedup window has evicted their keys (a test with a tiny window). Interleave 2 harness sessions and a manual MCP append in 1 project, and assert each transcript is captured exactly once into its own session. Fill the project session with 10,000 other turns between 2 hooks, and assert the hook still finishes within its timeout. Crash after appending 15 turns of 256 KiB each, before the cursor moves, then rerun with a tiny dedup window and a 1 MiB read budget, so reconciliation spans several pages. Assert no duplicate. Grow the transcript while a killed hook's append is in flight, then rerun. Assert no `IdempotencyConflict` and no duplicate. Leave 100 entries pending after the last hook. Assert `doctor` reports the lag and `capture sync` drains it. Keep zerodb down for a whole new session, then start it. Assert `capture sync` finds and captures that session. Commit a turn, kill the hook before the cursor moves, forget that turn, evict its key with a tiny dedup window, then run `capture sync`. Assert the session is retired and the forgotten text is not written again. Run `capture forget` while a hook is mid-drain, and while a killed hook's append is in flight. Assert no captured turn is readable after it returns, and no hook appends afterwards. Use a 255-byte project directory name with multibyte characters, and assert the generated session names fit. Capture in 2 projects both named `app` in different parents, and assert each SessionStart injects only its own turns and facts. Capture entries of exactly 3 MiB, 3 MiB plus 1 byte, just under 4 MiB, 10 MiB and 100 MiB. Kill the hook after chunk 1 of 3, evict its key with a tiny dedup window, and rerun. Assert each chunk is stored once, in order, that none is skipped, that no append fails with `BatchTooLarge`, and that the 100 MiB entry is marked `truncated`. Kill the hook between the append and the cursor update, then rerun. Run 2 hooks for the same harness session at once, with a tiny dedup window, and assert no duplicate. Kill a hook while its append is in flight, run the next hook at once with the default window, and assert no duplicate. Assert that zerodb being down exits 0 within the timeout.
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
| Keep the token out of harness configs | Pass `ZERODB_TOKEN` in the MCP `env` block | Harness configs get shared and committed. `zerodb mcp` can read the token itself |
| Capture is opt-in | Capture by default | Transcripts hold secrets. Users must choose to store them |
| Objects live in a content-addressed store beside the tape, with refs on the tape | Objects as tape records, or an S3 protocol server | Frame cap, recovery cost and streaming rule out the tape. S3 is a large surface nobody has asked for |
| Hide, not erase, for objects | Reference counting with delete now | Matches I10 and the deferred compaction. Erase needs the same GC compaction will |
| Object puts carry an `idem_key` | Dedup by digest only | A digest can't tell a retry from a new put after a forget. The `idem_key` keeps a lost reply from restoring forgotten content |
| Each harness session captures into its own zerodb session, with a cursor fact, reconciled under a per-session file lock | Rely on `idem_key` only, or share the project session and scan it | The dedup window is bounded (I6). A shared session makes reconciliation scan other sessions' turns, without a bound. A session of its own bounds it to 1 page. Without the lock, 2 overlapping hooks can both append once a key is evicted |
| Object forget hides every ref the tenant holds to the digest, up to the forget's LSN | Hide only 1 ref | 2 puts of the same bytes would leave the object readable through the other ref |
| Capture appends 1 transcript entry per write | Batches of several entries | A batch key covers its whole content, so a transcript that grows during a retry would change the batch. 1 entry per key never changes |
| A hook registers its session in a local file before any network call | Rely on cursor facts in zerodb | A session with zerodb down from the start would have no cursor and could never be found |
| A redacted turn after the cursor retires the capture session | Skip it and continue | Forget removes the position, so continuing could append the forgotten entry again once its key is evicted |
| Per-project keys use the path-hashed project id | The directory name | 2 repos with the same name would overwrite each other's pointer and facts |
| Object file tools work only inside configured roots, after resolving symlinks | Any path the model sends | zerodb runs beside the trusted harness (D79). A model-supplied path could read credentials or overwrite files |
| Entries over 3 MiB are chunked deterministically into 3 MiB turns, up to 22 chunks, and resume at the first missing chunk | Skip them, or chunk at 4 MiB | Skipping stalls the in-order drain behind 1 entry forever. A 4 MiB chunk plus encoding overflows the 4 MiB transaction limit |
| A killed hook's in-flight append relies on the dedup window, with the bound stated | A server-side capture fence | A duplicate needs over 1M writes from 1 tenant within a few-second hook timeout. A fence is a new server mechanism for a case the window already bounds |
| Object directories are created and synced once, before any ref | Create shards on first use | A put that creates a shard would also have to sync its parent. Doing it once removes the case |
| `disconnect` removes only zerodb's entries | Restore the backup | A restore would drop anything the user changed after `connect` |
| No outbound network calls | Fetch link contents, or call remote stores | Keeps zerodb local and predictable. Links stay references |
| Compression only on dogfood evidence | Ship zstd now | #36 chose to measure first. A frame flag is a format commitment |
