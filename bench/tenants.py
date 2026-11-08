"""Tenants per VM: 1 `atticdb serve` per tenant against 1 shared `serve` with N tenants.

Usage: tenants.py --layout per-process|shared --tenants N --active-pct P --duration S
                  --think-ms T --out <tsv>
The binary comes from ATTICDB_BIN. Run directories go under TENANTS_BASE, default bench/.tb.
Unix socket paths must stay under 104 bytes on macOS, so keep TENANTS_BASE short.
On a VM, point TENANTS_BASE at the data disk. per-process needs about 35 MiB per tenant.
Appends 1 row to <tsv>, with a header when the file is new. p50_ms and p99_ms cover all
ops. The _append_ms and _fact_ms columns split them by op. See bench/TENANTS.md.
"""

import argparse
import json
import math
import multiprocessing as mp
import os
import random
import resource
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import time

COLUMNS = ("layout tenants active_pct ops p50_ms p99_ms p50_append_ms p99_append_ms p50_fact_ms "
           "p99_fact_ms rss_mib_idle rss_mib_load fds threads disk_mib_per_tenant "
           "cold_start_ms").split()
SEGMENT_MIB = 16
# A fresh per-process data dir is about 34 MiB. shared has 1 tape for every tenant.
MIB_PER_PROCESS_TENANT = 35
SHARED_FLOOR_MIB = 256
COLD_STARTS = 5
SIZE = 256
FACT_KEYS = 16
SEED = 1
IDLE_SECONDS = 5
# macOS sun_path is 104 bytes with the NUL. Linux allows 108.
MAX_SOCKET_BYTES = 103
# serve binds <dir>/.zs.<8 hex>/s first, then links it to <dir>/s.
STAGING_EXTRA_BYTES = len("/.zs.01234567")
TOOL_TIMEOUT = 120
LINUX = sys.platform.startswith("linux")


def config_json(tenants, segment_mib):
    return {"tenants": [{"name": name, "token": token} for name, token in tenants],
            "segment_mib": segment_mib}


def pick_active(n, pct, seed):
    count = max(1, round(n * pct / 100))
    return sorted(random.Random(seed).sample(range(n), count))


def percentile(samples, q):
    """Nearest rank. q is a percent, 0 to 100."""
    ordered = sorted(samples)
    rank = max(1, math.ceil(q * len(ordered) / 100))
    return ordered[rank - 1]


def latency_columns(samples):
    """samples are (op, seconds). Returns p50 and p99 in ms for all ops, append, then fact.
    An op with no samples gets `-`."""
    out = []
    for op in (None, "append", "fact"):
        values = [s for o, s in samples if op is None or o == op]
        for q in (50, 99):
            out.append(f"{percentile(values, q) * 1000:.3f}" if values else "-")
    return out


def needed_mib(layout, n):
    return n * MIB_PER_PROCESS_TENANT if layout == "per-process" else SHARED_FLOOR_MIB


def count_lsof_fds(text):
    """Counts numbered fds in `lsof -F f` output. cwd, txt and mem rows are not fds."""
    return sum(1 for line in text.splitlines() if line[:1] == "f" and line[1:].isdigit())


def count_ps_threads(text):
    """`ps -M` prints a header, then 1 line per thread."""
    return max(0, len([line for line in text.splitlines() if line.strip()]) - 1)


def staged_socket_bytes(sock):
    """Bytes in the staging path serve binds for `sock`. serve makes `sock` absolute."""
    return len(os.fsencode(os.path.abspath(sock))) + STAGING_EXTRA_BYTES


def check_socket(sock):
    staged = staged_socket_bytes(sock)
    if staged > MAX_SOCKET_BYTES:
        raise SystemExit(f"socket path {sock} needs {staged} bytes while serve binds it, over "
                         f"{MAX_SOCKET_BYTES}. Set a shorter TENANTS_BASE.")


def http(method, target, token, body=b""):
    head = f"{method} {target} HTTP/1.1\r\nAuthorization: Bearer {token}\r\n"
    if method == "POST":
        head += f"Content-Length: {len(body)}\r\n"
    return (head + "\r\n").encode() + body


def recv(conn):
    data = conn.recv(65536)
    if not data:
        raise RuntimeError("connection closed before the reply")
    return data


def read_http(conn, buf):
    """Returns (status, body, leftover bytes)."""
    while b"\r\n\r\n" not in buf:
        buf += recv(conn)
    head, buf = buf.split(b"\r\n\r\n", 1)
    lines = head.split(b"\r\n")
    length = next((int(l.split(b":")[1]) for l in lines if l.lower().startswith(b"content-length:")), 0)
    while len(buf) < length:
        buf += recv(conn)
    return int(lines[0].split()[1]), buf[:length], buf[length:]


def answers(path, token):
    """True when the socket answers `GET /v1/facts/get?key=x` with 200."""
    try:
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(5)
            conn.connect(path)
            conn.sendall(http("GET", "/v1/facts/get?key=x", token))
            return read_http(conn, b"")[0] == 200
    except (OSError, RuntimeError, ValueError, IndexError):
        return False


def client(path, token, index, run, deadline, think_ms):
    """1 request in flight. Returns (op, round trip seconds) per op. Think time is outside."""
    rng = random.Random(SEED * 1_000_003 + index)
    content = "x" * SIZE
    samples, buf, i = [], b"", 0
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(30)
        conn.connect(path)
        while True:
            if think_ms:
                time.sleep(rng.uniform(0, 2 * think_ms / 1000))
            if time.time() >= deadline:
                return samples
            idem = f"{run}-{index}-{i}"
            if i % 2 == 0:
                op, target = "append", "/v1/append"
                body = {"agent": "bench", "session": f"s{index}", "idem_key": idem,
                        "turns": [{"role": "user", "content": content}]}
            else:
                op, target = "fact", "/v1/facts/put"
                body = {"key": f"k{rng.randrange(FACT_KEYS)}", "content": content,
                        "agent": "bench", "idem_key": idem}
            data = http("POST", target, token, json.dumps(body).encode())
            sent = time.perf_counter()
            conn.sendall(data)
            status, reply, buf = read_http(conn, buf)
            samples.append((op, time.perf_counter() - sent))
            if status != 200:
                raise RuntimeError(f"tenant {index} {target}: {status} {reply[:200]!r}")
            i += 1


class Server:
    def __init__(self, root, tenants):
        self.root, self.tenants = root, tenants
        self.config, self.data = f"{root}/c.json", f"{root}/d"
        self.sock, self.log = f"{root}/s", f"{root}/log"
        self.proc = None
        check_socket(self.sock)
        os.makedirs(root, mode=0o700)
        fd = os.open(self.config, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(config_json(tenants, SEGMENT_MIB), f)

    def start(self, binary):
        with open(self.log, "ab") as log:
            self.proc = subprocess.Popen(
                [binary, "serve", "--config", self.config, "--data-dir", self.data,
                 "--listen", "none", "--unix-socket", self.sock],
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                start_new_session=True)

    def check_alive(self):
        if self.proc.poll() is not None:
            with open(self.log, errors="replace") as log:
                tail = log.read()[-2000:]
            raise RuntimeError(f"serve in {self.root} exited {self.proc.returncode}:\n{tail}")

    def stop(self, timeout=30):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(10)


def wait_ready(servers, timeout):
    deadline = time.monotonic() + timeout
    pending = list(servers)
    while pending:
        for server in pending:
            server.check_alive()
        pending = [s for s in pending if not answers(s.sock, s.tenants[0][1])]
        if pending and time.monotonic() > deadline:
            raise RuntimeError(f"{len(pending)} serve processes not ready after {timeout:.0f} s")
        if pending:
            time.sleep(0.05)


def tool(args):
    return subprocess.run(args, capture_output=True, text=True, timeout=TOOL_TIMEOUT).stdout


def sample(servers):
    """Sums RSS MiB, fds and threads over every serve pid."""
    for server in servers:
        server.check_alive()
    pids = [str(s.proc.pid) for s in servers]
    rss = [int(v) for v in tool(["ps", "-o", "rss=", "-p", ",".join(pids)]).split()]
    if len(rss) != len(pids):
        raise RuntimeError(f"ps returned {len(rss)} RSS values for {len(pids)} pids")
    if LINUX:
        fds = sum(len(os.listdir(f"/proc/{pid}/fd")) for pid in pids)
        threads = 0
        for pid in pids:
            with open(f"/proc/{pid}/status") as status:
                threads += next(int(l.split()[1]) for l in status if l.startswith("Threads:"))
    else:
        text = tool(["lsof", "-nP", "-F", "f", "-p", ",".join(pids)])
        seen = sum(1 for line in text.splitlines() if line[:1] == "p")
        if seen != len(pids):
            raise RuntimeError(f"lsof listed {seen} of {len(pids)} pids")
        fds = count_lsof_fds(text)
        threads = count_ps_threads(tool(["ps", "-M", "-p", ",".join(pids)]))
    return sum(rss) / 1024, fds, threads


def disk_kib(dirs):
    """Sums `du -sk` over dirs. Warns on stderr for a dir that du could not size."""
    done = subprocess.run(["du", "-sk", *dirs], capture_output=True, text=True,
                          timeout=TOOL_TIMEOUT)
    sized = dict(line.split("\t", 1)[::-1] for line in done.stdout.splitlines())
    missing = [d for d in dirs if d not in sized]
    if done.returncode or missing:
        print(f"tenants.py: warning: du exited {done.returncode}, no size for "
              f"{len(missing)} of {len(dirs)} dirs {missing[:3]}: {done.stderr.strip()[:500]}",
              file=sys.stderr)
    return sum(int(kib) for kib in sized.values())


def raise_fd_limit():
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    want = 65536 if hard == resource.RLIM_INFINITY else min(hard, 65536)
    if soft != resource.RLIM_INFINITY and soft < want:
        try:
            resource.setrlimit(resource.RLIMIT_NOFILE, (want, hard))
        except (ValueError, OSError) as error:
            print(f"tenants.py: warning: cannot raise the fd limit from {soft} to {want}: "
                  f"{error}", file=sys.stderr)


def check_space(base, layout, n):
    need = needed_mib(layout, n)
    free = shutil.disk_usage(base).free / 2**20
    if free < need:
        raise SystemExit(f"TENANTS_BASE {base} has {free:.0f} MiB free. {layout} at N={n} "
                         f"needs about {need} MiB. Point TENANTS_BASE at a bigger disk.")


def cold_start_ms(server, binary):
    """Stops the server, spawns it on its existing dir and times the first 200."""
    server.stop()
    started = time.perf_counter()
    server.start(binary)
    while not answers(server.sock, server.tenants[0][1]):
        server.check_alive()
        if time.perf_counter() > started + 60:
            raise RuntimeError("restarted serve not ready after 60 s")
        time.sleep(0.001)
    return (time.perf_counter() - started) * 1000


def ignore_signals():
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)


def stop_on_signal(signum, _frame):
    """Unwinds into the teardown, which stops every serve this run started. Repeats are
    ignored, so a second signal cannot cut the teardown short."""
    ignore_signals()
    sys.exit(128 + signum)


def run_bench(args, binary, run_dir):
    run = os.urandom(4).hex()
    tokens = [(f"t{i}", os.urandom(16).hex()) for i in range(args.tenants)]
    if args.layout == "shared":
        servers = [Server(f"{run_dir}/x", tokens)]
        routes = [(servers[0], token) for token in tokens]
    else:
        servers = [Server(f"{run_dir}/t{i}", [token]) for i, token in enumerate(tokens)]
        routes = [(server, server.tenants[0]) for server in servers]
    try:
        for server in servers:
            server.start(binary)
        wait_ready(servers, 60 + 0.2 * args.tenants)
        time.sleep(IDLE_SECONDS)
        rss_idle, _, _ = sample(servers)

        active = pick_active(args.tenants, args.active_pct, SEED)
        deadline = time.time() + args.duration
        jobs = [(routes[i][0].sock, routes[i][1][1], i, run, deadline, args.think_ms) for i in active]
        with mp.Pool(len(jobs)) as pool:
            pending = pool.starmap_async(client, jobs)
            # Sample near the end while every client still holds its connection. `serve` runs
            # 1 thread per connection, so a sample after the clients close would miss them.
            time.sleep(max(0, deadline - min(2, args.duration / 10) - time.time()))
            rss_load, fds, threads = sample(servers)
            results = pending.get(args.duration + 120)
        samples = [s for result in results for s in result]
        if not samples:
            raise RuntimeError("no ops completed")

        cold = "-"
        if args.layout == "per-process":
            times = [cold_start_ms(servers[0], binary) for _ in range(COLD_STARTS)]
            cold = f"{statistics.median(times):.1f}"
        disk = disk_kib([s.data for s in servers]) / 1024 / args.tenants
    finally:
        # A signal must not cut the teardown short and leave servers running.
        ignore_signals()
        for server in servers:
            if server.proc and server.proc.poll() is None:
                server.proc.terminate()
        for server in servers:
            server.stop(timeout=15)
    return [args.layout, args.tenants, f"{args.active_pct:g}", len(samples),
            *latency_columns(samples), f"{rss_idle:.1f}", f"{rss_load:.1f}", fds, threads,
            f"{disk:.2f}", cold]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--layout", choices=["per-process", "shared"], required=True)
    parser.add_argument("--tenants", type=int, required=True)
    parser.add_argument("--active-pct", type=float, required=True)
    parser.add_argument("--duration", type=float, required=True)
    parser.add_argument("--think-ms", type=float, required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    if args.tenants < 1 or not 0 < args.active_pct <= 100:
        parser.error("--tenants must be at least 1 and --active-pct in (0, 100]")
    binary = os.environ.get("ATTICDB_BIN")
    if not binary or not os.access(binary, os.X_OK):
        parser.error("set ATTICDB_BIN to an atticdb binary")
    binary = os.path.abspath(binary)
    if os.path.exists(args.out) and os.path.getsize(args.out):
        with open(args.out) as out:
            if out.readline().rstrip("\n").split("\t") != COLUMNS:
                parser.error(f"{args.out} has other columns. Use a new --out file.")
    raise_fd_limit()
    signal.signal(signal.SIGTERM, stop_on_signal)
    signal.signal(signal.SIGINT, stop_on_signal)
    base = os.environ.get("TENANTS_BASE") or os.path.join(os.path.dirname(os.path.abspath(__file__)), ".tb")
    run_dir = os.path.join(base, os.urandom(4).hex())
    os.makedirs(run_dir, mode=0o700)
    try:
        check_space(base, args.layout, args.tenants)
        row = run_bench(args, binary, run_dir)
    finally:
        shutil.rmtree(run_dir, ignore_errors=True)
        try:
            os.rmdir(base)
        except OSError:
            pass
    new = not os.path.exists(args.out) or os.path.getsize(args.out) == 0
    with open(args.out, "a") as out:
        if new:
            out.write("\t".join(COLUMNS) + "\n")
        out.write("\t".join(str(v) for v in row) + "\n")
    print("\t".join(f"{k}={v}" for k, v in zip(COLUMNS, row)))


if __name__ == "__main__":
    main()
