"""Run every scored question through the 5 arms, and write results.csv and summary.md.

Each conversation gets a fresh temp ATTICDB_HOME and its own `atticdb serve` on a Unix socket.
The server is stopped and the home deleted after the conversation, on errors too.
A failed search or a bad reply stops the run. It is never scored as 0 recall.
Outputs are written only at the end, and only to the --out dir.
"""

from __future__ import annotations

import csv
import datetime
import io
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path

from research.setup_eval import guard
from research.setup_eval.harness import SUN_PATH_BYTES
from research.setup_eval.proc import Runner

from . import arms, data, score

ARMS = ("atticdb-question", "atticdb-1word", "atticdb-2words", "fts5-bm25", "random")
KS = (5, 10, 20)
TOP = 20  # Ids kept per row. Scoring at any k uses this list.
SUMMARY_K = 10
TENANT = "locomo"
ROLE = "user"
BATCH_TURNS = 1000  # The server takes at most 1,024 turns per append.
BATCH_BYTES = 4 << 20  # Well under the 8 MiB body cap.
CHILD_TIMEOUT = 30.0
START_TIMEOUT = 30.0
STOP_TIMEOUT = 5.0
SERVING = "atticdb: serving "

CSV_COLUMNS = (["sample_id", "q_index", "category", "arm", "query", "retrieved", "n_evidence"]
               + [f"{m}@{k}" for m in ("recall", "hit", "tokens") for k in KS] + ["ms"])


class RunError(Exception):
    """The run can't go on. The CLI prints it and exits 1."""


def child_env(home: str) -> dict:
    """The parent env without any ATTICDB_* var, then this store's own ATTICDB_HOME."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("ATTICDB_")}
    env["ATTICDB_HOME"] = home
    return env


class Store:
    """A fresh AtticDB home and server for 1 conversation, used as a context manager.

    The layout is `<root>/zh` as ATTICDB_HOME, holding `atticdb.json` and `data/`.
    The socket is `<root>/s`, and the server log is `<root>/serve.log`.
    """

    def __init__(self, atticdb: str):
        self.atticdb = atticdb
        self.root: str | None = None
        self.process: subprocess.Popen | None = None
        self.client = None

    def __enter__(self) -> Store:
        try:
            self.start()
        except BaseException:
            self.close()
            raise
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def start(self) -> None:
        self.root = tempfile.mkdtemp(prefix="zlr-")
        home = os.path.join(self.root, "zh")
        config = os.path.join(home, "atticdb.json")
        sock = os.path.join(self.root, "s")
        log = os.path.join(self.root, "serve.log")
        # The server also makes `<socket>.lock`, so leave room for it.
        if len(os.fsencode(sock + ".lock")) + 1 > SUN_PATH_BYTES:
            raise RunError(f"the temp dir {os.path.dirname(self.root)} is too long for the atticdb "
                           "socket path. Set TMPDIR to a shorter dir")
        os.mkdir(home, 0o700)
        os.mkdir(os.path.join(home, "data"), 0o700)
        env = child_env(home)
        argv = [self.atticdb, "init", "--config", config, "--tenant", TENANT]
        r = Runner().run(argv, env=env, root=self.root, cwd=self.root, timeout=CHILD_TIMEOUT)
        if r.code != 0:
            why = "not found" if r.missing else "timed out" if r.timed_out else f"exit {r.code}"
            raise RunError(f"atticdb init failed ({why}): {self._excerpt(r.stderr)}")
        token = self._token(config)
        guard.check_env(env, self.root)  # The store's home must be under its own root.
        argv = [self.atticdb, "serve", "--config", config, "--data-dir", os.path.join(home, "data"),
                "--listen", "none", "--unix-socket", sock]
        with open(log, "ab") as out:
            self.process = subprocess.Popen(argv, cwd=self.root, env=env, stdin=subprocess.DEVNULL,
                                            stdout=out, stderr=out, start_new_session=True)
        self._wait_serving(log, sock)
        self.client = arms.atticdb_client.Client(f"unix://{sock}", token)

    def _excerpt(self, raw: bytes | str) -> str:
        text = raw.decode("utf-8", "replace") if isinstance(raw, bytes) else raw
        return guard.excerpt(text, {self.root or "": "<store>"}) or "no output"

    @staticmethod
    def _token(config: str) -> str:
        try:
            with open(config, encoding="utf-8") as f:
                tenants = json.load(f)["tenants"]
            return next(t["token"] for t in tenants if t["name"] == TENANT)
        except (OSError, ValueError, KeyError, TypeError, StopIteration) as e:
            raise RunError(f"the store's config has no token for tenant {TENANT}: {type(e).__name__}") from e

    def _wait_serving(self, log: str, sock: str) -> None:
        deadline = time.monotonic() + START_TIMEOUT
        while True:
            with open(log, encoding="utf-8", errors="replace") as f:
                text = f.read()
            line = next((ln for ln in text.splitlines(keepends=True)
                         if ln.startswith(SERVING) and ln.endswith("\n")), None)
            if line:
                fields = dict(f.split("=", 1) for f in line.split()[2:] if "=" in f)
                if fields.get("unix") != sock:
                    raise RunError(f"atticdb serve is on {fields.get('unix')!r}, not the store's socket")
                return
            if self.process.poll() is not None:
                raise RunError(f"atticdb serve exited with code {self.process.returncode}: "
                               f"{self._excerpt(text)}")
            if time.monotonic() > deadline:
                raise RunError(f"atticdb serve did not start in {START_TIMEOUT:.0f} s")
            time.sleep(0.01)

    def status(self) -> str:
        """The server's state, for an error message."""
        if self.process is None:
            return "The server never started."
        if (code := self.process.poll()) is not None:
            return f"The server exited with code {code}."
        return "The server is still running."

    def close(self) -> None:
        """Stops the server and deletes the root. Safe to call more than once."""
        if self.client is not None:
            self.client.close()
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(STOP_TIMEOUT)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if self.root is not None:
            shutil.rmtree(self.root, ignore_errors=True)


def ingest(client, conv: data.Conversation) -> int:
    """Appends every turn in order, in batches. Returns the batch count.

    Each note is `"<speaker>: <text>"` with meta `{"dia_id": ...}`, agent locomo, session sample_id.
    """
    batches = 0

    def flush(batch: list[dict]) -> None:
        nonlocal batches
        try:
            receipt = client.append(arms.AGENT, conv.sample_id, batch)
        except arms.atticdb_client.Error as e:
            raise RunError(f"ingest failed for {conv.sample_id}, batch {batches}: {e}") from e
        if receipt.get("deduplicated"):
            raise RunError(f"ingest for {conv.sample_id}, batch {batches} was deduplicated in a fresh store")
        seqs = receipt.get("seqs")
        if seqs is not None and len(seqs) != len(batch):
            raise RunError(f"ingest for {conv.sample_id}, batch {batches} wrote {len(seqs)} of {len(batch)} turns")
        batches += 1

    batch: list[dict] = []
    size = 0
    for t in conv.turns:
        note = {"role": ROLE, "content": t.content, "meta": {"dia_id": t.dia_id}}
        n = len(json.dumps(note).encode("utf-8"))
        if batch and (len(batch) >= BATCH_TURNS or size + n > BATCH_BYTES):
            flush(batch)
            batch, size = [], 0
        batch.append(note)
        size += n
    if batch:
        flush(batch)
    return batches


# ---- one conversation ----

def _call(name: str, client, corpus: arms.Corpus, q: data.Question, seed: int) -> arms.Result:
    if name == "atticdb-question":
        return arms.atticdb_question(client, corpus, q)
    if name == "atticdb-1word":
        return arms.atticdb_1word(client, corpus, q)
    if name == "atticdb-2words":
        return arms.atticdb_2words(client, corpus, q)
    if name == "fts5-bm25":
        return arms.fts5_bm25(corpus, q)
    if name == "random":
        return arms.random_turns(corpus, q, seed)
    raise ValueError(f"unknown arm {name!r}")


def score_row(sample_id: str, q: data.Question, arm: str, ids: list[str], query: str,
              ms: float | None, contents: Mapping[str, str]) -> dict:
    """1 CSV row: the top 20 ids, and recall, hit and tokens at each k."""
    kept = list(ids[:TOP])
    row = {"sample_id": sample_id, "q_index": q.index, "category": q.category, "arm": arm,
           "query": query, "retrieved": kept, "n_evidence": len(q.evidence), "ms": ms}
    for k in KS:
        row[f"recall@{k}"] = score.recall(kept, q.evidence, k)
    for k in KS:
        row[f"hit@{k}"] = score.hit(kept, q.evidence, k)
    for k in KS:
        row[f"tokens@{k}"] = score.approx_tokens(contents[i] for i in kept[:k])
    return row


def run_conversation(atticdb: str, conv: data.Conversation, seed: int,
                     log: Callable[[str], None]) -> list[dict]:
    corpus = arms.Corpus.build(conv)
    rows: list[dict] = []
    try:
        with Store(atticdb) as store:
            start = time.monotonic()
            batches = ingest(store.client, conv)
            ingest_s = time.monotonic() - start
            for q in conv.questions:
                for name in ARMS:
                    try:
                        ids, query, ms = _call(name, store.client, corpus, q, seed)
                    except arms.SearchError as e:
                        raise RunError(f"search failed in {conv.sample_id} qa[{q.index}], arm {name}: "
                                       f"{e}. {store.status()}") from e
                    rows.append(score_row(conv.sample_id, q, name, ids, query, ms, corpus.contents))
            if store.process.poll() is not None:
                raise RunError(f"the atticdb server died during {conv.sample_id}. {store.status()}")
    finally:
        corpus.close()
    log(f"{conv.sample_id}: {len(conv.turns)} turns in {batches} append{'s' if batches != 1 else ''} "
        f"({ingest_s:.1f} s), {len(conv.questions)} questions")
    return rows


# ---- outputs ----

def _num(x: float) -> str:
    return repr(float(x))


def csv_text(rows: Iterable[Mapping]) -> str:
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(CSV_COLUMNS)
    for r in rows:
        out = []
        for col in CSV_COLUMNS:
            v = r[col]
            if col == "retrieved":
                v = " ".join(v)
            elif col == "ms":
                v = "" if v is None else f"{v:.3f}"
            elif col.startswith("hit@"):
                v = int(bool(v))
            elif col.startswith(("recall@", "tokens@")):
                v = _num(v)
            out.append(v)
        w.writerow(out)
    return buf.getvalue()


def _pct(x: float) -> str:
    return f"{x * 100:.1f}%"


def _metric_rows(rows: Iterable[Mapping]) -> list[dict]:
    """What score.summarize takes: category and the metrics, no ids, no text, no latency."""
    keys = [f"{m}@{k}" for m in ("recall", "hit", "tokens") for k in KS]
    return [{"category": r["category"], **{k: r[k] for k in keys}} for r in rows]


def summary_markdown(rows: list[Mapping], meta: Mapping) -> str:
    """The summary: numbers and ids only, never question or turn text."""
    by_arm = {arm: [r for r in rows if r["arm"] == arm] for arm in ARMS}
    sums = {arm: score.summarize(_metric_rows(rs)) for arm, rs in by_arm.items()}
    scored = len({(r["sample_id"], r["q_index"]) for r in rows})
    pinned = "the pinned file" if meta["pinned"] else "not the pinned file, given by --dataset"
    lines = [
        f"# LoCoMo retrieval, {meta['date']}",
        "",
        "Numbers only. results.csv holds the per-question rows and stays out of the repo.",
        "",
        "## Run",
        "",
        f"- Date: {meta['date']}",
        f"- Dataset SHA-256: `{meta['sha256']}` ({pinned})",
        f"- atticdb: {meta['atticdb_version']}",
        f"- Machine: {meta['platform']}, CPUs: {meta['cpus']}",
        f"- Seed: {meta['seed']}",
        f"- Conversations: {meta['conversations']}",
        f"- Questions scored: {scored}",
        f"- Full context: ≈ {meta['full_context']:,.0f} tokens (bytes/4) per conversation, on average",
        "",
        "## Excluded and normalized",
        "",
        "| count | n |",
        "|---|---|",
        *(f"| {k} | {meta['excluded'][k]} |" for k in data.COUNT_KEYS),
        "",
        "## Overall",
        "",
        "Recall is averaged over questions. Hit is the share of questions with any evidence id found.",
        "",
        "| arm | k | n | recall | hit | ≈ tokens (bytes/4) |",
        "|---|---|---|---|---|---|",
    ]
    for arm in ARMS:
        s = sums[arm]
        for k in KS:
            if s["n"]:
                lines.append(f"| {arm} | {k} | {s['n']} | {_pct(s[f'recall@{k}'])} | "
                             f"{_pct(s[f'hit@{k}'])} | {s[f'tokens@{k}']:,.0f} |")
    k = SUMMARY_K
    lines += ["", f"## By category, k = {k}", "",
              f"| arm | category | n | recall@{k} | hit@{k} | ≈ tokens@{k} |", "|---|---|---|---|---|---|"]
    for arm in ARMS:
        for name, c in sums[arm]["by_category"].items():
            lines.append(f"| {arm} | {name} | {c['n']} | {_pct(c[f'recall@{k}'])} | "
                         f"{_pct(c[f'hit@{k}'])} | {c[f'tokens@{k}']:,.0f} |")
    lines += ["", "## Latency", "",
              "Wall time of the search call only, in ms. Ingestion and server start are excluded.",
              "Arms 1 to 3 go over the Unix socket. FTS5 runs in process. Random runs no search.",
              "", "| arm | searches | p50 | p95 |", "|---|---|---|---|"]
    for arm in ARMS:
        ms = [r["ms"] for r in by_arm[arm] if r["ms"] is not None]
        if ms:
            lines.append(f"| {arm} | {len(ms)} | {statistics.median(ms):.2f} | {score.percentile(ms, 95):.2f} |")
        else:
            lines.append(f"| {arm} | 0 | n/a | n/a |")
    return "\n".join(lines) + "\n"


def _write(out: Path, files: Mapping[str, str]) -> None:
    """Writes each file to a temp name, then moves them all into place."""
    out.mkdir(parents=True, exist_ok=True)
    tmp = {}
    try:
        for name, text in files.items():
            fd, path = tempfile.mkstemp(prefix=f".{name}.", dir=out)
            with os.fdopen(fd, "w", encoding="utf-8", newline="") as f:
                f.write(text)
            tmp[name] = path
        for name, path in tmp.items():
            os.replace(path, out / name)
    finally:
        for path in tmp.values():
            if os.path.exists(path):
                os.unlink(path)


# ---- the run ----

@dataclass
class Config:
    atticdb: str
    out: Path
    cache_dir: Path = field(default_factory=lambda: data.DEFAULT_CACHE_DIR)
    conversations: int | None = None
    seed: int = 0
    dataset: Path | None = None


def atticdb_version(atticdb: str) -> str:
    # A throwaway home, so the real one is never in reach.
    try:
        with tempfile.TemporaryDirectory(prefix="zlr-v-") as root:
            r = subprocess.run([atticdb, "--version"], cwd=root, env=child_env(os.path.join(root, "zh")),
                               stdin=subprocess.DEVNULL, capture_output=True, timeout=CHILD_TIMEOUT)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise RunError(f"can't run atticdb at {atticdb}: {e}") from e
    text = r.stdout.decode("utf-8", "replace").strip()
    if r.returncode != 0 or not text:
        raise RunError(f"atticdb --version failed with exit {r.returncode}")
    return text.splitlines()[0]


def _stderr(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def _dataset(cfg: Config) -> tuple[Path, str]:
    """The dataset path and its SHA-256. A download or cache with a wrong hash raises RunError."""
    try:
        path = Path(cfg.dataset) if cfg.dataset is not None else data.fetch(data.URL, data.SHA256, cfg.cache_dir)
        return path, data._sha256(path)
    except (ValueError, OSError) as e:
        raise RunError(f"can't get the dataset: {e}") from e


def run(cfg: Config, log: Callable[[str], None] = _stderr) -> dict[str, Path]:
    """Runs the benchmark. Returns the output paths. Raises RunError on any failure."""
    path, sha = _dataset(cfg)  # Before any other work.
    try:
        with open(path, encoding="utf-8") as f:
            samples = json.load(f)
        if not isinstance(samples, list):
            raise ValueError("the dataset must be a JSON list")
        if cfg.conversations is not None:
            samples = samples[:cfg.conversations]
        convs = data.parse(samples)
        excluded = data.count_excluded(samples)
    except (OSError, ValueError, KeyError, TypeError) as e:
        raise RunError(f"bad dataset {path}: {type(e).__name__}: {e}") from e
    version = atticdb_version(cfg.atticdb)
    if not arms.fts5_available():
        raise RunError("this Python's sqlite3 has no FTS5")
    log(f"{len(convs)} conversations, {sum(len(c.questions) for c in convs)} questions, {version}")

    rows: list[dict] = []
    for conv in convs:
        rows += run_conversation(cfg.atticdb, conv, cfg.seed, log)

    contexts = [score.approx_tokens(t.content for t in c.turns) for c in convs]
    meta = {"sha256": sha, "pinned": sha == data.SHA256, "atticdb_version": version,
            "platform": platform.platform(), "cpus": os.cpu_count(),
            "date": datetime.date.today().isoformat(), "seed": cfg.seed, "conversations": len(convs),
            "full_context": statistics.fmean(contexts) if contexts else 0.0, "excluded": excluded}
    out = Path(cfg.out)
    try:
        _write(out, {"results.csv": csv_text(rows), "summary.md": summary_markdown(rows, meta)})
    except OSError as e:
        raise RunError(f"can't write the outputs to {out}: {e}") from e
    return {"results": out / "results.csv", "summary": out / "summary.md"}
