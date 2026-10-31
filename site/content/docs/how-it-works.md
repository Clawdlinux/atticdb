AtticDB keeps 1 append-only tape. Indexes and checkpoints are derived from it.

## The tape

- 1 append-only tape is both WAL and store. Indexes and checkpoints are derived and rebuild from it (D1, I8).
- A zero-filled spare segment is kept ready for the next roll. On ext4 an `fdatasync` is then usually 1 device round trip (D78).
- Group commit never waits on a timer. The writer syncs whatever is queued when it wakes (D80).
- Frames carry location-seeded CRC32C and footers bind whole transactions. Recovery cuts only an unproven tail (D64, D65, D8).
- Damage inside the last synced group has no later proof, so it can still be cut (I1).
- Windows cost O(returned). Paged arrays, held to a memory budget, map seqs to LSNs and LSNs to offsets (D2, D66, I5).
- Every write carries an `idem_key`, so a retry in the dedup window can't commit twice (D67, I6).
- A client that can't tell whether a write landed reports `unknown`, never `not_committed` (D75). See [the outcome rule](docs:http-api#the-outcome-rule).
- Below a free-space floor the engine goes read-only. Writes are rejected before anything is written. Reads keep working (I14).

D-ids are in the [decision log](https://github.com/Clawdlinux/atticdb/blob/main/docs/design/0002-decision-log.md). I-ids are invariants in [0003](https://github.com/Clawdlinux/atticdb/blob/main/docs/design/0003-slim-v0.1.md), which defines the current scope.

CI runs both SimDisk crash simulations on 2,000 seeds. Anyone with write access to the data directory can edit history. Nothing detects it.

## Sandboxed agents

The harness is the trusted loop that calls the model and starts sandboxes. By default it holds memory (D79). It reads the window and facts, builds the prompt and appends results.

Code in a microVM or container gets no token and no socket, because a token grants a whole tenant. Direct access ([sandboxes.md](https://github.com/Clawdlinux/atticdb/blob/main/docs/deploy/sandboxes.md)) is for code you trust with that tenant. Or for code that has its own tenant.

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md)
