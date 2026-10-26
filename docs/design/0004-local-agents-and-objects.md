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
- **A-sessions:** default agent is the harness name. Default session is the project directory's name plus a short hash of its path. MCP tools still take explicit names.
- **A-readme:** an Install section that says where the data directory should live:
  - Tested: local NVMe and network block SSD on Linux ext4; macOS APFS for development.
  - Works but untested: SATA SSD and HDD. Each ack waits for a device sync, which is milliseconds on a spinning disk.
  - Don't use: NFS, SMB, FUSE mounts, or cloud-synced folders. Their `fsync` and `flock` guarantees vary, and sync clients copy files mid-write.

### Track B: automatic capture (Claude Code)

`zerodb connect claude-code --capture` also installs hooks.
- **SessionStart:** injects the project session's last N turns and the facts under `project/<name>/` as extra context, within a byte budget.
- **UserPromptSubmit and Stop:** append the user prompt and the assistant reply. Progress is tracked per harness session, not per project, because several harness sessions and manual MCP appends can share a project session.
  - Each captured turn's `meta` records its harness session id and its position in `transcript_path`.
  - A fact `capture/<harness session id>` holds that session's cursor: the last captured position and the zerodb seq of its turn. Facts are never evicted, so the cursor survives the bounded dedup window (I6) and restarts.
  - A hook reads the cursor fact, then reads the project session's turns after that seq and keeps those whose `meta` names this harness session. The highest position among them wins. This covers a crash between appending turns and updating the fact. Facts can't share a batch with turns (D76), so the 2 writes can't be atomic. Then the hook appends the entries past the cursor, and moves the cursor with a conditional put (`if_lsn`).
  - The whole cycle runs under an exclusive `flock` on `<home>/capture/<harness session id>.lock`: read the cursor, reconcile, append, move the cursor. So 2 hooks for the same harness session never overlap. Hooks run on the harness's machine, the same one as zerodb's home. A hook that can't get the lock within its timeout exits 0. Nothing is lost, because the next hook resumes from the cursor.
  - The `idem_key` from the harness session id and position, and the conditional put (`if_lsn`), stay as a second guard.
- **PreCompact:** appends anything not yet captured before the harness compacts its context.

Each hook is a short `zerodb` subcommand with a hard timeout. If zerodb is down, the hook exits 0 and the session goes on without memory. Capture never blocks the agent.
Other harnesses use the MCP tools only, until they have equivalent hooks.

### Track C: links and objects

- **C-links:** a turn or a fact can carry `refs`: a list of `{url, title?, mime?}` in `meta`. zerodb stores them and returns them. It never fetches them. No format change.
- **C-objects:** a local, content-addressed object store in the data directory.
  - **Layout:** the first time a data directory needs it, the server creates `objects/`, `objects/tmp/`, `objects/sha256/` and all 256 `objects/sha256/<2 hex>/` shards. It `fsync`s each new directory's parent, up to the data directory, before writing any ref. No put ever creates a directory. So a ref can't point into a directory whose entry was lost.
  - **Write:** `PUT /v1/objects` streams a body with a `Content-Type`. The server writes `objects/tmp/<random>`, hashes it while writing, then `fsync`s. It renames it to `objects/sha256/<2 hex>/<62 hex>` and `fsync`s that shard directory. Then it appends an `ObjectRef {tenant, digest, bytes, mime}` record to the tape. The ack comes after that record is durable. The reply is `{"object":"sha256:<hex>","bytes":N}`.
  - **Read:** `GET /v1/objects/sha256:<hex>` streams the bytes, only if the caller's tenant holds a live ref. Turns and facts point at objects through `refs`, using an `object:` URL.
  - **Retries:** a put carries an `idem_key`, like every other write (I6). Within the dedup window, a retry returns the original result and writes nothing, even if the ref was forgotten in between. So a lost reply can't bring back a forgotten object. After eviction a retry writes a new ref, as I6 allows for every write. The digest only dedups the file on disk.
  - **Limits:** 1 GiB per object by default, configurable. Objects count against the free-space floor, which is checked before writing.
  - **Forget (I10):** forgetting a ref hides the object from that tenant. Bytes stay on disk until compaction exists, like turns today. Tenants never share a ref. Identical bytes stored by 2 tenants share 1 file, but each tenant's ref is separate (Q5).
  - **Crashes:** a crash after the rename but before the ref leaves an unreferenced file. It's invisible, and a later sweep removes it. A ref can never point at a missing file, because the file is synced first.
- **C-mcp:**
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
- **Capture hooks:** feed recorded hook JSON into the subcommands. Assert that no turn is written twice across repeated runs, including after the dedup window has evicted their keys (a test with a tiny window). Interleave 2 harness sessions and a manual MCP append in 1 project session, and assert each transcript is captured exactly once. Kill the hook between the append and the cursor update, then rerun. Run 2 hooks for the same harness session at once, with a tiny dedup window, and assert no duplicate. Assert that zerodb being down exits 0 within the timeout.
- **Objects:**
  - a sim case for a crash at every step of the put, including the first-use directory setup;
  - a power-loss case in `tests/crash/power_loss.py`, starting from a data directory with no `objects/` yet;
  - forget hiding the object from every read path;
  - free-space floor rejection before any bytes are written.

## Decision log (proposed)

| Decision | Alternatives | Why |
|---|---|---|
| Register through the harness's own command where it exists, else merge its JSON with a backup | Only print instructions, or always edit files | Its own command tracks format changes. Printing leaves the 5-step problem |
| Keep the token out of harness configs | Pass `ZERODB_TOKEN` in the MCP `env` block | Harness configs get shared and committed. `zerodb mcp` can read the token itself |
| Capture is opt-in | Capture by default | Transcripts hold secrets. Users must choose to store them |
| Objects live in a content-addressed store beside the tape, with refs on the tape | Objects as tape records, or an S3 protocol server | Frame cap, recovery cost and streaming rule out the tape. S3 is a large surface nobody has asked for |
| Hide, not erase, for objects | Reference counting with delete now | Matches I10 and the deferred compaction. Erase needs the same GC compaction will |
| Object puts carry an `idem_key` | Dedup by digest only | A digest can't tell a retry from a new put after a forget. The `idem_key` keeps a lost reply from restoring forgotten content |
| Capture keeps a cursor fact per harness session, reconciled with the turns after it, under a per-session file lock | Rely on `idem_key` only, or use the project session's newest turn | The dedup window is bounded (I6). Several harness sessions and manual appends share a project session, so its newest turn isn't this transcript's cursor. Without the lock, 2 overlapping hooks can both append once a key is evicted |
| Object directories are created and synced once, before any ref | Create shards on first use | A put that creates a shard would also have to sync its parent. Doing it once removes the case |
| `disconnect` removes only zerodb's entries | Restore the backup | A restore would drop anything the user changed after `connect` |
| No outbound network calls | Fetch link contents, or call remote stores | Keeps zerodb local and predictable. Links stay references |
| Compression only on dogfood evidence | Ship zstd now | #36 chose to measure first. A frame flag is a format commitment |
