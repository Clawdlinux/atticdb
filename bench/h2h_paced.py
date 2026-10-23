"""Agent-paced durable appends: each writer sleeps a uniform 0 to 2x think, then sends one.

One process per writer, one raw socket each, so every system pays the same client cost.
Speaks RESP (RPUSH) to Redis-family servers and HTTP (POST /v1/append) to zerodb.
Usage: h2h_paced.py resp|zerodb <unix socket> <writers> <requests per writer> <bytes> <think ms>
zerodb reads its token from ZERODB_TOKEN.
"""

import json
import math
import multiprocessing as mp
import os
import random
import socket
import sys
import time


def resp_request(writer, i, body):
    key = f"s{writer}".encode()
    parts = [b"RPUSH", key, body]
    return b"*3\r\n" + b"".join(b"$%d\r\n%s\r\n" % (len(p), p) for p in parts)


def zerodb_request(writer, i, body):
    turn = {"role": "user", "content": body.decode()}
    run = os.environ["H2H_RUN"]
    payload = json.dumps({"agent": "paced", "session": f"s{writer}",
                          "idem_key": f"{run}-{writer}-{i}", "turns": [turn]}).encode()
    head = (f"POST /v1/append HTTP/1.1\r\nAuthorization: Bearer {os.environ['ZERODB_TOKEN']}\r\n"
            f"Content-Length: {len(payload)}\r\n\r\n")
    return head.encode() + payload


def recv(conn):
    data = conn.recv(4096)
    if not data:
        raise RuntimeError("connection closed before the reply")
    return data


def read_resp(conn, buf):
    while b"\r\n" not in buf:
        buf += recv(conn)
    line, buf = buf.split(b"\r\n", 1)
    if not line.startswith(b":"):
        raise RuntimeError(f"reply {line!r}")
    return buf


def read_http(conn, buf):
    while b"\r\n\r\n" not in buf:
        buf += recv(conn)
    head, buf = buf.split(b"\r\n\r\n", 1)
    length = next(int(l.split(b":")[1]) for l in head.split(b"\r\n") if l.lower().startswith(b"content-length:"))
    while len(buf) < length:
        buf += recv(conn)
    if not head.startswith(b"HTTP/1.1 200 "):
        raise RuntimeError(f"reply {head[:40]!r} {buf[:length]!r}")
    return buf[length:]


def writer(proto, path, index, requests, size, think):
    request, read = (resp_request, read_resp) if proto == "resp" else (zerodb_request, read_http)
    conn = socket.socket(socket.AF_UNIX)
    conn.connect(path)
    body, buf, samples = b"x" * size, b"", []
    rng = random.Random(index)
    start = time.perf_counter()
    for i in range(requests):
        time.sleep(rng.uniform(0, 2 * think / 1000))
        data = request(index, i, body)
        sent = time.perf_counter()
        conn.sendall(data)
        buf = read(conn, buf)
        samples.append(time.perf_counter() - sent)
    return samples, start, time.perf_counter()


def main():
    proto, path = sys.argv[1:3]
    writers, requests, size = map(int, sys.argv[3:6])
    think = float(sys.argv[6])
    os.environ["H2H_RUN"] = os.urandom(4).hex()
    with mp.Pool(writers) as pool:
        args = [(proto, path, w, requests, size, think) for w in range(writers)]
        results = pool.starmap(writer, args)
    samples = sorted(s for result in results for s in result[0])
    seconds = max(r[2] for r in results) - min(r[1] for r in results)
    us = lambda p: samples[max(1, math.ceil(len(samples) * p)) - 1] * 1e6
    print(f"turns={len(samples)}\nseconds={seconds:.3f}")
    print(f"turns_per_sec={len(samples) / seconds:.1f}")
    for name, p in [("p50_us", 0.5), ("p99_us", 0.99), ("p999_us", 0.999), ("max_us", 1.0)]:
        print(f"{name}={us(p):.1f}")


if __name__ == "__main__":
    main()
