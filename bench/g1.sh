#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Override G1_TURNS for local smoke runs. The gate trace defaults to 1M turns.
# Set G1_NVME=1 only on Linux NVMe. Other runs print a local measurement, not the verdict.
# G1_SYMMETRIC=1 (default) runs 2 recent-data readers on bare, Tape and Engine.
# G1_SYMMETRIC=0 runs readers on Engine only, as in traces before D74.
export G1_TURNS="${G1_TURNS:-1000000}"
export G1_BATCH="${G1_BATCH:-4}"
cargo bench -p zerodb-core --bench g1
