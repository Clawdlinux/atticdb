"""A labeling sheet: sampled references from the public sets, for a human to judge.

    python3 -m research.ctxcache label-sample --dataset openhands --n 100 --seed 87

It draws references from the same sample as the analysis, stratified by subtype. Each
row shows a short excerpt of the referenced view, the later action, and empty `needed`
(yes, no or partial) and `note` columns. It writes the CSV outside the repo, by default
under the cache directory. Never commit a sheet.
"""

from __future__ import annotations

import argparse
import csv
import os
import random
import sys
from collections import Counter

from . import hf, openhands, swe_agent
from .guard import inside, outside
from .run import positive
from .trace import REF_KINDS

ADAPTERS = {"swe-agent": swe_agent, "openhands": openhands}
FIELDS = ["id", "dataset", "row", "step", "subtype", "view_tokens", "accesses_since_view", "view_excerpt",
          "action", "needed", "note"]


def parse(argv) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python3 -m research.ctxcache label-sample", description=__doc__.split("\n\n")[0])
    p.add_argument("--dataset", required=True, choices=sorted(ADAPTERS))
    p.add_argument("--n", type=positive, default=100, help="references to draw")
    p.add_argument("--seed", type=int, default=87)
    p.add_argument("--sample", type=positive, default=1000, help="the analysis sample to draw from")
    p.add_argument("--block", type=positive, help="as in the analysis. Default: 10 for openhands, else 1")
    p.add_argument("--cache-dir", default=hf.CACHE)
    p.add_argument("--offline", action="store_true", help="use cached rows only")
    p.add_argument("--out", help="the CSV to write. Default: <cache-dir>/labels/<dataset>-n<N>-seed<S>.csv")
    a = p.parse_args(argv)
    a.block = a.block or hf.DATASETS[a.dataset].block
    a.out = a.out or os.path.join(a.cache_dir, "labels", f"{a.dataset}-n{a.n}-seed{a.seed}.csv")
    for flag, path in (("--out", a.out), ("--cache-dir", a.cache_dir)):
        if inside(path):
            p.error(f"{flag} must point outside the repo, so sheets and rows are never committed")
    return a


def excerpt(text: str, head: int = 3, tail: int = 3, width: int = 120) -> str:
    """The first and last lines, each cut to `width` characters."""
    cut = [line if len(line) <= width else line[: width - 3] + "..." for line in text.split("\n")]
    if len(cut) <= head + tail:
        return "\n".join(cut)
    return "\n".join([*cut[:head], f"[{len(cut) - head - tail} more lines]", *cut[-tail:]])


def allocate(n: int, sizes: dict[str, int]) -> dict[str, int]:
    """Equal shares of n across the subtypes present. A small subtype passes its shortfall on."""
    quota = {k: 0 for k in sizes}
    open_ = [k for k in REF_KINDS if sizes.get(k, 0) > 0]
    left = n
    while left > 0 and open_:
        share = max(1, left // len(open_))
        for k in list(open_):
            take = min(share, sizes[k] - quota[k], left)
            quota[k] += take
            left -= take
            if quota[k] == sizes[k]:
                open_.remove(k)
            if left == 0:
                break
    return quota


def collect(a: argparse.Namespace) -> dict[str, list[dict]]:
    """Every reference in the sample, by subtype, with the text a labeler needs."""
    fetcher = hf.Fetcher(a.dataset, a.cache_dir, offline=a.offline)
    groups: dict[str, list[dict]] = {k: [] for k in REF_KINDS}
    for offset, row in hf.sampled_rows(a.dataset, a.sample, a.seed, fetcher, Counter(), a.block):
        acc, info = ADAPTERS[a.dataset].trajectory(row, f"{a.dataset}:{offset}", keep_text=True)
        shown: dict[str, int] = {}  # object -> its newest tool output. References do not count.
        for t, x in enumerate(acc):
            if x.ref and t in info["notes"]:
                note = info["notes"][t]
                groups[x.sub].append({
                    "dataset": a.dataset, "row": offset, "step": t, "subtype": x.sub, "view_tokens": x.tokens,
                    "accesses_since_view": t - shown[x.obj], "view_excerpt": excerpt(note["view"]),
                    "action": note["action"] if len(note["action"]) <= 1500 else note["action"][:1500] + " [cut]",
                })
            if not x.ref:
                shown[x.obj] = t
    return groups


def main(argv=None) -> int:
    a = parse(argv)
    try:
        groups = collect(a)
        quota = allocate(a.n, {k: len(v) for k, v in groups.items()})
        rng = random.Random(a.seed)
        rows = []
        for k in REF_KINDS:
            picked = rng.sample(groups[k], quota.get(k, 0))
            rows += sorted(picked, key=lambda r: (r["row"], r["step"]))
        # Again at write time, after symlinks resolve: a labels folder may link into the repo.
        out = outside(os.path.abspath(a.out), "the sheet")
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=FIELDS)
            w.writeheader()
            for i, r in enumerate(rows, 1):
                w.writerow({"id": i, **r, "needed": "", "note": ""})
    except (hf.FetchError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    drawn = ", ".join(f"{k} {quota[k]} of {len(groups[k])}" for k in REF_KINDS if groups[k])
    print(f"wrote {len(rows)} references to {a.out}: {drawn}")
    return 0
