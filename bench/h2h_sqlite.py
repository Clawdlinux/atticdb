"""Closed-loop durable inserts into SQLite: WAL, synchronous=FULL, one row per transaction.

Each writer is a process with its own connection, as separate agents would be.
Usage: h2h_sqlite.py <db path> <writers> <rows per writer> <row bytes>
"""

import math
import multiprocessing as mp
import os
import sqlite3
import sys
import time


def writer(path, rows, size, out):
    db = sqlite3.connect(path, timeout=60, isolation_level=None)
    db.execute("PRAGMA synchronous=FULL")
    body, samples = os.urandom(size), []
    start = time.perf_counter()
    for _ in range(rows):
        sent = time.perf_counter()
        db.execute("INSERT INTO turns(body) VALUES (?)", (body,))
        samples.append(time.perf_counter() - sent)
    out.put((samples, start, time.perf_counter()))


def main():
    path, (writers, rows, size) = sys.argv[1], map(int, sys.argv[2:5])
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("CREATE TABLE turns(id INTEGER PRIMARY KEY, body BLOB)")
    db.close()
    out = mp.Queue()
    procs = [mp.Process(target=writer, args=(path, rows, size, out)) for _ in range(writers)]
    for proc in procs:
        proc.start()
    results = [out.get() for _ in procs]
    for proc in procs:
        proc.join()
    samples = sorted(s for result in results for s in result[0])
    seconds = max(r[2] for r in results) - min(r[1] for r in results)
    us = lambda p: samples[max(1, math.ceil(len(samples) * p)) - 1] * 1e6
    print(f"turns={len(samples)}\nseconds={seconds:.3f}")
    print(f"turns_per_sec={len(samples) / seconds:.1f}")
    for name, p in [("p50_us", 0.5), ("p99_us", 0.99), ("p999_us", 0.999), ("max_us", 1.0)]:
        print(f"{name}={us(p):.1f}")


if __name__ == "__main__":
    main()
