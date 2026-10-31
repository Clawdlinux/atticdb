#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

work="scripts/.og-work-$$"
mkdir -p "$work/tmp"
trap 'rm -rf "$work"' EXIT

python3 - "$work" <<'PY'
import os
from pathlib import Path
import struct
import subprocess
import sys

work = Path(sys.argv[1]).resolve()
chrome = os.environ.get("CHROME_BIN", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
output = Path("public/og.png")
subprocess.run(
    [
        chrome,
        "--headless",
        "--disable-gpu",
        "--disable-background-networking",
        "--no-first-run",
        "--no-default-browser-check",
        "--allow-file-access-from-files",
        f"--user-data-dir={work / 'profile'}",
        "--force-device-scale-factor=1",
        "--hide-scrollbars",
        "--window-size=1200,630",
        "--virtual-time-budget=3000",
        f"--screenshot={output}",
        Path("scripts/og.html").resolve().as_uri(),
    ],
    env={**os.environ, "TMPDIR": str(work / "tmp")},
    check=True,
    timeout=60,
)
header = output.read_bytes()[:24]
if header[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", header[16:24]) != (1200, 630):
    raise SystemExit("Expected a 1200x630 PNG")
print(f"Created {output} (1200x630)")
PY
