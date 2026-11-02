#!/usr/bin/env python3
"""Head-to-head reads on real agent transcripts: AtticDB, SQLite and Redis.

Usage: python3 bench/reads.py --atticdb <binary> [--target-mib 512] [--refs 20000]
       [--redis redis-server] [--home ~] [--out <json>] [--keep]

Each system holds the same turns, from realload.py's corpus, and the same file-ref facts.
Then the same reads, from the same seed, are timed from this process:

- window: the newest 20 turns of a random session.
- fact: 1 value by key.
- search: 1 or 2 words, newest 10 turns holding all of them, with their content.

How each system holds it:
- AtticDB: `atticdb serve` with `compression: auto`, HTTP over a Unix socket. The word index
  has caught up before reads start.
- SQLite: in this process, so no socket or JSON. `turns(id, session, seq, content)` indexed
  on `(session, seq)`, `facts(key PRIMARY KEY)`, and an FTS5 trigram table over turn content
  for substring search, as AtticDB matches. WAL mode.
- Redis: `redis-server` on a Unix socket, persistence off. 1 list per session (RPUSH) and
  1 string per fact. It has no text search, so it skips that read.

Search semantics differ a little. AtticDB matches singulars and Unicode case. FTS5's
trigram tokenizer folds case its own way and needs 3 characters, so words are 4 or more.
Transcript text stays in a 0700 temp dir that is deleted at exit. Output holds numbers only.
"""

import argparse
import glob
import json
import os
import random
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import realload as rl  # noqa: E402

READS = 2000
SEARCHES = 200


class Resp:
    """The few RESP commands this needs, over a Unix socket."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.buf = b""

    def send(self, *args):
        out = [b"*%d\r\n" % len(args)]
        for arg in args:
            arg = arg if isinstance(arg, bytes) else str(arg).encode()
            out.append(b"$%d\r\n%s\r\n" % (len(arg), arg))
        self.sock.sendall(b"".join(out))

    def _line(self):
        while b"\r\n" not in self.buf:
            self._fill()
        line, self.buf = self.buf.split(b"\r\n", 1)
        return line

    def _fill(self):
        chunk = self.sock.recv(1 << 20)
        if not chunk:
            raise OSError("redis closed the connection")
        self.buf += chunk

    def reply(self):
        line = self._line()
        kind, rest = line[:1], line[1:]
        if kind in (b"+", b":"):
            return rest
        if kind == b"-":
            raise RuntimeError(rest.decode())
        if kind == b"$":
            n = int(rest)
            if n < 0:
                return None
            while len(self.buf) < n + 2:
                self._fill()
            value, self.buf = self.buf[:n], self.buf[n + 2:]
            return value
        if kind == b"*":
            return [self.reply() for _ in range(int(rest))]
        raise RuntimeError(f"unexpected reply {line[:20]!r}")

    def call(self, *args):
        self.send(*args)
        return self.reply()


def timed(fn):
    started = time.perf_counter()
    value = fn()
    return time.perf_counter() - started, value


def stats(prefix, times, metrics):
    rl.ms_stats(prefix, times, metrics)


def dir_mib(path):
    return rl.disk_usage(path)[0] / rl.MIB


def load_attic(args, root, corpus, plan, names, refs, metrics):
    server = rl.Server(args.atticdb, root, "auto")
    client = rl.Client(f"unix://{server.socket}", server.token)
    server.start()
    server.wait_answer(client.health)
    fill_args = SimpleNamespace(writers=8, batch=16)
    took, _ = timed(lambda: rl.fill(fill_args, client, corpus, plan, names, {}))
    for key, size, modified in refs:
        client.fact_put(key, rl.REF_CONTENT, meta={"file": {"bytes": size}}, agent=rl.AGENT)
    metrics["attic_load_s"] = took
    metrics["attic_index_wait_s"] = server.wait_indexed() or 0.0
    metrics["attic_disk_mib"] = dir_mib(server.data)
    metrics["attic_rss_mib"] = server.rss_mib()
    return server, client


def load_sqlite(root, corpus, plan, names, refs, metrics):
    path = os.path.join(root, "sqlite.db")
    db = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=NORMAL")
    db.executescript("""
        CREATE TABLE turns(id INTEGER PRIMARY KEY, session TEXT, seq INTEGER, content TEXT);
        CREATE INDEX turns_session ON turns(session, seq);
        CREATE TABLE facts(key TEXT PRIMARY KEY, content TEXT);
        CREATE VIRTUAL TABLE turns_fts USING fts5(
            content, content='turns', content_rowid='id', tokenize='trigram');
    """)
    started = time.perf_counter()
    db.execute("BEGIN")
    for name, (index, count) in zip(names, plan):
        rows = ((name, seq, turn[1]) for seq, turn in enumerate(corpus.sessions[index][:count], 1))
        db.executemany("INSERT INTO turns(session, seq, content) VALUES (?, ?, ?)", rows)
    db.executemany("INSERT INTO facts VALUES (?, ?)", ((k, rl.REF_CONTENT) for k, _, _ in refs))
    db.execute("INSERT INTO turns_fts(turns_fts) VALUES ('rebuild')")
    db.execute("COMMIT")
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    metrics["sqlite_load_s"] = time.perf_counter() - started
    metrics["sqlite_disk_mib"] = sum(
        os.path.getsize(p) for p in (path, path + "-wal") if os.path.exists(p)) / rl.MIB
    return db


def load_redis(args, root, corpus, plan, names, refs, metrics):
    data, sock = os.path.join(root, "redis"), os.path.join(root, "redis.sock")
    os.mkdir(data, 0o700)
    process = subprocess.Popen(
        [args.redis, "--port", "0", "--unixsocket", sock, "--unixsocketperm", "700",
         "--save", "", "--appendonly", "no", "--dir", data],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True)
    for _ in range(500):
        if os.path.exists(sock):
            break
        time.sleep(0.01)
    redis = Resp(sock)
    started, sent = time.perf_counter(), 0
    for name, (index, count) in zip(names, plan):
        for turn in corpus.sessions[index][:count]:
            redis.send("RPUSH", name, turn[1].encode("utf-8", "surrogatepass"))
            sent += 1
        for _ in range(sent):
            redis.reply()
        sent = 0
    for key, _, _ in refs:
        redis.send("SET", key, rl.REF_CONTENT)
    for _ in refs:
        redis.reply()
    metrics["redis_load_s"] = time.perf_counter() - started
    info = redis.call("INFO", "memory").decode()
    used = next(line for line in info.splitlines() if line.startswith("used_memory:"))
    metrics["redis_memory_mib"] = int(used.split(":")[1]) / rl.MIB
    return process, redis


def queries(rng, corpus, plan):
    """1-word and 2-word queries. A pair comes from 1 turn, so it usually has hits."""
    out = []
    while len(out) < SEARCHES:
        index, count = rng.choice(plan)
        content = corpus.sessions[index][rng.randrange(count)][1]
        start = rng.randrange(max(1, len(content) - 4096))
        found = rl.WORD.findall(content[start:start + 4096])
        if len(found) < 2:
            continue
        words = rng.sample(found, 1 + len(out) % 2)
        out.append(" ".join(word.lower() for word in words))
    return out


def run(args, root, metrics):
    rng = random.Random(args.seed)
    target = args.target_mib * rl.MIB
    paths = sorted({p for g in rl.DEFAULT_SOURCES for p in glob.glob(os.path.expanduser(g))})
    random.Random(args.seed).shuffle(paths)
    corpus = rl.Corpus()
    corpus.read(paths, target, max(rl.MIB, target // 32))
    plan, _ = corpus.plan(target)
    names = [f"realload-{n}" for n in range(len(plan))]
    refs = rl.walk_files(os.path.expanduser(args.home), args.refs)
    metrics["turns"] = sum(count for _, count in plan)
    metrics["sessions"], metrics["facts"] = len(plan), len(refs)

    os.mkdir(os.path.join(root, "attic"), 0o700)
    server, client = load_attic(args, os.path.join(root, "attic"), corpus, plan, names, refs,
                                metrics)
    db = load_sqlite(root, corpus, plan, names, refs, metrics)
    process, redis = load_redis(args, root, corpus, plan, names, refs, metrics)
    try:
        sessions = [rng.choice(names) for _ in range(READS)]
        keys = [rng.choice(refs)[0] for _ in range(READS)]
        words = queries(rng, corpus, plan)
        # Interleaved, so drift on the machine hits all 3 alike.
        times = {f"{s}_{op}": [] for s in ("attic", "sqlite", "redis")
                 for op in ("window", "fact", "search")}
        hits = {"attic": 0, "sqlite": 0}
        for name in sessions:
            times["attic_window"].append(timed(lambda: client.window(rl.AGENT, name, last=20))[0])
            times["sqlite_window"].append(timed(lambda: db.execute(
                "SELECT seq, content FROM turns WHERE session = ? ORDER BY seq DESC LIMIT 20",
                (name,)).fetchall())[0])
            times["redis_window"].append(timed(lambda: redis.call("LRANGE", name, -20, -1))[0])
        for key in keys:
            times["attic_fact"].append(timed(lambda: client.fact_get(key))[0])
            times["sqlite_fact"].append(timed(lambda: db.execute(
                "SELECT content FROM facts WHERE key = ?", (key,)).fetchone())[0])
            times["redis_fact"].append(timed(lambda: redis.call("GET", key))[0])
        for query in words:
            took, reply = timed(lambda: client.search(query, limit=10))
            times["attic_search"].append(took)
            hits["attic"] += bool(reply["hits"])
            match = " AND ".join('"%s"' % word.replace('"', '""') for word in query.split())

            def sqlite_search():
                ids = [row[0] for row in db.execute(
                    "SELECT rowid FROM turns_fts WHERE turns_fts MATCH ? ORDER BY rowid DESC "
                    "LIMIT 10", (match,))]
                marks = ",".join("?" * len(ids))
                return db.execute(f"SELECT content FROM turns WHERE id IN ({marks})",
                                  ids).fetchall() if ids else []
            took, rows = timed(sqlite_search)
            times["sqlite_search"].append(took)
            hits["sqlite"] += bool(rows)
        for key, values in times.items():
            if values:
                stats(key, values, metrics)
        metrics["attic_search_with_hits"] = hits["attic"]
        metrics["sqlite_search_with_hits"] = hits["sqlite"]
        metrics["search_queries"] = len(words)
    finally:
        server.stop(signal.SIGKILL)
        process.kill()
        process.wait(10)
        db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--atticdb", required=True)
    parser.add_argument("--redis", default="redis-server")
    parser.add_argument("--target-mib", type=int, default=512)
    parser.add_argument("--refs", type=int, default=20000)
    parser.add_argument("--home", default="~", help="folder walked for file-ref keys")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--out", help="write the metrics as JSON here")
    parser.add_argument("--keep", action="store_true", help="keep the temp dir")
    args = parser.parse_args()
    for name in ("atticdb", "redis"):
        found = shutil.which(getattr(args, name))
        if not found:
            parser.error(f"{getattr(args, name)} not found")
        setattr(args, name, found)
    for name in ("SIGTERM", "SIGHUP", "SIGQUIT"):
        signal.signal(getattr(signal, name), lambda signum, _: sys.exit(128 + signum))
    root = tempfile.mkdtemp(prefix="reads-")
    metrics = {}
    try:
        run(args, root, metrics)
    except Exception as error:  # noqa: BLE001
        print(f"reads: failed: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    finally:
        if args.keep:
            print(f"reads: kept {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)
    print(rl.table(metrics))
    if args.out:
        with open(args.out, "w") as out:
            json.dump(metrics, out, indent=1)
            out.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
