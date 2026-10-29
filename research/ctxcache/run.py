"""Build traces, replay every policy at every budget, and write aggregate results.

    python3 -m research.ctxcache --dataset swe-agent --sample 1000
    python3 research/ctxcache/run.py --dataset copilot
    python3 -m research.ctxcache label-sample --dataset openhands --n 100 --seed 87

README.md lists the flags and the trace format.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from collections import Counter

if __package__ in (None, ""):  # run as a script: make `research` importable
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

from research.ctxcache import copilot, hf, openhands, report, swe_agent, trace  # noqa: E402
from research.ctxcache.stats import measure, reuse_distances  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
DEFAULT_BUDGETS = "1k,2k,4k,8k,16k,32k,64k,128k"
TITLES = {
    "swe-agent": "SWE-agent trajectories (nebius)",
    "openhands": "OpenHands trajectories (nebius, SWE-rebench)",
    "copilot": "Our Copilot CLI sessions",
    "trace": "Trace file",
}


def positive(text: str) -> int:
    try:
        n = int(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not an integer: {text!r}") from None
    if n < 1:
        raise argparse.ArgumentTypeError(f"must be at least 1: {text!r}")
    return n


def budget_list(text: str) -> list[int]:
    """A comma list such as 1k,2k,4096. 1k is 1,000 tokens."""
    out = set()
    for part in text.split(","):
        part = part.strip().lower()
        try:
            n = int(float(part[:-1]) * 1000) if part.endswith("k") else int(part)
        except ValueError:
            raise argparse.ArgumentTypeError(f"not a budget: {part!r}") from None
        if n < 1:
            raise argparse.ArgumentTypeError(f"a budget must be at least 1 token: {part!r}")
        out.add(n)
    return sorted(out)


def parse(argv=None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python3 -m research.ctxcache", description=__doc__.split("\n\n")[0])
    p.add_argument("--dataset", required=True, choices=sorted(TITLES))
    p.add_argument("--sample", type=positive, default=1000, help="rows to sample (public sets)")
    p.add_argument("--block", type=positive, help="consecutive rows per stratum. Default: 10 for openhands, else 1")
    p.add_argument("--seed", type=int, default=87, help="sample and bootstrap seed")
    p.add_argument("--budgets", type=budget_list, default=DEFAULT_BUDGETS, help="comma list, 1k = 1,000 tokens")
    p.add_argument("--out", default=os.path.join(HERE, "results"), help="directory for the CSV and markdown")
    p.add_argument("--cache-dir", default=hf.CACHE, help="where downloaded rows are cached (outside the repo)")
    p.add_argument("--sleep", type=float, default=1.6, help="seconds between API calls")
    p.add_argument("--max-requests", type=positive, default=2500)
    p.add_argument("--deadline", type=float, default=5400.0, help="seconds of fetching before giving up")
    p.add_argument("--offline", action="store_true", help="use cached rows only")
    p.add_argument("--input", help="trace JSONL for --dataset trace")
    p.add_argument("--copilot-root", default=copilot.ROOT)
    p.add_argument("--export-trace", help="also write the hashed trace here (outside the repo)")
    p.add_argument("--bootstrap", type=int, default=1000, help="cluster bootstrap resamples, 0 for none")
    p.add_argument("--sensitivity-budgets", type=budget_list, default="4k,8k",
                   help="budgets for the reference proxy check")
    a = p.parse_args(argv)
    a.block = a.block or (hf.DATASETS[a.dataset].block if a.dataset in hf.DATASETS else 1)
    if a.dataset == "trace" and not a.input:
        p.error("--dataset trace needs --input")
    if a.block > a.sample:
        p.error("--block must not exceed --sample")
    if a.bootstrap < 0:
        p.error("--bootstrap must be 0 or more")
    if a.export_trace and _inside(a.export_trace, REPO):
        p.error("--export-trace must point outside the repo, so traces are never committed")
    if _inside(a.cache_dir, REPO):
        p.error("--cache-dir must point outside the repo, so downloaded rows are never committed")
    return a


def budget_text(budgets: list[int]) -> str:
    return ",".join(f"{b // 1000}k" if b % 1000 == 0 else str(b) for b in budgets)


def _inside(path: str, root: str) -> bool:
    """True if `path` is `root` or lies below it. It compares files, not strings, so an
    alias still counts: another case on a case-insensitive disk, a firmlink such as
    /System/Volumes/Data/..., or a symlink. Parts that do not exist yet are skipped."""
    p = os.path.realpath(path)
    while True:
        try:
            if os.path.samefile(p, root):
                return True
        except OSError:
            pass  # not created yet
        parent = os.path.dirname(p)
        if parent == p:
            return False
        p = parent


def source(a: argparse.Namespace, skipped: Counter):
    """Yields (trajectory id, cluster, accesses, info)."""
    if a.dataset in ("swe-agent", "openhands"):
        module = swe_agent if a.dataset == "swe-agent" else openhands
        fetcher = hf.Fetcher(a.dataset, a.cache_dir, a.sleep, max_requests=a.max_requests,
                             deadline=a.deadline, offline=a.offline)
        counts: Counter = Counter()
        offsets = hf.sample_offsets(hf.DATASETS[a.dataset].rows, a.sample, a.seed, a.block)
        block = {o: offsets[i - i % a.block] for i, o in enumerate(offsets)}  # offset -> its block's first row
        for offset, row in hf.sampled_rows(a.dataset, a.sample, a.seed, fetcher, counts, a.block):
            traj = f"{a.dataset}:{offset}"
            acc, info = module.trajectory(row, traj)
            # The bootstrap resamples the units drawn: blocks of rows, or else instances.
            cluster = f"block:{block[offset]}" if a.block > 1 else (info.get("cluster") or traj)
            yield traj, cluster, acc, info
        skipped["truncated"] += counts["truncated"]
    elif a.dataset == "copilot":
        for traj, cluster, acc, info in copilot.traces(a.copilot_root):
            if acc is None:
                skipped["fork"] += 1
                continue
            yield traj, cluster, acc, info
    else:
        with open(a.input) as f:
            for traj, acc in trace.load(f).items():
                yield traj, traj, acc, {}


def describe(a: argparse.Namespace, s: dict) -> dict:
    cmd = f"python3 -m research.ctxcache --dataset {a.dataset}"
    if a.dataset in ("swe-agent", "openhands"):
        cmd += f" --sample {a.sample} --seed {a.seed}" + (f" --block {a.block}" if a.block > 1 else "")
    if a.budgets != budget_list(DEFAULT_BUDGETS):
        cmd += f" --budgets {budget_text(a.budgets)}"
    if a.sensitivity_budgets != budget_list("4k,8k"):
        cmd += f" --sensitivity-budgets {budget_text(a.sensitivity_budgets)}"
    if a.dataset == "trace":
        cmd += f" --input {os.path.basename(a.input)}"
    meta = {"title": TITLES[a.dataset], "command": cmd, "source": [], "extra_rows": [], "scaffold": [], "notes": [],
            "measures": {}}
    if a.dataset in hf.DATASETS:
        ds = hf.DATASETS[a.dataset]
        drawn = len(hf.sample_offsets(ds.rows, a.sample, a.seed, a.block))
        meta["measures"]["rows_drawn"] = drawn
        unit = "instances" if a.block == 1 else f"the sampled blocks of {a.block} rows"
        meta["notes"].append(f"95% intervals resample {unit}, 1,000 times.")
        meta["source"] = [
            f"Data: [{ds.repo}]({ds.url}). {ds.cite}. Only aggregate counts appear here.",
            f"Sample: {drawn:,} rows. The {ds.rows:,} rows are cut into {max(1, a.sample // a.block)} equal "
            f"strata, and each gives {'1 random row' if a.block == 1 else f'1 random run of {a.block} rows'}. "
            f"Seed {a.seed}.",
        ]
    elif a.dataset == "copilot":
        meta["source"] = ["Data: our own Copilot CLI session logs, main agent only. Aggregates only."]
        meta["notes"].append(f"Descriptive only: {s['trajectories']} sessions from 1 user. The intervals resample "
                             "these sessions. They do not describe other users.")
    else:
        meta["source"] = [f"Data: {os.path.basename(a.input)}."]
    held = lambda x: report.num(x)  # noqa: E731
    meta["scaffold"] = [
        f"- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: "
        f"{report.pct(s['last5_token_miss'])}% of proxy-access tokens missed "
        f"({report.pct(s['last5_miss'])}% of proxy accesses), {held(s['last5_mean_held'])} tokens held on average.",
        f"- Keep everything: no misses, {held(s['all_mean_held'])} tokens held on average.",
    ]
    if a.dataset == "openhands":
        meta["extra_rows"].append(f"Rows with signs of condensed history | {s['condensed']}")
        meta["scaffold"].append("- OpenHands keeps everything unless its condenser runs. "
                                f"Rows with signs of condensation: {s['condensed']}.")
    if a.dataset == "copilot":
        meta["extra_rows"].append(f"Compactions | {s['compactions']}")
        meta["scaffold"].append(
            f"- Observed: {s['reread_same_window']} re-reads in the same compaction window "
            f"({held(s['reread_same_window_tokens'])} tokens), {s['reread_after_compaction']} after a "
            f"compaction ({held(s['reread_after_compaction_tokens'])} tokens)."
        )
    return meta


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else list(argv)
    if argv[:1] == ["label-sample"]:
        from research.ctxcache import label  # imports run, so import it late

        return label.main(argv[1:])
    a = parse(argv)
    try:
        return run(a)
    except (hf.FetchError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


def run(a: argparse.Namespace) -> int:
    sizes = a.budgets
    skipped: Counter = Counter()
    recs, distances = [], []
    export = open(a.export_trace, "w") if a.export_trace else None
    start = time.time()
    try:
        for traj, cluster, acc, info in source(a, skipped):
            trace.check(acc)
            if export:
                trace.dump(traj, acc, export)
            rec = measure(acc, info, sizes, a.sensitivity_budgets)
            rec["cluster"] = cluster
            recs.append(rec)
            distances += reuse_distances(acc)
            if len(recs) % 100 == 0:
                print(f"{len(recs)} trajectories, {time.time() - start:.0f} s", file=sys.stderr)
    finally:
        if export:
            export.close()
    if not recs:
        print("no trajectories", file=sys.stderr)
        return 1
    summary = report.summarize(recs, distances, skipped)
    rows = report.table(recs, sizes, a.bootstrap, a.seed)
    sens = report.sensitivity(recs, a.sensitivity_budgets, a.bootstrap, a.seed)
    paths = report.write(a.out, a.dataset, rows, sens, summary, describe(a, summary))
    print(f"{a.dataset}: {summary['trajectories']} trajectories, {summary['accesses']:,} accesses, "
          f"skipped {dict(skipped) or 0}, {time.time() - start:.0f} s")
    for path in paths:
        print(f"wrote {os.path.relpath(path)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
