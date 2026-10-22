"""A zerodb client. Standard library only.

Writes follow the outcome rule: a call returns a `committed` receipt, raises
`NotCommitted` when nothing was written, or raises `OutcomeUnknown` when the write may
be saved. Only `committed` replaces `unknown`, as in `crates/zerodb/src/outcome.rs`.
"""

import ipaddress
import json
import random
import secrets
import socket
import time
import urllib.parse

FIRST_BACKOFF = 0.05
MAX_BACKOFF = 2.0
# Time kept for the request itself: no attempt connects or sends with less left.
MIN_ATTEMPT = 2.0
# A page holds up to 8 MiB of keys, agents, content and meta, and JSON escapes a byte
# to at most 6. Fixed fields of up to 4,096 items and a 6 KiB `next` fit in 1 MiB.
MAX_RESPONSE = 6 * (8 << 20) + (1 << 20)
# Receipt fields per write. The last one is missing when the server had no receipt.
RECEIPT = ("deduplicated", "txn", "first_lsn", "last_lsn", "seqs")
FACT_RECEIPT = ("deduplicated", "lsn", "txn")


class Error(Exception):
    """A request failed. Messages never hold the token."""


class NotCommitted(Error):
    """Nothing was written. Calling again is safe."""


class Conflict(NotCommitted):
    """A fact write's `if_lsn` did not match the key's current LSN. Nothing was written."""


class OutcomeUnknown(Error):
    """The write may be saved. Call again with `resume=` this error to settle it."""


def _query(**params):
    return urllib.parse.urlencode({k: v for k, v in params.items() if v is not None})


def _left(deadline):
    left = deadline - time.monotonic()
    if left <= 0:
        raise TimeoutError("deadline passed")
    return left


def _connect(family, address, deadline):
    sock = socket.socket(family, socket.SOCK_STREAM)
    try:
        sock.settimeout(_left(deadline))
        sock.connect(address)
    except BaseException:
        sock.close()
        raise
    return sock


def _connect_tcp(host, port, deadline):
    """Tries each address in turn, each with only what is left of one connect budget.
    Every resolved address must be loopback: the token travels in cleartext."""
    error = OSError(f"{host} has no address")
    infos = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    for *_, address in infos:
        if not ipaddress.ip_address(address[0]).is_loopback:
            raise OSError(f"{address[0]} is not loopback; http sends the token in cleartext")
    for family, _, _, _, address in infos:
        try:
            return _connect(family, address, deadline)
        except OSError as failed:
            error = failed
    raise error


def _recv(sock, deadline):
    """One read, bounded by what is left of the deadline."""
    sock.settimeout(_left(deadline))
    chunk = sock.recv(1 << 16)
    if not chunk:
        raise ConnectionError("connection closed before the whole response")
    return chunk


def _u32(text):
    if not (text.isascii() and text.isdigit() and int(text) < 1 << 32):
        raise ValueError("url must be vsock://<cid>:<port>, each 0 to 2^32 - 1")
    return int(text)


class Client:
    """Talks to `http://127.0.0.1:<port>`, `unix:///path` or `vsock://<cid>:<port>`."""

    def __init__(self, url, token, retry_deadline=30.0):
        if not (token.isascii() and token.isprintable()):
            raise ValueError("token must be printable ASCII")
        self._url, self._token, self.retry_deadline = url, token, retry_deadline
        parts = urllib.parse.urlsplit(url)
        if parts.scheme == "unix" and not parts.netloc and parts.path.startswith("/"):
            self._connect = lambda deadline: _connect(socket.AF_UNIX, parts.path, deadline)
        elif parts.scheme == "vsock":
            # vsock ports are 32 bits, so urlsplit's TCP port parsing does not apply.
            cid, _, port = parts.netloc.partition(":")
            address = (_u32(cid), _u32(port))
            if not hasattr(socket, "AF_VSOCK"):
                raise ValueError("vsock:// needs socket.AF_VSOCK, which this platform lacks")
            self._connect = lambda deadline: _connect(socket.AF_VSOCK, address, deadline)
        elif parts.scheme == "http" and parts.hostname in ("127.0.0.1", "localhost", "::1"):
            # The token travels in cleartext, so http takes loopback only.
            host, port = parts.hostname, parts.port or 80
            self._connect = lambda deadline: _connect_tcp(host, port, deadline)
        else:
            raise ValueError(
                "url must be unix:///path, vsock://<cid>:<port> or "
                "http://<127.0.0.1, localhost or [::1]>:<port>; "
                "http sends the token in cleartext"
            )

    def __repr__(self):
        return f"Client({self._url!r})"

    def append(self, agent, session, turns, idem_key=None, resume=None):
        """Appends turns as one batch. Returns the receipt, with `receipt_missing` true
        when the write is durable but the server could not rebuild its receipt.

        `resume` takes an `OutcomeUnknown` from an earlier call with the same turns. Its
        key replaces `idem_key`, and the call starts at unknown: only `committed`
        settles it, never `not_committed`."""
        body = {"agent": agent, "session": session, "turns": turns}
        return self._write("/v1/append", body, idem_key, resume, RECEIPT)

    def fact_put(self, key, content, meta=None, agent=None, if_lsn=None, idem_key=None,
                 resume=None):
        """Stores `content` as the key's current value. With `if_lsn` the write happens
        only if the key's current LSN matches, 0 meaning no current value. A mismatch
        raises `Conflict`. Retries and `resume` work as for `append`."""
        body = {"key": key, "content": content, "meta": meta, "agent": agent, "if_lsn": if_lsn}
        return self._write("/v1/facts/put", body, idem_key, resume, FACT_RECEIPT)

    def fact_delete(self, key, agent=None, if_lsn=None, idem_key=None, resume=None):
        """Writes a tombstone: the key then has no current value."""
        body = {"key": key, "agent": agent, "if_lsn": if_lsn}
        return self._write("/v1/facts/delete", body, idem_key, resume, FACT_RECEIPT)

    def _write(self, target, body, idem_key, resume, receipt):
        key = secrets.token_hex(16) if idem_key is None else idem_key
        key = resume.idem_key if resume else key
        body = {**{k: v for k, v in body.items() if v is not None}, "idem_key": key}
        body = json.dumps(body).encode()
        deadline = time.monotonic() + self.retry_deadline
        state, backoff, reasons = "unknown" if resume else None, FIRST_BACKOFF, []
        while True:
            sent, _, reply, detail = self._attempt("POST", target, body, deadline)
            # Every distinct reason, in order: the last alone depends on timing.
            reasons += [] if detail in reasons else [detail]
            detail = "; ".join(reasons)
            # A failed connect sent nothing. A transport error or a malformed reply is unclear.
            outcome = (reply or {}).get("outcome") if sent else "not_committed"
            if outcome == "committed":
                missing = receipt[-1] not in reply
                return {**dict.fromkeys(receipt), **reply, "receipt_missing": missing}
            unclear = state == "unknown" or outcome != "not_committed"
            state = "unknown" if unclear else "not_committed"
            if (state == "not_committed" and sent) or (
                time.monotonic() + backoff + MIN_ATTEMPT > deadline
            ):
                if state == "unknown":
                    error = OutcomeUnknown(f"unknown: the write may be saved ({detail})")
                else:
                    kind = Conflict if reply and reply.get("error") == "conflict" else NotCommitted
                    error = kind(f"not_committed: nothing was written ({detail})")
                error.idem_key = key
                raise error
            time.sleep(backoff / 2 + random.uniform(0, backoff / 2))
            backoff = min(backoff * 2, MAX_BACKOFF)

    def window(self, agent, session, last=None, since_seq=None, limit=None, max_bytes=None):
        """Reads a session window: pass `last`, or `since_seq` with an optional `limit`."""
        query = _query(agent=agent, session=session, last=last, since_seq=since_seq,
                       limit=limit, max_bytes=max_bytes)
        return self._read(f"/v1/window?{query}", (200,))

    def fact_get(self, key):
        """Returns the key's current value, or None when it has none."""
        return self._read(f"/v1/facts/get?{_query(key=key)}", (200,))["fact"]

    def fact_history(self, key, max_bytes=None):
        """Returns `{"revisions", "truncated"}`, newest first, tombstones included."""
        return self._read(f"/v1/facts/history?{_query(key=key, max_bytes=max_bytes)}", (200,))

    def fact_list(self, prefix="", after=None, limit=None, max_bytes=None):
        """Returns `{"facts", "next"}` in key order. Pass `next` as `after` until it is None."""
        query = _query(prefix=prefix, after=after, limit=limit, max_bytes=max_bytes)
        return self._read(f"/v1/facts/list?{query}", (200,))

    def health(self):
        """Returns `{"status", "instance"}`. A poisoned server answers too, with 503."""
        return self._read("/healthz", (200, 503))

    def _read(self, target, ok):
        deadline = time.monotonic() + self.retry_deadline
        _, status, reply, detail = self._attempt("GET", target, None, deadline)
        if status not in ok:
            raise Error(f"GET {target.partition('?')[0]} failed ({detail})")
        return reply

    def _attempt(self, method, target, body, deadline):
        """One request on a new connection, all by `deadline`. Nothing is sent unless
        2 s are left before connecting and before sending. Returns
        (sent, status, reply, detail)."""
        late = False, None, None, "less than 2 s left before the deadline"
        if time.monotonic() > deadline - MIN_ATTEMPT:
            return late
        try:
            sock = self._connect(deadline - MIN_ATTEMPT)
        except OSError as error:
            return False, None, None, f"connect failed: {error}"
        with sock:
            if time.monotonic() > deadline - MIN_ATTEMPT:
                return late
            try:
                status, reply = self._exchange(sock, method, target, body, deadline)
            except (OSError, ValueError, IndexError) as error:
                return True, None, None, f"{type(error).__name__}: {error}"
        if not isinstance(reply, dict):
            return True, None, None, "malformed response"
        error = ": ".join(str(reply[k]) for k in ("error", "message") if k in reply)
        return True, status, reply, f"{status} {error}".strip()

    def _exchange(self, sock, method, target, body, deadline):
        """Sends one request and reads one `Content-Length` response by `deadline`."""
        head = f"{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
        head += f"Authorization: Bearer {self._token}\r\n"
        if body is not None:
            head += f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
        sock.settimeout(_left(deadline))
        sock.sendall(head.encode() + b"\r\n" + (body or b""))
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 1 << 16:
            data += _recv(sock, deadline)
        head, _, rest = data.partition(b"\r\n\r\n")
        lines = head.decode("latin-1").split("\r\n")
        fields = (line.partition(":") for line in lines[1:])
        lengths = [int(v) for k, _, v in fields if k.strip().lower() == "content-length"]
        if len(lengths) != 1 or not 0 <= lengths[0] <= MAX_RESPONSE:
            raise ValueError("response without one valid Content-Length")
        chunks, size = [rest], len(rest)
        while size < lengths[0]:
            chunks.append(_recv(sock, deadline))
            size += len(chunks[-1])
        return int(lines[0].split(" ", 2)[1]), json.loads(b"".join(chunks)[: lengths[0]])
