"""Run the cross-harness handoff eval, plan it, or check the eval logins.

    python3 -m research.handoff_eval --check
    python3 -m research.handoff_eval --regrade research/handoff_eval/results/<date>.csv
    python3 -m research.handoff_eval --claude-model ID --codex-model ID --dry-run
    python3 -m research.handoff_eval --directions claude-code:copilot,copilot:claude-code \
        --claude-model ID --copilot-model ID --dry-run
    python3 -m research.handoff_eval --cases H1,H2,H3,H4,H5,H6 --arms native,files,atticdb,oracle \\
        --directions claude-code:codex,codex:claude-code --reps 1 --claude-model ID --codex-model ID \\
        --max-sessions 90 --out research/handoff_eval/results/<date>.csv

README.md has the details.
"""

from __future__ import annotations

import argparse
import csv
import datetime
import os
import shutil
import sys
import tempfile

from ..setup_eval import agents, guard
from ..setup_eval import cli as setup_cli
from ..setup_eval.harness import HarnessError, Step
from ..setup_eval.proc import Runner
from . import arms, results
from .cases import CASES
from .run import (SECURE_STORAGE_ENV, Budget, HandoffConfig, describe, env_for, prepare_config_dirs, run_handoff,
                  vscode_mcp_file)

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
DEFAULT_DIRECTIONS = "claude-code:codex,codex:claude-code"
MODEL_FLAGS = arms.MODEL_FLAGS
CROSS = "claude-code:codex, codex:claude-code, claude-code:copilot or copilot:claude-code"


def _list(text: str, allowed, what: str) -> list[str]:
    out = []
    for part in text.split(","):
        part = part.strip()
        if part not in allowed:
            raise argparse.ArgumentTypeError(f"unknown {what} {part!r}; use {','.join(allowed)}")
        if part not in out:
            out.append(part)
    return out


def case_list(text: str) -> list[str]:
    return _list(text.upper(), CASES, "case")


def arm_list(text: str) -> list[str]:
    return _list(text.lower(), arms.ARMS, "arm")


def direction_list(text: str) -> list[tuple[str, str]]:
    out = []
    for part in text.split(","):
        writer, sep, reader = part.strip().partition(":")
        if not sep or writer not in agents.ALL_HARNESSES or reader not in agents.ALL_HARNESSES:
            raise argparse.ArgumentTypeError(f"bad direction {part.strip()!r}; use writer:reader with "
                                             f"{', '.join(agents.ALL_HARNESSES)}")
        if writer == reader:
            raise argparse.ArgumentTypeError(f"same-harness direction {part.strip()!r} is not supported: the pilot "
                                             f"is cross-harness only. Use {CROSS}")
        if (writer, reader) not in out:
            out.append((writer, reader))
    return out


def positive_float(text: str) -> float:
    try:
        n = float(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a number: {text!r}") from None
    if n <= 0:
        raise argparse.ArgumentTypeError(f"must be above 0: {text!r}")
    return n


def matrix(a: argparse.Namespace) -> list[tuple[int, str, str, str, str]]:
    """Every run in batch order, as (rep, writer, reader, case, arm)."""
    return [(n, w, r, c, arm) for n in range(1, a.reps + 1) for w, r in a.directions for c in a.cases
            for arm in a.arms]


def session_count(runs) -> tuple[int, int]:
    """(writer sessions, reader sessions) for a list of runs."""
    writers = sum(arms.writer_sessions(CASES[c], arm) for _, _, _, c, arm in runs)
    return writers, len(runs)


def parse(argv) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python3 -m research.handoff_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--check", action="store_true",
                   help="check that the 2 eval config dirs are safe and logged in, then exit")
    p.add_argument("--regrade", metavar="CSV",
                   help="grade the answer replies stored in a results CSV again with the current graders, and "
                        "print the summary. Runs no agent and writes nothing")
    p.add_argument("--cases", type=case_list, default=list(CASES), help="comma list of " + ",".join(CASES))
    p.add_argument("--arms", type=arm_list, default=list(arms.ARMS), help="comma list of " + ",".join(arms.ARMS))
    p.add_argument("--directions", type=direction_list, default=direction_list(DEFAULT_DIRECTIONS),
                   help=f"comma list of writer:reader. Default {DEFAULT_DIRECTIONS}")
    p.add_argument("--reps", type=setup_cli.positive, default=1, help="runs per case, arm and direction")
    p.add_argument("--claude-model", default="", help="the model id passed to claude as --model. Required when a "
                   "direction uses claude-code")
    p.add_argument("--codex-model", default="", help="the model id passed to codex exec as -m. Required when a "
                   "direction uses codex")
    p.add_argument("--copilot-model", default="", help="the model id passed to copilot as --model. Required when a "
                   "direction uses copilot")
    p.add_argument("--max-sessions", type=setup_cli.positive,
                   help="refuse a batch of more agent sessions than this, writers and readers both. "
                        "Required for a real run")
    p.add_argument("--atticdb", default=os.path.join(REPO_ROOT, "target", "debug", "atticdb"),
                   help="the atticdb binary. Default target/debug/atticdb")
    today = datetime.date.today().isoformat()
    p.add_argument("--out", default=os.path.join(HERE, "results", f"{today}.csv"),
                   help="the CSV rows are appended to. The summary goes next to it, as .md")
    p.add_argument("--agent-timeout", type=positive_float, default=agents.AGENT_TIMEOUT,
                   help=f"seconds per agent session. Default {agents.AGENT_TIMEOUT:g}")
    p.add_argument("--dry-run", action="store_true", help="print every planned session and the total, run nothing")
    p.add_argument("--keep-api-keys", action="store_true",
                   help=f"pass {agents.account_names()} on to the children")
    a = p.parse_args(argv)
    if a.check and a.regrade:
        p.error("--check and --regrade don't go together")
    if a.check or a.regrade:
        return a
    a.models = {"claude-code": a.claude_model, "codex": a.codex_model, "copilot": a.copilot_model}
    used = {h for d in a.directions for h in d}
    for h in agents.ALL_HARNESSES:
        if h in used and not a.models[h]:
            p.error(f"{MODEL_FLAGS[h]} is required: the directions use {h}, and no model is picked silently")
    a.runs = matrix(a)
    a.writer_sessions, a.reader_sessions = session_count(a.runs)
    a.total = a.writer_sessions + a.reader_sessions
    if not a.dry_run:
        if a.max_sessions is None:
            p.error(f"--max-sessions is required. This batch is {a.total} agent sessions")
        if a.total > a.max_sessions:
            p.error(f"the batch is {a.total} agent sessions ({a.writer_sessions} writer, {a.reader_sessions} "
                    f"reader), over --max-sessions {a.max_sessions}. Raise it to allow the batch")
    a.atticdb = os.path.abspath(a.atticdb)
    return a


def config(a: argparse.Namespace) -> HandoffConfig:
    home = guard.user_home()
    claude_dir, codex_dir = guard.eval_dirs(home)
    return HandoffConfig(atticdb=a.atticdb, home=home, claude_dir=claude_dir, codex_dir=codex_dir,
                         tmp_parent=tempfile.gettempdir(), agent_timeout=a.agent_timeout,
                         keep_api_keys=a.keep_api_keys, models=dict(a.models))


def dry_run(a: argparse.Namespace, cfg: HandoffConfig, out=print) -> int:
    guard.check_config_dirs(cfg.claude_dir, cfg.codex_dir, cfg.home)  # paths only
    out(f"Dry run: nothing is started. Rows would go to {a.out}.")
    out(f"<root> and <harness-root> are 2 new temp dirs per run in {cfg.tmp_parent}. Env: as setup_eval, by name "
        f"only: the inherited env without ATTICDB_*, GIT_*, COPILOT_*, CLAUDE_CONFIG_DIR, CODEX_HOME"
        f"{'' if cfg.keep_api_keys else ' and the credential and provider vars'}, then HOME, ATTICDB_HOME "
        f"(<root>/zh), CLAUDE_CONFIG_DIR (<harness-root>/claude), CODEX_HOME (<harness-root>/codex), COPILOT_HOME "
        f"(<harness-root>/copilot), and {SECURE_STORAGE_ENV} ({cfg.claude_dir}), so Claude finds the eval login in "
        "the Keychain. No GitHub token is set: Copilot logs in from the OS keychain.")
    used = {h for d in a.directions for h in d}
    models = ", ".join(f"{h}={m}" for h, m in a.models.items() if m and h in used)
    out(f"Models: {models}.")
    for i, (n, writer, reader, cid, arm) in enumerate(a.runs, 1):
        case = CASES[cid]
        k = arms.planned_sessions(case, arm)
        out(f"run {i}/{len(a.runs)}: rep {n} {cid} {arm} {writer} to {reader}: {k} agent session"
            f"{'' if k == 1 else 's'}")
        for line in describe(cfg, case, arm, writer, reader):
            out(line)
    for h in agents.ALL_HARNESSES:
        if h in used:
            out(f"Overrides for {h}: " + "; ".join(agents.OVERRIDES[h]))
    if "codex" in used:
        out("Outside the atticdb arm, codex runs without the -c fact_set override: with no atticdb server "
            "configured, it alone makes Codex refuse its config.")
    flags = {"claude-code": "claude gets --model <id> last", "copilot": "copilot gets --model <id> last",
             "codex": "codex exec gets -m <id> just before the prompt"}
    out("Model flags: " + "; ".join(flags[h] for h in agents.ALL_HARNESSES if h in used) + ".")
    out(f"Total: {a.total} agent sessions ({a.writer_sessions} writer, {a.reader_sessions} reader) "
        f"in {len(a.runs)} runs.")
    if a.max_sessions is not None and a.total > a.max_sessions:
        out(f"A real run would be refused: over --max-sessions {a.max_sessions}.")
    return 0


def file_stat(path: str) -> tuple | None:
    """(size, mtime_ns) of 1 file, or None when it is missing or can't be read. Stats only."""
    try:
        st = os.stat(path)
    except OSError:
        return None
    return st.st_size, st.st_mtime_ns


def summary_path(csv_path: str) -> str:
    return os.path.splitext(csv_path)[0] + ".md"


def batch(a: argparse.Namespace, cfg: HandoffConfig, out=print, runner: Runner | None = None) -> int:
    guard.check_config_dirs(cfg.claude_dir, cfg.codex_dir, cfg.home)
    used = {h for d in a.directions for h in d}
    for folder, harness in ((cfg.claude_dir, "claude-code"), (cfg.codex_dir, "codex")):
        if harness in used and not os.path.isdir(folder):
            raise guard.Refused(f"{folder} is missing. Log in once: "
                                f"{agents.login_hint(harness, cfg.claude_dir, cfg.codex_dir)}, then run --check")
    if not (os.path.isfile(cfg.atticdb) and os.access(cfg.atticdb, os.X_OK)):
        raise guard.Refused(f"--atticdb {cfg.atticdb} is not an executable file. Build it: cargo build -p atticdb")
    try:
        results.check_header(a.out)
    except ValueError as e:
        raise guard.Refused(str(e)) from None
    cfg.budget = Budget(a.max_sessions)
    data_dirs = guard.watched_data_dirs(cfg.base_env)
    before = {label: guard.snapshot(path) for label, path in data_dirs}
    config_before = guard.config_snapshot(cfg.home)
    vscode = vscode_mcp_file(cfg)
    vscode_before = file_stat(vscode)
    runner = runner or Runner()
    done, today, stopped = [], datetime.date.today().isoformat(), False
    try:
        for n, writer, reader, cid, arm in a.runs:
            case = CASES[cid]
            need = arms.planned_sessions(case, arm)
            if not cfg.budget.fits(need):
                out(f"Stopped: {cid} {arm} needs {need} more agent sessions, and {cfg.budget.used} of "
                    f"--max-sessions {cfg.budget.limit} are used.")
                stopped = True
                break
            res = run_handoff(cfg, runner, case, arm, writer, reader, n)
            done.append(res)
            results.append(a.out, [results.row(today, res)])
            verdict = (f"error in {res.error_stage}" if res.errored
                       else "pass" if res.passed else "fail")
            out(f"[{len(done)}/{len(a.runs)}] rep {n} {cid} {arm} {writer} to {reader}: {verdict} "
                f"(captured={results.flag(res.captured) or '-'}, delivered={results.flag(res.delivered) or '-'})")
            if res.error_stage == "budget":
                stopped = True
                break
    finally:
        lines = results.summary(done)
        with open(summary_path(a.out), "w") as f:
            f.write("\n".join(lines) + "\n")
        for line in lines:
            out(line)
        out("")
        out(f"Rows: {a.out}. Summary: {summary_path(a.out)}.")
        out(f"Agent sessions started: {cfg.budget.used} of --max-sessions {cfg.budget.limit}.")
        out(f"Every child had ATTICDB_HOME set under its own run root: {runner.children} children checked.")
        for label, path in data_dirs:
            setup_cli.warn(out, f"files in {label}", guard.compare(before[label], guard.snapshot(path)),
                           f"{label[0].upper()}{label[1:]} did not change (size and mtime of each file).")
        setup_cli.warn(out, "entries at the top level of ~/.claude*, ~/.codex or ~/.copilot",
                       guard.compare(config_before, guard.config_snapshot(cfg.home)),
                       "The top level of ~/.claude*, ~/.codex and ~/.copilot did not change (names, sizes and mtimes).")
        vscode_after = file_stat(vscode)
        setup_cli.warn(out, "files: VS Code's user mcp.json", [vscode] if vscode_after != vscode_before else [],
                       "VS Code's user mcp.json did not change (size and mtime).")
    return 1 if stopped or any(r.errored for r in done) else 0


def check_run_dirs(cfg: HandoffConfig, out=print) -> int:
    """Logs in from a run's own config dirs, as a run would: an empty CLAUDE_CONFIG_DIR with the
    Keychain lookup on the eval Claude dir, a CODEX_HOME with the eval auth.json copied in, and an
    empty COPILOT_HOME. Prints only whether each is logged in, and Copilot's version."""
    root = tempfile.mkdtemp(prefix="zhe-check-", dir=cfg.tmp_parent)
    runner, ok = Runner(), True
    try:
        try:
            prepare_config_dirs(cfg, root)  # a check has no repo, so 1 temp dir serves as both roots
        except HarnessError as e:
            out(f"run dirs: {e}")
            return 1
        for harness in agents.ALL_HARNESSES:
            step = Step("check", agents.check_argv(harness), root, agents.CHECK_TIMEOUT)
            r = runner.run(step.argv, env=env_for(cfg, root, step, root), root=root, cwd=root, timeout=step.timeout)
            if r.code == 0 and harness == "copilot":
                version = (r.stdout.decode("utf-8", "replace").strip().splitlines() or ["?"])[0]
                out(f"copilot: {guard.excerpt(version.rstrip('.'), None, 80)} runs from a run's own COPILOT_HOME. Login not "
                    "checked: Copilot CLI has no status command that makes no model call. Its login is in the OS "
                    "keychain, which every COPILOT_HOME shares")
            elif r.code == 0:
                out(f"{harness}: logged in from a run's own config dir")
            else:
                why = "not on PATH" if r.missing else "no answer" if r.timed_out else f"exit {r.code}"
                out(f"{harness}: not logged in from a run's own config dir ({why})")
                ok = False
    finally:
        shutil.rmtree(root, ignore_errors=True)
    return 0 if ok else 1


def regrade(path: str, out=print) -> int:
    """Re-grade a results CSV and print what changed and the summary. Exit 2 if it can't be read."""
    try:
        rg = results.regrade(results.read(path))
    except (OSError, ValueError, KeyError, csv.Error) as e:
        print(f"regrade: {path}: {e}", file=sys.stderr)
        return 2
    for line in results.regrade_report(path, rg):
        out(line)
    return 0


def main(argv=None) -> int:
    a = parse(sys.argv[1:] if argv is None else list(argv))
    if a.regrade:
        return regrade(a.regrade)
    try:
        if a.check:
            code = setup_cli.check()
            home = guard.user_home()
            claude_dir, codex_dir = guard.eval_dirs(home)
            cfg = HandoffConfig(atticdb="", home=home, claude_dir=claude_dir, codex_dir=codex_dir,
                                tmp_parent=tempfile.gettempdir())
            return max(code, check_run_dirs(cfg))
        cfg = config(a)
        if a.dry_run:
            return dry_run(a, cfg)
        return batch(a, cfg)
    except guard.Refused as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2
