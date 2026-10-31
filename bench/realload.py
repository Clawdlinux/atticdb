#!/usr/bin/env python3
"""Fills atticdb with real agent transcripts and measures fill, disk, reads, file refs,
reload and a compression estimate. Standard library only.

Phases: 1 corpus, 2 fill, 3 disk, 4 reads, 5 file refs, 6 reload, 7 compression.
The server runs on a 0700 temp dir over a Unix socket. The temp dir holds the only copy
of transcript text and is deleted at exit unless --keep. Output holds numbers only.

Usage: python3 bench/realload.py [--target-mib 256] [--writers 8] [--batch 16]
       [--refs 20000] [--source <glob>]... [--atticdb atticdb] [--out <json>] [--keep]
"""

import argparse
import glob
import json
import lzma
import math
import os
import random
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "clients", "python"))
from atticdb_client import Client, Error  # noqa: E402

MIB = 1 << 20
DEFAULT_SOURCES = ["~/.claude/projects/*/*.jsonl", "~/.copilot/session-state/*/events.jsonl"]
ROLES = ("system", "user", "assistant", "tool")
AGENT = "realload"
# A turn may hold 4 MiB, but a whole batch must fit one 4 MiB tape frame and the request
# body 8 MiB. The client escapes a UTF-8 byte to at most 6 JSON bytes, so 1 MiB pieces
# always fit alone.
TURN_MAX = 1 * MIB
FRAME_BUDGET = 4 * MIB - 4096
TURN_OVERHEAD = 256
BODY_BUDGET = 8 * MIB - 64 * 1024
JSON_OVERHEAD = 64
REF_CONTENT = "realload file ref"
BLOCK = 64 * 1024
BLOCK_SPAN = 64 * MIB
GATE_SAVED = 0.30
WORD = re.compile(r"[A-Za-z]{4,24}")


def split_utf8(data):
    """Splits UTF-8 bytes into pieces of at most TURN_MAX bytes on character boundaries."""
    pieces, start = [], 0
    while len(data) - start > TURN_MAX:
        cut = start + TURN_MAX
        while data[cut] & 0xC0 == 0x80:
            cut -= 1
        pieces.append(data[start:cut])
        start = cut
    pieces.append(data[start:])
    return pieces


def text_of(value):
    """Text of a Claude Code tool_result content: a string or a list of text parts."""
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        parts = (p.get("text") for p in value if isinstance(p, dict))
        return "\n".join(p for p in parts if isinstance(p, str))
    return ""


def turns_of(line):
    """(role, text) pairs from 1 JSONL line."""
    message = line.get("message")
    if isinstance(message, dict):
        role, content = message.get("role"), message.get("content")
        if role not in ROLES:
            return []
        if isinstance(content, str):
            return [(role, content)]
        out = []
        for part in content if isinstance(content, list) else []:
            if not isinstance(part, dict):
                continue
            kind = part.get("type")
            if kind == "text":
                out.append((role, part.get("text")))
            elif kind == "thinking":
                out.append((role, part.get("thinking")))
            elif kind == "tool_use":
                out.append(("tool", json.dumps(part.get("input"))))
            elif kind == "tool_result":
                out.append(("tool", text_of(part.get("content"))))
        return out
    # Other formats: string content and text fields on the line, its data and data.result.
    kind = line.get("type") if isinstance(line.get("type"), str) else ""
    role = line.get("role") if line.get("role") in ROLES else kind.split(".")[0]
    role = role if role in ROLES else "tool"
    data = line.get("data") if isinstance(line.get("data"), dict) else {}
    result = data.get("result") if isinstance(data.get("result"), dict) else {}
    return [(role, obj[k]) for obj in (line, data, result) for k in ("content", "text")
            if isinstance(obj.get(k), str)]


class Corpus:
    """Source sessions as lists of (role, content, utf8 bytes, json bytes) pieces."""

    def __init__(self):
        self.sessions = []
        self.stats = dict.fromkeys(
            ("files", "lines", "bad_lines", "turns", "split_turns", "split_pieces", "bytes"), 0)

    def read(self, paths, target, session_cap):
        for path in paths:
            if self.stats["bytes"] >= target:
                break
            session, size = [], 0
            self.stats["files"] += 1
            with open(path, "rb") as file:
                for raw in file:
                    if size >= session_cap or self.stats["bytes"] + size >= target:
                        break
                    self.stats["lines"] += 1
                    try:
                        line = json.loads(raw)
                    except ValueError:
                        self.stats["bad_lines"] += 1
                        continue
                    for role, text in turns_of(line) if isinstance(line, dict) else []:
                        if not isinstance(text, str) or not text.strip():
                            continue
                        # Lone surrogates are not valid UTF-8, so the server would refuse them.
                        data = text.encode("utf-8", "replace")
                        pieces = split_utf8(data)
                        self.stats["turns"] += 1
                        if len(pieces) > 1:
                            self.stats["split_turns"] += 1
                            self.stats["split_pieces"] += len(pieces)
                        for piece in pieces:
                            content = piece.decode("utf-8")
                            session.append((role, content, len(piece), len(json.dumps(content))))
                            size += len(piece)
            if session:
                self.sessions.append(session)
                self.stats["bytes"] += size

    def plan(self, target):
        """Appended sessions as (source index, turn count), repeating sources until the
        target size. The last one may be cut short."""
        out, total = [], 0
        while total < target and self.stats["bytes"] > 0:
            for index, session in enumerate(self.sessions):
                count = 0
                for _, _, size, _ in session:
                    if total >= target:
                        break
                    total, count = total + size, count + 1
                if count:
                    out.append((index, count))
                if total >= target:
                    break
        return out, total


def batches(session, count, batch):
    """Splits a session's first `count` pieces into batches that fit a frame and a body."""
    out, current, frame, body = [], [], 0, 0
    for role, content, size, escaped in session[:count]:
        more_frame, more_body = size + TURN_OVERHEAD, escaped + JSON_OVERHEAD
        if current and (len(current) == batch or frame + more_frame > FRAME_BUDGET
                        or body + more_body > BODY_BUDGET):
            out.append(current)
            current, frame, body = [], 0, 0
        current.append({"role": role, "content": content})
        frame, body = frame + more_frame, body + more_body
    if current:
        out.append(current)
    return out


def pct(values, p):
    if not values:
        return float("nan")
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, math.ceil(p * len(ordered)) - 1))]


def ms_stats(prefix, values, metrics):
    metrics[f"{prefix}_p50_ms"] = pct(values, 0.50) * 1000
    metrics[f"{prefix}_p99_ms"] = pct(values, 0.99) * 1000


def timed(call):
    started = time.perf_counter()
    result = call()
    return time.perf_counter() - started, result


class Server:
    """`atticdb serve` on a Unix socket in the temp dir."""

    def __init__(self, binary, root):
        self.binary, self.root = binary, root
        self.config = os.path.join(root, "c.json")
        self.data = os.path.join(root, "data")
        self.socket = os.path.join(root, "s.sock")
        self.log = os.path.join(root, "serve.log")
        self.process = None
        token_path = os.path.join(root, "token")
        fd = os.open(token_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as out:
            subprocess.run([binary, "init", "--config", self.config, "--tenant", "bench"],
                           stdout=out, stdin=subprocess.DEVNULL, check=True, timeout=60,
                           umask=0o077)
        with open(token_path) as file:
            self.token = file.read().strip()
        os.mkdir(self.data, 0o700)

    def start(self):
        with open(self.log, "ab") as log:
            self.process = subprocess.Popen(
                [self.binary, "serve", "--config", self.config, "--data-dir", self.data,
                 "--listen", "none", "--unix-socket", self.socket, "--exit-on-poison"],
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)

    def wait_answer(self, read, timeout=120.0):
        """Seconds from now until `read` answers."""
        started = time.perf_counter()
        while True:
            if self.process.poll() is not None:
                raise RuntimeError(f"server exited with {self.process.returncode}; "
                                   "rerun with --keep to read serve.log")
            try:
                read()
                return time.perf_counter() - started
            except (Error, OSError):
                if time.perf_counter() - started > timeout:
                    raise RuntimeError("server did not answer in time") from None
                time.sleep(0.002)

    def stop(self, sig, timeout=60.0):
        """Sends `sig` and returns seconds until the process exits."""
        if not self.process or self.process.poll() is not None:
            return 0.0
        started = time.perf_counter()
        self.process.send_signal(sig)
        try:
            self.process.wait(timeout)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(10)
        return time.perf_counter() - started


def walk_files(home, limit):
    """Up to `limit` regular files under `home`, hidden dirs skipped, with their keys."""
    out = []
    for folder, dirs, files in os.walk(home):
        dirs[:] = sorted(d for d in dirs if not d.startswith("."))
        for name in sorted(files):
            path = os.path.join(folder, name)
            key = "file/" + path.replace(os.sep, "/")
            try:
                info = os.lstat(path)
                encoded = key.encode("utf-8")
            except (OSError, UnicodeEncodeError):
                continue
            if stat.S_ISREG(info.st_mode) and len(encoded) <= 1024:
                out.append((key, info.st_size, info.st_mtime_ns))
                if len(out) >= limit:
                    return out
    return out


def disk_usage(root):
    allocated = apparent = 0
    for folder, _, files in os.walk(root):
        for name in files:
            info = os.lstat(os.path.join(folder, name))
            # st_blocks counts 512-byte units. Windows lacks it, so size stands in.
            allocated += info.st_blocks * 512 if hasattr(info, "st_blocks") else info.st_size
            apparent += info.st_size
    return allocated, apparent


def run(args, root, metrics):
    rng = random.Random(args.seed)
    target = args.target_mib * MIB

    # 1. Corpus.
    paths = sorted({p for g in args.source for p in glob.glob(os.path.expanduser(g))})
    random.Random(args.seed).shuffle(paths)
    corpus = Corpus()
    # Cap each source session so the target spans many sessions and all writers.
    took, _ = timed(lambda: corpus.read(paths, target, max(MIB, target // 32)))
    if not corpus.sessions:
        raise RuntimeError("no turns found in the sources")
    plan, planned = corpus.plan(target)
    metrics.update({f"corpus_{k}": v for k, v in corpus.stats.items() if k != "bytes"})
    metrics["corpus_source_files_found"] = len(paths)
    metrics["corpus_source_mib"] = corpus.stats["bytes"] / MIB
    metrics["corpus_source_sessions"] = len(corpus.sessions)
    metrics["corpus_sessions"] = len(plan)
    metrics["corpus_repeats"] = max(0, len(plan) - len(corpus.sessions))
    metrics["corpus_read_s"] = took
    names = [f"realload-{n}" for n in range(len(plan))]

    # 2. Fill.
    server = Server(args.atticdb, root)
    client = Client(f"unix://{server.socket}", server.token)
    server.start()
    try:
        metrics["start_s"] = server.wait_answer(client.health)
        fill(args, client, corpus, plan, names, metrics)

        # 3. Disk, while the server runs: nothing more is written until phase 5.
        allocated, apparent = disk_usage(server.data)
        metrics["disk_allocated_mib"] = allocated / MIB
        metrics["disk_apparent_mib"] = apparent / MIB
        metrics["disk_logical_mib"] = planned / MIB
        metrics["disk_allocated_per_logical"] = allocated / planned

        # 4. Reads.
        windows = []
        for name in (rng.choice(names) for _ in range(200)):
            windows.append(timed(lambda: client.window(AGENT, name, last=20))[0])
        ms_stats("read_window20", windows, metrics)
        searches, found = [], 0
        for word in draw_words(rng, corpus, plan, 100):
            took, reply = timed(lambda: client.search(word))
            searches.append(took)
            found += bool(reply["hits"])
        ms_stats("read_search", searches, metrics)
        metrics["read_search_queries"] = len(searches)
        metrics["read_search_with_hits"] = found

        # 5. File refs.
        file_refs(args, rng, client, metrics)

        # 6. Reload. A sample window must read the same before and after.
        sample = rng.choice(names)
        read = lambda: client.window(AGENT, sample, last=20)  # noqa: E731
        before = [t["seq"] for t in read()["turns"]]
        server.stop(signal.SIGKILL)
        server.start()
        metrics["reload_after_kill_s"] = server.wait_answer(read)
        intact = [t["seq"] for t in read()["turns"]] == before
        metrics["reload_clean_stop_s"] = server.stop(signal.SIGTERM)
        server.start()
        metrics["reload_after_clean_stop_s"] = server.wait_answer(read)
        metrics["reload_window_intact"] = intact and [t["seq"] for t in read()["turns"]] == before
    finally:
        server.stop(signal.SIGKILL)

    # 7. Compression.
    compression(corpus, plan, metrics)


def fill(args, client, corpus, plan, names, metrics):
    # Balance sessions over writers by bytes. Each writer interleaves its sessions'
    # batches round-robin, and each session has 1 writer, so turns stay in order.
    loads = [[0, []] for _ in range(args.writers)]
    sized = [(sum(t[2] for t in corpus.sessions[i][:c]), name, i, c)
             for name, (i, c) in zip(names, plan)]
    for size, name, index, count in sorted(sized, key=lambda item: -item[0]):
        lightest = min(loads, key=lambda load: load[0])
        lightest[0] += size
        lightest[1].append((name, batches(corpus.sessions[index], count, args.batch)))
    latencies, lock = [], threading.Lock()

    def writer(assigned):
        mine, queues = [], [(name, iter(b)) for name, b in assigned]
        while queues:
            alive = []
            for name, queue in queues:
                batch = next(queue, None)
                if batch is None:
                    continue
                took, _ = timed(lambda: client.append(AGENT, name, batch))
                mine.append(took)
                alive.append((name, queue))
            queues = alive
        with lock:
            latencies.extend(mine)

    started = time.perf_counter()
    with ThreadPoolExecutor(args.writers) as pool:
        for future in [pool.submit(writer, assigned) for _, assigned in loads]:
            future.result()
    took = time.perf_counter() - started
    turns = sum(count for _, count in plan)
    content = sum(sum(t[2] for t in corpus.sessions[i][:c]) for i, c in plan)
    metrics["fill_s"] = took
    metrics["fill_turns"] = turns
    metrics["fill_batches"] = len(latencies)
    metrics["fill_turns_per_s"] = turns / took
    metrics["fill_mib_per_s"] = content / MIB / took
    ms_stats("fill_batch", latencies, metrics)


def draw_words(rng, corpus, plan, n):
    words, tries = [], 0
    while len(words) < n and tries < n * 100:
        tries += 1
        index, count = rng.choice(plan)
        content = corpus.sessions[index][rng.randrange(count)][1]
        start = rng.randrange(max(1, len(content) - 4096))
        found = WORD.findall(content[start:start + 4096])
        if found:
            words.append(rng.choice(found).lower())
    return words


def file_refs(args, rng, client, metrics):
    took, refs = timed(lambda: walk_files(os.path.expanduser(args.home), args.refs))
    metrics["refs_walk_s"] = took
    metrics["refs_count"] = len(refs)
    if not refs:
        return

    def put(ref):
        key, size, modified = ref
        meta = {"file": {"bytes": size, "modified_ns": modified, "blake3": "0" * 64,
                         "mime": "application/octet-stream"}}
        return timed(lambda: client.fact_put(key, REF_CONTENT, meta=meta, agent=AGENT))[0]

    started = time.perf_counter()
    with ThreadPoolExecutor(args.writers) as pool:
        puts = list(pool.map(put, refs))
    metrics["refs_put_per_s"] = len(refs) / (time.perf_counter() - started)
    ms_stats("refs_put", puts, metrics)
    gets, hits = [], 0
    for key, _, _ in (rng.choice(refs) for _ in range(1000)):
        took, fact = timed(lambda: client.fact_get(key))
        gets.append(took)
        hits += fact is not None
    ms_stats("refs_get", gets, metrics)
    metrics["refs_get_found"] = hits
    lists, listed = [], 0
    for key, _, _ in (rng.choice(refs) for _ in range(200)):
        prefix = key.rsplit("/", 1)[0] + "/"
        took, page = timed(lambda: client.fact_list(prefix))
        lists.append(took)
        listed += len(page["facts"])
    ms_stats("refs_list", lists, metrics)
    metrics["refs_list_mean_facts"] = listed / len(lists)


def compression(corpus, plan, metrics):
    # Per turn over every appended turn. Repeated sessions reuse their sizes.
    sizes, raw, packed, started = {}, 0, 0, time.perf_counter()
    for index, count in plan:
        session = corpus.sessions[index]
        for i in range(count):
            if (index, i) not in sizes:
                sizes[index, i] = len(zlib.compress(session[i][1].encode("utf-8"), 6))
            raw, packed = raw + session[i][2], packed + sizes[index, i]
    metrics["compress_turn_zlib6_saved_pct"] = 100 * (1 - packed / raw)
    metrics["compress_turn_zlib6_s"] = time.perf_counter() - started
    metrics["compress_gate_30pct_met"] = 1 - packed / raw >= GATE_SAVED

    # 64 KiB blocks over the first 64 MiB, in append order.
    chunks, size = [], 0
    for index, count in plan:
        for piece in corpus.sessions[index][:count]:
            if size >= BLOCK_SPAN:
                break
            chunks.append(piece[1].encode("utf-8"))
            size += piece[2]
    span = b"".join(chunks)[:BLOCK_SPAN]
    blocks = [span[i:i + BLOCK] for i in range(0, len(span), BLOCK)]
    metrics["compress_block_input_mib"] = len(span) / MIB
    for name, squeeze in (("zlib6", lambda b: zlib.compress(b, 6)), ("lzma", lzma.compress)):
        took, packed = timed(lambda: sum(len(squeeze(b)) for b in blocks))
        metrics[f"compress_block_{name}_saved_pct"] = 100 * (1 - packed / max(1, len(span)))
        metrics[f"compress_block_{name}_mib_per_s"] = len(span) / MIB / took if took else 0.0


def table(metrics):
    lines = ["| metric | value |", "|---|---|"]
    for key, value in metrics.items():
        if isinstance(value, bool):
            shown = "yes" if value else "no"
        elif isinstance(value, float):
            shown = f"{value:,.3f}" if abs(value) < 100 else f"{value:,.1f}"
        else:
            shown = f"{value:,}"
        lines.append(f"| {key} | {shown} |")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--source", action="append", help="JSONL glob, repeatable")
    parser.add_argument("--target-mib", type=int, default=256)
    parser.add_argument("--writers", type=int, default=8)
    parser.add_argument("--batch", type=int, default=16)
    parser.add_argument("--refs", type=int, default=20000)
    parser.add_argument("--home", default="~", help="folder walked for file refs")
    parser.add_argument("--atticdb", default="atticdb")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--out", help="write the metrics as JSON here")
    parser.add_argument("--keep", action="store_true", help="keep the temp dir")
    args = parser.parse_args()
    args.source = args.source or DEFAULT_SOURCES
    if args.target_mib < 1 or args.writers < 1 or not 1 <= args.batch <= 1024 or args.refs < 0:
        parser.error("--target-mib, --writers and --batch must be positive, --batch at most 1024")
    binary = shutil.which(args.atticdb)
    if not binary:
        parser.error(f"{args.atticdb} not found")
    args.atticdb = binary
    # SIGTERM unwinds like Ctrl-C, so the server stops and the temp dir goes.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))

    # mkdtemp makes the dir 0700. A short base keeps the socket path under 104 bytes.
    root = tempfile.mkdtemp(prefix="realload-")
    metrics = {}
    try:
        run(args, root, metrics)
    except Exception as error:  # noqa: BLE001
        # Type and message only: they never hold transcript text.
        print(f"realload: failed: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    finally:
        if args.keep:
            print(f"realload: kept {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)
    print(table(metrics))
    if args.out:
        with open(args.out, "w") as out:
            json.dump(metrics, out, indent=1)
            out.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
