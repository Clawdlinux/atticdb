#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Override G1_TURNS for local smoke runs. The gate trace defaults to 1M turns.
export G1_TURNS="${G1_TURNS:-1000000}"
export G1_BATCH="${G1_BATCH:-4}"
cargo bench -p zerodb-core --bench g1
