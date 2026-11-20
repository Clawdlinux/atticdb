"""Runs scripts/demo_correction.py --fast against ATTICDB_BIN, a real atticdb binary.

The demo makes its own temp home under /tmp, so the real HOME is never touched.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

DEMO = Path(__file__).resolve().parents[1] / "demo_correction.py"
BIN = os.environ.get("ATTICDB_BIN", "")

needs_bin = pytest.mark.skipif(not BIN or not os.path.isfile(BIN),
                               reason="ATTICDB_BIN is unset or not a file")

CORRECTION = "Use pnpm, not npm. npm broke the lockfile on 2026-11-02."


def run_demo(binary, timeout):
    env = dict(os.environ, ATTICDB_BIN=os.path.abspath(binary))
    return subprocess.run([sys.executable, str(DEMO), "--fast"], env=env, capture_output=True,
                          text=True, timeout=timeout, check=False)


@needs_bin
def test_demo_tells_the_story():
    result = run_demo(BIN, 120)
    assert result.returncode == 0, result.stdout + result.stderr
    lines = result.stdout.splitlines()
    assert '    "Use npm." (by agent-a, lsn 2)' in lines
    assert "    Refused. Nothing was written. The server says:" in lines
    assert any(line.startswith("    | conflict: this fact changed") for line in lines)
    assert f"    | {CORRECTION}" in lines
    assert f"    lsn 3 by agent-a: {CORRECTION}" in lines
    assert "    lsn 2 by agent-a: Use npm." in lines
    assert "agent-b:" not in result.stdout
    assert lines[-3] == f'Current value: "{CORRECTION}"'
    assert lines[-2] == "B's stale write never landed."
    assert lines[-1] == "B saw the correction before any write of its own could land."


def test_demo_fails_when_the_binary_is_not_atticdb(tmp_path):
    fake = tmp_path / "atticdb"
    fake.write_text("#!/bin/sh\nexit 0\n")
    fake.chmod(0o755)
    result = run_demo(fake, 60)
    assert result.returncode == 1
    assert "FAIL:" in result.stderr
