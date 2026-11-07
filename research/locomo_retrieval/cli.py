"""Score AtticDB search on LoCoMo's evidence labels. No answering model is involved.

    python3 -m research.locomo_retrieval --atticdb target/release/atticdb --out /tmp/locomo-out

README.md has the details.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

from . import data, run


def positive(text: str) -> int:
    try:
        n = int(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not an integer: {text!r}") from None
    if n < 1:
        raise argparse.ArgumentTypeError(f"must be at least 1: {text!r}")
    return n


def parse(argv) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python3 -m research.locomo_retrieval",
                                description=__doc__.split("\n\n")[0])
    p.add_argument("--atticdb", required=True, help="the atticdb binary to serve each store with")
    p.add_argument("--out", required=True, type=Path,
                   help="the dir for results.csv and summary.md. Nothing else is written there")
    p.add_argument("--cache-dir", type=Path, default=data.DEFAULT_CACHE_DIR,
                   help="where the hash-checked dataset is cached (default: %(default)s)")
    p.add_argument("--conversations", type=positive, help="run only the first N conversations")
    p.add_argument("--seed", type=int, default=0, help="the random arm's seed (default: 0)")
    p.add_argument("--dataset", type=Path,
                   help="a local LoCoMo-shaped JSON file. It skips the download, and its hash is only reported")
    a = p.parse_args(argv)
    # Store children run from their own temp dir, so the binary path must be absolute.
    a.atticdb = os.path.abspath(shutil.which(a.atticdb) or a.atticdb)
    return a


def main(argv=None) -> int:
    a = parse(argv)
    cfg = run.Config(atticdb=a.atticdb, out=a.out, cache_dir=a.cache_dir,
                     conversations=a.conversations, seed=a.seed, dataset=a.dataset)
    try:
        paths = run.run(cfg)
    except run.RunError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    print(f"wrote {paths['results']} and {paths['summary']}")
    return 0
