import os
import time
from concurrent.futures import ThreadPoolExecutor

import pytest
from helpers import TOKEN, send_without_reading, turns, wait_for
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
    since = client.window("a", "s", since_seq=first["seqs"][1], limit=1)["turns"]
    assert [t["seq"] for t in since] == first["seqs"][1:2]
    for url in ("http://10.0.0.1:80", "http://127.0.0.1.evil:1", "https://127.0.0.1:1"):
        with pytest.raises(ValueError, match="cleartext"):
            Client(url, TOKEN)


def test_retry_survives_server_restart(server):
    body = {"agent": "a", "session": "s", "idem_key": "fixed", "turns": turns("once")}
    with send_without_reading(server.socket, body):
        server.kill()
    # Every connect fails, so nothing was sent: not_committed, and nothing appears later.
    with pytest.raises(NotCommitted, match="connect failed"):
        Client(server.url["unix"], TOKEN, retry_deadline=2.5).append("a", "s", turns("never"))

    client = Client(server.url["unix"], TOKEN)
    with ThreadPoolExecutor(1) as pool:
        call = pool.submit(client.append, "a", "s", turns("once"), "fixed")
        server.start()
        receipt = call.result()
    assert receipt["outcome"] == "committed" and len(receipt["seqs"]) == 1
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
        if not restart:
            with pytest.raises(OutcomeUnknown, match="unknown: the write may be saved") as e:
                call.result()
            assert len(e.value.idem_key) == 32 and TOKEN not in str(e.value)
            return
        time.sleep(1)
        server.kill()
        server.start()
        receipt = call.result()
    assert receipt["outcome"] == "committed" and receipt["deduplicated"] is True
    assert contents(client, "s") == ["maybe"]
