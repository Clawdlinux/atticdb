# zerodb Python client

One file, `zerodb_client.py`, standard library only. Copy it into your project.

```python
from zerodb_client import Client, NotCommitted, OutcomeUnknown

client = Client("unix:///var/lib/zerodb/zerodb.sock", token)
receipt = client.append("planner", "run-42", [{"role": "user", "content": "hi"}])
window = client.window("planner", "run-42", last=20, max_bytes=65536)
print(client.health())
```

## URL forms

- `http://127.0.0.1:<port>`. Also `localhost` and `[::1]`. Other hosts raise `ValueError`, because the token travels in cleartext.
- `unix:///path` for the server's Unix socket.
- `vsock://<cid>:<port>` from a microVM guest. It needs `socket.AF_VSOCK`. Where that is missing, the constructor raises `ValueError`.

## The outcome rule

- `append` returns the receipt when the write committed. `deduplicated` is true when an earlier attempt with the same key wrote it.
- `receipt_missing: True` means the write is durable but the server could not rebuild its receipt. Do not retry it.
- It retries with the same `idem_key` until `retry_deadline` (30 s by default). A failed connect sent nothing.
- `NotCommitted` means nothing was written. Calling again is safe.
- `OutcomeUnknown` means the write may be saved. Call again with `error.idem_key` to settle it without a second copy.

Reads do not retry. Errors and `repr` never show the token.

## Tests

```sh
cargo build -p zerodb --release --features fault-inject
python3 -m pip install -r clients/python/requirements-test.txt
ZERODB_BIN=target/release/zerodb python3 -m pytest clients/python/tests
```

Sockets live under `target/s`. Set `ZERODB_TEST_DIR` to a short directory if the checkout path is too deep for `sun_path`.
