# AtticDB Python client

One file, `atticdb_client.py`, standard library only. Copy it into your project.

```python
from atticdb_client import Client, Conflict, NotCommitted, OutcomeUnknown

client = Client("unix:///var/lib/atticdb/atticdb.sock", token)
receipt = client.append("planner", "run-42", [{"role": "user", "content": "hi"}])
window = client.window("planner", "run-42", last=20, max_bytes=65536)
print(client.health())
```

## Facts

```python
put = client.fact_put("repo/payments/retry-rule", "Retry 3 times, then fail.")
client.fact_put("repo/payments/retry-rule", "Retry 5 times.", if_lsn=put["lsn"])
fact = client.fact_get("repo/payments/retry-rule")      # dict, or None
history = client.fact_history("repo/payments/retry-rule")  # newest first
page = client.fact_list("repo/", limit=50)               # pass page["next"] as after=
client.fact_delete("repo/payments/retry-rule")
```

- `fact_put(key, content, meta=None, agent=None, if_lsn=None, idem_key=None, resume=None)` and `fact_delete(key, agent=None, if_lsn=None, idem_key=None, resume=None)` follow the outcome rule below, `resume` included. They return `{txn, lsn, deduplicated, receipt_missing}`.
- `if_lsn` writes only if the key's current LSN matches. `0` means the key has no current value. A mismatch raises `Conflict`, a `NotCommitted`. After an unclear attempt the call raises `OutcomeUnknown`, never `Conflict`.
- `if_lsn` is not part of the retry identity: a retry with the same key and payload returns the original receipt.
- `fact_get(key)`, `fact_history(key, max_bytes=None)` and `fact_list(prefix="", after=None, limit=None, max_bytes=None)` read once, like `window`.

## URL forms

- `http://127.0.0.1:<port>`. Also `localhost` and `[::1]`. Other hosts raise `ValueError`, because the token travels in cleartext.
- `unix:///path` for the server's Unix socket.
- `vsock://<cid>:<port>` from a microVM guest. It needs `socket.AF_VSOCK`. Where that is missing, the constructor raises `ValueError`.

## The outcome rule

- `append`, `fact_put` and `fact_delete` return the receipt when the write committed. `deduplicated` is true when an earlier attempt with the same key wrote it.
- `receipt_missing: True` means the write is durable but the server could not rebuild its receipt. Do not retry it.
- It retries with the same `idem_key` until `retry_deadline` (30 s by default). The deadline bounds connect, send and every read.
- An attempt sends nothing when connect fails or under 2 s are left, the first attempt included. So a `retry_deadline` under 2 s sends nothing and raises `NotCommitted`.
- `NotCommitted` means nothing was written. Calling again is safe.
- `OutcomeUnknown` means the write may be saved. Settle it with the same call plus `resume=error`, for example `client.append(agent, session, turns, resume=error)`, from any `Client`. It reuses the key and stays unknown until a `committed` answer, so it never reports `not_committed`. It cannot write a second copy while the server still remembers the key: same data directory, tenant and request (agent, session and turns, or fact key and payload). The engine forgets keys past its per-tenant and global dedup limits, so settle unknown writes promptly.
- `idem_key=None` makes a new key per call. Any other value, `""` included, is sent as is.

Reads do not retry. Errors and `repr` never show the token.

## Tests

```sh
cargo build -p atticdb --release --features fault-inject
python3 -m pip install -r clients/python/requirements-test.txt
ATTICDB_BIN=target/release/atticdb python3 -m pytest clients/python/tests
```

Sockets live under `target/s`. Set `ATTICDB_TEST_DIR` to a short directory if the checkout path is too deep for `sun_path`.
