import os
import socket
import time
from concurrent.futures import ThreadPoolExecutor

import pytest
from helpers import TOKEN, send_without_reading, trickle, turns, wait_for
from zerodb_client import Client, NotCommitted, OutcomeUnknown


def contents(client, session):
    return [turn["content"] for turn in client.window("a", session, last=100)["turns"]]


@pytest.mark.parametrize("listener", ["http", "unix"])
def test_append_retry_and_window(server, listener):
    client = Client(server.url[listener], TOKEN)
    assert client.health() == {"status": "ok", "instance": server.instance}
    batch = turns("one", "two" * 10, "three" * 10)
    first = client.append("a", "s", batch, idem_key="k1")
    assert first["outcome"] == "committed" and first["deduplicated"] is False
    assert first["receipt_missing"] is False and len(first["seqs"]) == 3
    again = client.append("a", "s", batch, idem_key="k1")
    assert again["deduplicated"] is True
    assert [again[k] for k in ("txn", "seqs")] == [first[k] for k in ("txn", "seqs")]
    with pytest.raises(NotCommitted, match="idempotency_conflict") as conflict:
        client.append("a", "s", turns("changed"), idem_key="k1")
    assert TOKEN not in str(conflict.value) and TOKEN not in repr(client)

    window = client.window("a", "s", last=10)
    assert [t["seq"] for t in window["turns"]] == first["seqs"] and not window["truncated"]
    for max_bytes, expect, truncated in [
        (1 << 20, ["one", "two" * 10, "three" * 10], False),
        (80, ["two" * 10, "three" * 10], True),
        (10, [], True),
    ]:
        window = client.window("a", "s", last=10, max_bytes=max_bytes)
        assert [t["content"] for t in window["turns"]] == expect, max_bytes
        assert window["truncated"] is truncated, max_bytes
    # Only None makes a key. An empty one reaches the server, which refuses it.
    with pytest.raises(NotCommitted, match="idem_key must be 1 to 128 bytes"):
        client.append("a", "empty-key", turns("x"), idem_key="")
    assert contents(client, "empty-key") == []
    since = client.window("a", "s", since_seq=first["seqs"][1], limit=1)["turns"]
    assert [t["seq"] for t in since] == first["seqs"][1:2]
    for url in ("http://10.0.0.1:80", "http://127.0.0.1.evil:1", "https://127.0.0.1:1"):
        with pytest.raises(ValueError, match="cleartext"):
            Client(url, TOKEN)
    # vsock CIDs and ports are 32 bits, past TCP's 65535.
    for url in ("vsock://2:4294967296", "vsock://x:1", "vsock://2"):
        with pytest.raises(ValueError, match="each 0 to 2"):
            Client(url, TOKEN)
    if hasattr(socket, "AF_VSOCK"):
        Client("vsock://4294967295:4294967295", TOKEN)

    # JSON escapes each NUL to 6 bytes, so a 2 MiB window is a 12 MiB response.
    nul = "\0" * (1 << 20)
    for _ in range(2):
        client.append("a", "nul", turns(nul))
    window = client.window("a", "nul", last=2, max_bytes=2 << 20)
    assert [turn["content"] for turn in window["turns"]] == [nul, nul]
    # The 2 s reserve holds for the first attempt too: nothing is sent.
    with pytest.raises(NotCommitted, match="less than 2 s left"):
        Client(server.url[listener], TOKEN, retry_deadline=1).append("a", "late", turns("x"))
    assert contents(client, "late") == []


def test_retry_survives_server_restart(server):
    # The turn is durable but the reply is never read when the server dies.
    client = Client(server.url["unix"], TOKEN)
    body = {"agent": "a", "session": "s", "idem_key": "fixed", "turns": turns("once")}
    with send_without_reading(server.socket, body):
        wait_for(lambda: contents(client, "s") == ["once"], "the turn in the window")
        server.kill()
    # Every connect fails, so nothing was sent: not_committed, and nothing appears later.
    with pytest.raises(NotCommitted, match="connect failed"):
        Client(server.url["unix"], TOKEN, retry_deadline=2.5).append("a", "s", turns("never"))

    with ThreadPoolExecutor(1) as pool:
        call = pool.submit(client.append, "a", "s", turns("once"), "fixed")
        server.start()
        receipt = call.result()
    assert receipt["deduplicated"] is True and len(receipt["seqs"]) == 1
    assert contents(client, "s") == ["once"]


@pytest.mark.parametrize("restart", [True, False], ids=["restart after 1 s", "no restart"])
def test_unknown_outcome_never_downgrades(server, restart):
    # The first answer is unknown, retries get not_committed from the poisoned server.
    client = Client(server.url["unix"], TOKEN, retry_deadline=30 if restart else 3)
    open(os.path.join(server.data, "FAULT-SYNC-ONCE"), "w").close()
    with ThreadPoolExecutor(1) as pool:
        call = pool.submit(client.append, "a", "s", turns("maybe"))
        wait_for(lambda: "POST /v1/append 503" in server.log(), "a not_committed retry")
        assert "POST /v1/append 500" in server.log(), "first answer unknown"
        if restart:
            time.sleep(1)
            server.kill()
            server.start()
            receipt = call.result()
        else:
            with pytest.raises(OutcomeUnknown, match="unknown: the write may be saved") as e:
                call.result()
            assert len(e.value.idem_key) == 32 and TOKEN not in str(e.value)
            # Resuming keeps the uncertainty: the poisoned server's not_committed cannot
            # clear it. After a restart the same key settles it, from any client.
            with pytest.raises(OutcomeUnknown):
                client.append("a", "s", turns("maybe"), resume=e.value)
            server.kill()
            server.start()
            resumed = Client(server.url["unix"], TOKEN)
            receipt = resumed.append("a", "s", turns("maybe"), resume=e.value)
    assert receipt["outcome"] == "committed" and receipt["deduplicated"] is True
    assert contents(client, "s") == ["maybe"]


def test_deadline_bounds_a_slow_response(server):
    # The reply trickles in for 10 s. The call gives up at its deadline with unknown.
    path = os.path.join(server.dir, "slow")
    with trickle(path, 10):
        started = time.monotonic()
        with pytest.raises(OutcomeUnknown, match="unknown: the write may be saved"):
            Client(f"unix://{path}", TOKEN, retry_deadline=2.5).append("a", "s", turns("x"))
        assert time.monotonic() - started < 4


def test_connect_budget_spans_every_address(monkeypatch):
    # localhost resolves to two addresses that never answer. Together they get one
    # connect budget: the deadline minus the 2 s reserve, 0.5 s here.
    waits = []

    class Blackhole:
        def __init__(self, *args):
            self.timeout = None

        def settimeout(self, timeout):
            self.timeout = timeout

        def connect(self, address):
            waits.append(self.timeout)
            time.sleep(self.timeout)
            raise TimeoutError("timed out")

        def close(self):
            pass

    addresses = [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (f"192.0.2.{i}", 1)) for i in (1, 2)]
    monkeypatch.setattr(socket, "getaddrinfo", lambda *args, **kwargs: addresses)
    monkeypatch.setattr(socket, "socket", Blackhole)
    started = time.monotonic()
    with pytest.raises(NotCommitted, match="connect failed"):
        Client("http://localhost:1", TOKEN, retry_deadline=2.5).append("a", "s", turns("x"))
    assert time.monotonic() - started < 0.8 and sum(waits) <= 0.5, waits
