"""The fixture repo: a small ledger module with passing stdlib tests, in 1 commit.

No file here may hold a case's evidence tokens (test_no_reader_prompt_leaks_evidence).
"""

from __future__ import annotations

import os
import subprocess

from ..setup_eval.agents import GIT_TIMEOUT, git_env

LEDGER_PY = '''"""Ledger rows: headers, export and parsing."""

import json


def pack_header(n):
    """Return the header bytes for a batch of n rows."""
    return json.dumps({"version": 1, "count": n}).encode("utf-8")


def export(rows):
    lines = []
    for row in rows:
        lines.append(json.dumps(row, sort_keys=True))
    return "\\n".join(lines)


def parse(line):
    """Parse 1 exported line back into a row dict."""
    row = json.loads(line)
    if not isinstance(row, dict):
        raise ValueError("not a row: " + line)
    return row
'''

TEST_LEDGER_PY = '''import unittest

import ledger

ROWS = [{"id": 1, "amount": 250, "memo": "rent"}, {"id": 2, "amount": 75, "memo": "tea"}]


class LedgerTest(unittest.TestCase):
    def test_header_is_bytes_and_counts_rows(self):
        self.assertIsInstance(ledger.pack_header(2), bytes)
        self.assertNotEqual(ledger.pack_header(2), ledger.pack_header(3))

    def test_export_round_trips(self):
        lines = ledger.export(ROWS).splitlines()
        self.assertEqual([ledger.parse(line) for line in lines], ROWS)

    def test_export_keeps_order_on_big_input(self):
        rows = [{"id": i, "amount": i * 3} for i in range(2000)]
        lines = ledger.export(rows).splitlines()
        self.assertEqual([ledger.parse(line) for line in lines], rows)

    def test_parse_rejects_non_rows(self):
        with self.assertRaises(ValueError):
            ledger.parse("[1, 2]")


if __name__ == "__main__":
    unittest.main()
'''

README_MD = "# ledger\n"

FILES = {"ledger.py": LEDGER_PY, "test_ledger.py": TEST_LEDGER_PY, "README.md": README_MD}


def write_fixture(repo: str) -> None:
    """Write the files into `repo`, which must be missing or empty, and commit them once."""
    os.makedirs(repo, exist_ok=True)
    if os.listdir(repo):
        raise FileExistsError(f"fixture repo is not empty: {repo}")
    for name, body in FILES.items():
        with open(os.path.join(repo, name), "w") as f:
            f.write(body)
    env = {"PATH": os.environ.get("PATH", os.defpath), **git_env()}
    for args in (["init", "-q"], ["add", *FILES], ["commit", "-q", "--no-verify", "-m", "Initial commit"]):
        subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True, timeout=GIT_TIMEOUT)
