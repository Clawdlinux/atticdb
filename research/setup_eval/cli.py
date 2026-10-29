"""Run the zerodb setup eval headless, or check the eval logins.

    python3 -m research.setup_eval check
    python3 -m research.setup_eval run --zerodb target/debug/zerodb --dry-run
    python3 -m research.setup_eval run --harness both --scenarios S1,S2,S3,S4,B1,B2 --runs 1 \\
        --zerodb target/debug/zerodb

README.md has the details.
"""

from __future__ import annotations

import argparse
import datetime
import os
import shutil
import sys
import tempfile

from . import agents, guard, results
from .harness import Config, describe, run_one
from .proc import Runner
from .scenarios import SCENARIOS

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_MAX_RUNS = 12
BUDGET_CAP = 36  # the plan's whole budget


def positive(text: str) -> int:
    try:
        n = int(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not an integer: {text!r}") from None
    if n < 1:
        raise argparse.ArgumentTypeError(f"must be at least 1: {text!r}")
    return n


def scenario_list(text: str) -> list[str]:
    out = []
    for part in text.split(","):
        part = part.strip().upper()
        if part not in SCENARIOS:
            raise argparse.ArgumentTypeError(f"unknown scenario {part!r}; use {','.join(SCENARIOS)}")
        if part not in out:
            out.append(part)
    return out


def parse(argv) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python3 -m research.setup_eval", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("check", help="check that the 2 eval config dirs exist and are logged in")
    r = sub.add_parser("run", help="run a batch")
    r.add_argument("--harness", choices=["claude-code", "codex", "both"], default="both")
    r.add_argument("--scenarios", type=scenario_list, default=list(SCENARIOS),
                   help="comma list of S1,S2,S3,S4,B1,B2")
    r.add_argument("--runs", type=positive, default=1, help="runs per harness and scenario")
    r.add_argument("--zerodb", required=True, help="the zerodb binary to set up and seed with")
    today = datetime.date.today().isoformat()
    r.add_argument("--out", default=os.path.join(HERE, "results", f"{today}.csv"),
                   help="the CSV rows are appended to")
    r.add_argument("--dry-run", action="store_true", help="print every command and env name, run nothing")
    r.add_argument("--keep-api-keys", action="store_true",
                   help=f"pass {agents.account_names()} on to the children, so they may use "
                        "another account, provider or endpoint than the eval logins")
    r.add_argument("--max-runs", type=positive, default=DEFAULT_MAX_RUNS,
                   help=f"refuse a batch larger than this. At most {BUDGET_CAP}")
    a = p.parse_args(argv)
    if a.command == "run":
        if a.max_runs > BUDGET_CAP:
            p.error(f"--max-runs is at most {BUDGET_CAP}, the plan's whole budget")
        a.harnesses = list(agents.HARNESSES) if a.harness == "both" else [a.harness]
        a.total = len(a.harnesses) * len(a.scenarios) * a.runs
        if a.total > a.max_runs:
            p.error(f"the batch is {a.total} runs, over --max-runs {a.max_runs}. Each run is 1 headless "
                    "agent session. Raise --max-runs to allow it")
        a.zerodb = os.path.abspath(a.zerodb)
    return a


def config(zerodb: str, keep_api_keys: bool = False) -> Config:
    home = guard.user_home()
    claude_dir, codex_dir = guard.eval_dirs(home)
    return Config(zerodb=zerodb, home=home, claude_dir=claude_dir, codex_dir=codex_dir,
                  tmp_parent=tempfile.gettempdir(), keep_api_keys=keep_api_keys)


def check(out=print) -> int:
    """Runs `claude auth status` and `codex login status` on the eval dirs, 20 s each, and
    prints only whether each is logged in, never their output."""
    home = guard.user_home()
    claude_dir, codex_dir = guard.eval_dirs(home)
    guard.check_config_dirs(claude_dir, codex_dir, home)
    runner, ok = Runner(), True
    for harness, folder in (("claude-code", claude_dir), ("codex", codex_dir)):
        hint = agents.login_hint(harness, claude_dir, codex_dir)
        if not os.path.isdir(folder):
            out(f"{harness}: {folder} is missing. Log in once: {hint}")
            ok = False
            continue
        root = tempfile.mkdtemp(prefix="zse-check-")
        try:
            # Without credential or provider vars, so the status shows the eval login.
            env = agents.child_env(dict(os.environ), home=home, zerodb_home=os.path.join(root, "zh"),
                                   claude_dir=claude_dir, codex_dir=codex_dir)
            r = runner.run(agents.check_argv(harness), env=env, root=root, cwd=root,
                           timeout=agents.CHECK_TIMEOUT)
        finally:
            shutil.rmtree(root, ignore_errors=True)
        if r.missing:
            out(f"{harness}: {agents.check_argv(harness)[0]} is not on PATH")
        elif r.timed_out:
            out(f"{harness}: no answer in {agents.CHECK_TIMEOUT:g} s")
        elif r.code == 0:
            out(f"{harness}: logged in ({folder})")
            continue
        else:
            out(f"{harness}: not logged in (exit {r.code}). Log in once: {hint}")
        ok = False
    return 0 if ok else 1


def dry_run(a: argparse.Namespace, cfg: Config, out=print) -> int:
    guard.check_config_dirs(cfg.claude_dir, cfg.codex_dir, cfg.home)  # paths only
    out(f"Dry run: {a.total} runs, nothing is started. Rows would go to {a.out}.")
    out(f"<root> is a new temp dir per run in {cfg.tmp_parent}.")
    keys = "" if cfg.keep_api_keys else ", the credential and provider vars below"
    out(f"env base, by name only: the inherited env without ZERODB_*, CLAUDE_CONFIG_DIR, CODEX_HOME{keys}, "
        f"then HOME, ZERODB_HOME (<root>/zh), CLAUDE_CONFIG_DIR and CODEX_HOME (the eval dirs under "
        f"~/{guard.EVAL_DIR}).")
    if cfg.keep_api_keys:
        out("Credential and provider vars: kept (--keep-api-keys). The agents may not use the eval logins.")
    else:
        stripped = agents.stripped_keys(cfg.base_env)
        out(f"Credential and provider vars stripped from every child: "
            f"{', '.join(stripped) if stripped else 'none were set'}. Checked: {agents.account_names()}.")
    for n in range(1, a.runs + 1):
        for harness in a.harnesses:
            for sid in a.scenarios:
                out(f"run {n} {harness} {sid}")
                for line in describe(cfg, harness, SCENARIOS[sid]):
                    out(line)
    for harness in a.harnesses:
        out(f"Overrides for {harness}: " + "; ".join(agents.OVERRIDES[harness]))
    return 0


def warn(out, what: str, changed: list[str], same: str) -> None:
    """A warning naming up to 5 changes, or `same`. Never a failure: another session, such as
    the controller's, may write there."""
    if not changed:
        out(same)
        return
    shown = ", ".join(changed[:5]) + (", ..." if len(changed) > 5 else "")
    out(f"WARNING: {len(changed)} {what} changed during the batch ({shown}). "
        "Another session, such as the controller's, may have written them.")


def batch(a: argparse.Namespace, cfg: Config, out=print, environ=None) -> int:
    guard.check_config_dirs(cfg.claude_dir, cfg.codex_dir, cfg.home)
    for folder, harness in ((cfg.claude_dir, "claude-code"), (cfg.codex_dir, "codex")):
        if harness in a.harnesses and not os.path.isdir(folder):
            raise guard.Refused(f"{folder} is missing. Log in once: "
                                f"{agents.login_hint(harness, cfg.claude_dir, cfg.codex_dir)}, then run check")
    if not (os.path.isfile(a.zerodb) and os.access(a.zerodb, os.X_OK)):
        raise guard.Refused(f"--zerodb {a.zerodb} is not an executable file. Build it: cargo build -p zerodb")
    environ = cfg.base_env if environ is None else environ
    data_dirs = guard.watched_data_dirs(environ)
    before = {label: guard.snapshot(path) for label, path in data_dirs}
    config_before = guard.config_snapshot(cfg.home)
    runner, done, today = Runner(), [], datetime.date.today().isoformat()
    try:
        for n in range(1, a.runs + 1):
            for harness in a.harnesses:
                for sid in a.scenarios:
                    res = run_one(cfg, runner, harness, SCENARIOS[sid], n)
                    done.append(res)
                    results.append(a.out, [results.row(today, res)])
                    verdict = "error" if res.errored else ("pass" if res.passed else "fail")
                    out(f"[{len(done)}/{a.total}] {harness} {sid} run {n}: {verdict}")
    finally:
        for line in results.summary(done):
            out(line)
        out("")
        out(f"Rows: {a.out}")
        out(f"Every child had ZERODB_HOME set under its own run root: {runner.children} children checked.")
        for label, path in data_dirs:
            warn(out, f"files in {label}", guard.compare(before[label], guard.snapshot(path)),
                 f"{label[0].upper()}{label[1:]} did not change (size and mtime of each file).")
        warn(out, "entries at the top level of ~/.claude*, ~/.codex or ~/.copilot",
             guard.compare(config_before, guard.config_snapshot(cfg.home)),
             "The top level of ~/.claude*, ~/.codex and ~/.copilot did not change (names, sizes and mtimes).")
    return 1 if any(r.errored for r in done) else 0


def main(argv=None) -> int:
    a = parse(sys.argv[1:] if argv is None else list(argv))
    try:
        if a.command == "check":
            return check()
        cfg = config(a.zerodb, a.keep_api_keys)
        if a.dry_run:
            return dry_run(a, cfg)
        return batch(a, cfg)
    except guard.Refused as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2
