"""Child processes: each one checked by the env guard, bounded by a timeout, in its own
process group, and killed with that group on timeout."""

from __future__ import annotations

import os
import signal
import subprocess
import time
from dataclasses import dataclass

from .guard import check_env


@dataclass
class Result:
    code: int | None  # None when it timed out or could not start
    stdout: bytes
    stderr: bytes
    wall_s: float
    timed_out: bool = False
    missing: bool = False  # the program is not on PATH


class Runner:
    """Starts children. Counts each one, so the summary can say how many ran and that every
    one had `ATTICDB_HOME` under its run root."""

    def __init__(self) -> None:
        self.children = 0

    def run(self, argv: list[str], *, env: dict, root: str, cwd: str, timeout: float,
            stdin: bytes | None = None) -> Result:
        check_env(env, root)  # raises before anything starts
        if not timeout or timeout <= 0:
            raise ValueError("every child needs a timeout")
        start = time.monotonic()
        try:
            p = subprocess.Popen(argv, cwd=cwd, env=env, start_new_session=True,
                                 stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        except FileNotFoundError:
            return Result(None, b"", b"", 0.0, missing=True)
        self.children += 1
        timed_out = False
        try:
            out, err = p.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            _kill_group(p.pid)
            try:
                out, err = p.communicate(timeout=10)
            except subprocess.TimeoutExpired:  # a grandchild that left the group holds a pipe
                for pipe in (p.stdout, p.stderr):
                    if pipe:
                        pipe.close()
                p.wait(timeout=10)
                out, err = b"", b""
        except BaseException:
            _kill_group(p.pid)
            p.wait()
            raise
        finally:
            # Anything the child left in its group, such as an MCP adapter, goes too. A server
            # AtticDB started sits in its own group and is stopped through its LOCK file.
            _kill_group(p.pid)
        wall = time.monotonic() - start
        return Result(None if timed_out else p.returncode, out, err, wall, timed_out)


def _kill_group(pgid: int) -> None:
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
