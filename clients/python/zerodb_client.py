"""A zerodb client. Standard library only.

Writes follow the outcome rule: a call returns a `committed` receipt, raises
`NotCommitted` when nothing was written, or raises `OutcomeUnknown` when the write may
be saved. Only `committed` replaces `unknown`, as in `crates/zerodb/src/outcome.rs`.
"""

import http.client
import json
import random
import secrets
import socket
import time
import urllib.parse

FIRST_BACKOFF = 0.05
MAX_BACKOFF = 2.0
# Time kept for the request itself: no new attempt starts with less left.
MIN_ATTEMPT = 2.0
RECEIPT = ("deduplicated", "txn", "first_lsn", "last_lsn", "seqs")


class Error(Exception):
    """A request failed. Messages never hold the token."""


class NotCommitted(Error):
    """Nothing was written. Calling again is safe."""


class OutcomeUnknown(Error):
    """The write may be saved. Call again with `idem_key` to settle it without a copy."""


class _Socket(http.client.HTTPConnection):
    def __init__(self, family, address, timeout):
        super().__init__("localhost", timeout=timeout)
        self._family, self._address = family, address

    def connect(self):
        self.sock = socket.socket(self._family, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self._address)


class Client:
    """Talks to `http://127.0.0.1:<port>`, `unix:///path` or `vsock://<cid>:<port>`."""

    def __init__(self, url, token, retry_deadline=30.0):
        if not (token.isascii() and token.isprintable()):
            raise ValueError("token must be printable ASCII")
        self._url, self._token, self.retry_deadline = url, token, retry_deadline
        parts = urllib.parse.urlsplit(url)
        if parts.scheme == "unix" and not parts.netloc and parts.path.startswith("/"):
            self._open = lambda timeout: _Socket(socket.AF_UNIX, parts.path, timeout)
        elif parts.scheme == "vsock":
            if not hasattr(socket, "AF_VSOCK"):
                raise ValueError("vsock:// needs socket.AF_VSOCK, which this platform lacks")
            address = (int(parts.hostname), parts.port)
            self._open = lambda timeout: _Socket(socket.AF_VSOCK, address, timeout)
        elif parts.scheme == "http" and parts.hostname in ("127.0.0.1", "localhost", "::1"):
            # The token travels in cleartext, so http takes loopback only.
            host, port = parts.hostname, parts.port
            self._open = lambda timeout: http.client.HTTPConnection(host, port, timeout)
        else:
            raise ValueError(
                "url must be unix:///path, vsock://<cid>:<port> or "
                "http://<127.0.0.1, localhost or [::1]>:<port>; "
                "http sends the token in cleartext"
            )

    def __repr__(self):
        return f"Client({self._url!r})"

    def append(self, agent, session, turns, idem_key=None):
        """Appends turns as one batch. Returns the receipt, with `receipt_missing` true
        when the write is durable but the server could not rebuild its receipt."""
        key = idem_key or secrets.token_hex(16)
        body = {"agent": agent, "session": session, "idem_key": key, "turns": turns}
        body = json.dumps(body).encode()
        deadline = time.monotonic() + self.retry_deadline
        state, backoff = None, FIRST_BACKOFF
        while True:
            left = deadline - time.monotonic()
            sent, _, reply, detail = self._attempt("POST", "/v1/append", body, left)
            # A failed connect sent nothing. A transport error or a malformed reply is unclear.
            outcome = (reply or {}).get("outcome") if sent else "not_committed"
            if outcome == "committed":
                return {**dict.fromkeys(RECEIPT), **reply, "receipt_missing": "seqs" not in reply}
            unclear = state == "unknown" or outcome != "not_committed"
            state = "unknown" if unclear else "not_committed"
            if (state == "not_committed" and sent) or (
                time.monotonic() + backoff + MIN_ATTEMPT > deadline
            ):
                if state == "unknown":
                    error = OutcomeUnknown(f"unknown: the write may be saved ({detail})")
                else:
                    error = NotCommitted(f"not_committed: nothing was written ({detail})")
                error.idem_key = key
                raise error
            time.sleep(backoff / 2 + random.uniform(0, backoff / 2))
            backoff = min(backoff * 2, MAX_BACKOFF)

    def window(self, agent, session, last=None, since_seq=None, limit=None, max_bytes=None):
        """Reads a session window: pass `last`, or `since_seq` with an optional `limit`."""
        params = {"agent": agent, "session": session, "last": last, "since_seq": since_seq}
        params.update(limit=limit, max_bytes=max_bytes)
        query = urllib.parse.urlencode({k: v for k, v in params.items() if v is not None})
        return self._read(f"/v1/window?{query}", (200,))

    def health(self):
        """Returns `{"status", "instance"}`. A poisoned server answers too, with 503."""
        return self._read("/healthz", (200, 503))

    def _read(self, target, ok):
        _, status, reply, detail = self._attempt("GET", target, None, self.retry_deadline)
        if status not in ok:
            raise Error(f"GET {target.partition('?')[0]} failed ({detail})")
        return reply

    def _attempt(self, method, target, body, timeout):
        """One request on a new connection. Returns (sent, status, reply, detail)."""
        conn = self._open(timeout)
        try:
            try:
                conn.connect()
            except OSError as error:
                return False, None, None, f"connect failed: {error}"
            headers = {"Authorization": f"Bearer {self._token}"}
            if body is not None:
                headers["Content-Type"] = "application/json"
            conn.request(method, target, body, headers)
            response = conn.getresponse()
            status, reply = response.status, json.loads(response.read())
        except (OSError, http.client.HTTPException, ValueError) as error:
            return True, None, None, f"{type(error).__name__}: {error}"
        finally:
            conn.close()
        if not isinstance(reply, dict):
            return True, None, None, "malformed response"
        error = ": ".join(str(reply[k]) for k in ("error", "message") if k in reply)
        return True, status, reply, f"{status} {error}".strip()
