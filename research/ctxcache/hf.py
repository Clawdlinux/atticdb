"""Sampled rows from the Hugging Face datasets-server, cached on disk.

Requests are sequential. There is a sleep between calls and backoff on 429 and 5xx.
The request count and the wall time are both capped. Rows are cached under
~/.cache/zerodb-ctxcache/<dataset>/ and never in the repo.
"""

from __future__ import annotations

import gzip
import http.client
import json
import os
import random
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from dataclasses import dataclass

API = "https://datasets-server.huggingface.co/rows"
CACHE = os.path.expanduser("~/.cache/zerodb-ctxcache")
HEADERS = {"User-Agent": "zerodb-ctxcache/0.1 (research; https://github.com/Clawdlinux/zerodb)"}


@dataclass(frozen=True)
class Dataset:
    repo: str
    url: str
    rows: int  # the row count when sampled. It pins the sample.
    keep: tuple[str, ...]  # columns cached per row
    cite: str
    block: int = 1  # consecutive rows per stratum in the published sample


DATASETS = {
    "swe-agent": Dataset(
        "nebius/SWE-agent-trajectories",
        "https://huggingface.co/datasets/nebius/SWE-agent-trajectories",
        80_036,
        ("instance_id", "model_name", "target", "exit_status", "trajectory"),
        "Nebius, SWE-agent-trajectories, CC-BY-4.0",
    ),
    "openhands": Dataset(
        "nebius/SWE-rebench-openhands-trajectories",
        "https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories",
        67_074,
        ("trajectory_id", "instance_id", "repo", "exit_status", "resolved", "trajectory"),
        "Trofimova et al., OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct, "
        "Nebius blog, 2025, CC-BY-4.0",
        block=10,  # its consecutive rows are distinct instances, and the API allows few calls
    ),
}


class FetchError(RuntimeError):
    """The API failed after retries, or the request or time budget ran out."""


def sample_offsets(total: int, size: int, seed: int, block: int = 1) -> list[int]:
    """`size // block` equal strata. In each, 1 random run of `block` consecutive rows.
    With block 1 that is 1 random row per stratum."""
    strata = max(1, min(size, total) // block)
    rng = random.Random(seed)
    out: list[int] = []
    for k in range(strata):
        lo, hi = k * total // strata, (k + 1) * total // strata
        start = lo + rng.randrange(max(1, hi - lo - block + 1))
        out += range(start, min(start + block, hi))
    return out


class Fetcher:
    """Fetches rows and caches each one, including the fact that it came back truncated."""

    def __init__(
        self,
        name: str,
        cache_dir: str = CACHE,
        sleep: float = 1.6,
        retries: int = 6,
        max_requests: int = 2_500,
        deadline: float = 5_400.0,
        offline: bool = False,
        urlopen=urllib.request.urlopen,
        clock=time.monotonic,
        pause=time.sleep,
        log=sys.stderr,
    ):
        self.ds = DATASETS[name]
        self.dir = os.path.join(cache_dir, name)
        self.sleep, self.retries, self.offline = sleep, retries, offline
        self.max_requests, self.deadline = max_requests, deadline
        self.urlopen, self.clock, self.pause, self.log = urlopen, clock, pause, log
        self.start = clock()
        self.requests = 0
        self.warned = False

    def _path(self, offset: int) -> str:
        return os.path.join(self.dir, f"{offset}.json.gz")

    def row(self, offset: int, length: int = 1) -> tuple[dict | None, str]:
        """(row, "ok"), or (None, "truncated") when the trajectory cell came back truncated.
        A miss fetches `length` rows from `offset` in 1 request and caches them all."""
        if not os.path.exists(self._path(offset)):
            if self.offline:
                raise FetchError(f"row {offset} is not cached and --offline is set")
            os.makedirs(self.dir, exist_ok=True)
            for cached in self._fetch(offset, length):
                tmp = f"{self._path(cached['offset'])}.{os.getpid()}.tmp"
                with gzip.open(tmp, "wt", encoding="utf-8") as f:
                    json.dump(cached, f)
                os.replace(tmp, self._path(cached["offset"]))
        with gzip.open(self._path(offset), "rt", encoding="utf-8") as f:
            cached = json.load(f)
        if cached["truncated"]:
            return None, "truncated"
        return cached["row"], "ok"

    def _fetch(self, offset: int, length: int) -> list[dict]:
        query = urllib.parse.urlencode(
            {"dataset": self.ds.repo, "config": "default", "split": "train", "offset": offset, "length": length}
        )
        body = self._get(f"{API}?{query}")
        total = body.get("num_rows_total")
        if total != self.ds.rows and not self.warned:
            print(f"warning: {self.ds.repo} has {total} rows, the sample assumes {self.ds.rows}", file=self.log)
            self.warned = True
        rows = body.get("rows") or []
        if [r.get("row_idx") for r in rows] != list(range(offset, offset + length)):
            raise FetchError(f"rows {offset}+{length}: got {len(rows)} rows, not the ones asked for")
        out = []
        for r in rows:
            truncated = "trajectory" in (r.get("truncated_cells") or [])
            row = None if truncated else {k: r["row"].get(k) for k in self.ds.keep}
            out.append({"offset": r["row_idx"], "truncated": truncated, "row": row})
        return out

    def _get(self, url: str) -> dict:
        backoff = 1.0
        for attempt in range(self.retries + 1):
            if self.requests >= self.max_requests:
                raise FetchError(f"request budget of {self.max_requests} spent")
            if self.clock() - self.start > self.deadline:
                raise FetchError(f"time budget of {self.deadline:.0f} s spent")
            if self.requests:
                self.pause(self.sleep)
            self.requests += 1
            try:
                with self.urlopen(urllib.request.Request(url, headers=HEADERS), timeout=60) as resp:
                    return json.load(resp)
            except urllib.error.HTTPError as e:
                if e.code != 429 and e.code < 500:
                    raise FetchError(f"HTTP {e.code} for {url}") from e
                wait = _retry_after(e) or backoff
                reason = f"HTTP {e.code}"
                if e.code == 429 and attempt == 0:
                    # Slow down for good, once per episode, to stay under the limit.
                    self.sleep = min(max(self.sleep, 0.5) * 1.25, 10.0)
                    reason += f", gap now {self.sleep:.1f} s"
            except (OSError, ValueError, http.client.HTTPException) as e:
                # Resets, timeouts, cut bodies and bad JSON are all worth a retry.
                wait, reason = backoff, type(e).__name__
            if attempt == self.retries:
                raise FetchError(f"{reason} after {self.retries} retries: {url}")
            wait = min(wait, 60.0)
            print(f"retry {attempt + 1} in {wait:.0f} s: {reason}", file=self.log)
            self.pause(wait)
            backoff = min(backoff * 2, 60.0)
        raise AssertionError("unreachable")


def _retry_after(e: urllib.error.HTTPError) -> float | None:
    try:
        return float(e.headers.get("Retry-After", ""))
    except (TypeError, ValueError):
        return None


def sampled_rows(name: str, sample: int, seed: int, fetcher: Fetcher, counts: Counter, block: int = 1):
    """Yields (offset, row) over the sample, 1 request per run of uncached rows.
    Counts "ok" and "truncated" rows in `counts`."""
    offsets = sample_offsets(DATASETS[name].rows, sample, seed, block)
    for i, offset in enumerate(offsets):
        run = 1
        while i + run < len(offsets) and run < block and offsets[i + run] == offset + run:
            run += 1
        row, status = fetcher.row(offset, run)
        counts[status] += 1
        if row is not None:
            yield offset, row
