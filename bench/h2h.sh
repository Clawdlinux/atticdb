#!/usr/bin/env bash
# Head-to-head: durable 256-byte appends, acknowledged only after fsync, closed-loop clients.
# Linux only. Usage: bench/h2h.sh <directory on the disk under test> [label]
# Builds pinned Redis, Valkey, DiceDB and memtier_benchmark into $H2H_TOOLS on first run.
set -euo pipefail
DISK=$(realpath "${1:?usage: bench/h2h.sh <dir on the disk under test> [label]}")
LABEL=${2:-$(basename "$DISK")}
cd "$(dirname "$0")/.."
REPS=${H2H_REPS:-3}
WRITERS=${H2H_WRITERS:-"1 4 16"}
PER=${H2H_PER_WRITER:-5000}
SIZE=256
TOOLS=${H2H_TOOLS:-$HOME/h2h-tools}
OUT=bench/h2h-$LABEL.tsv
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fetch() {
    [ -d "$TOOLS/$1" ] || git clone -q --depth 1 --branch "$3" "https://github.com/$2" "$TOOLS/$1"
}
mkdir -p "$TOOLS"
fetch redis redis/redis 8.10.2
fetch valkey valkey-io/valkey 9.1.2
fetch dicedb dicedb/dicedb 1.0.0
fetch memtier RedisLabs/memtier_benchmark 2.5.2
for name in redis valkey dicedb; do
    [ -x "$TOOLS/$name/src/$name-server" ] || make -C "$TOOLS/$name" -j"$(nproc)" >/dev/null 2>&1
done
if [ ! -x "$TOOLS/memtier/memtier_benchmark" ]; then
    (cd "$TOOLS/memtier" && autoreconf -ivf && ./configure && make -j"$(nproc)") >/dev/null 2>&1
fi
cargo build -q --release -p zerodb
ZDB=target/release/zerodb
MEMTIER=$TOOLS/memtier/memtier_benchmark

# Waits for a server's socket. Fails with its log if the server exits first.
wait_socket() {
    until [ -S "$2" ]; do
        if ! kill -0 "$1" 2>/dev/null; then
            echo "server exited before creating $2:" >&2 && cat "$3" >&2
            return 1
        fi
        sleep 0.1
    done
}

# Prints `turns_per_sec p50_us p99_us p999_us max_us` from `key=value` lines on stdin.
pick() {
    awk -F= '{v[$1]=$2} END {print v["turns_per_sec"], v["p50_us"], v["p99_us"], v["p999_us"], v["max_us"]}'
}

zerodb() {
    local data=$DISK/zerodb
    rm -rf "$data" "$TMP/c.json"
    (umask 077 && "$ZDB" init --config "$TMP/c.json" --tenant h2h >"$TMP/token")
    "$ZDB" serve --config "$TMP/c.json" --data-dir "$data" --listen none >"$TMP/serve.log" 2>&1 &
    local pid=$! status=0
    wait_socket $pid "$data/zerodb.sock" "$TMP/serve.log"
    "$ZDB" load --url "unix://$data/zerodb.sock" --token-file "$TMP/token" \
        --writers "$1" --turns $(($1 * PER)) --size $SIZE >"$TMP/load" || status=$?
    kill $pid && wait $pid || true
    # A failed run must not become a row.
    [ $status = 0 ] && pick <"$TMP/load"
}

# Redis-protocol servers: AOF with appendfsync always, so each reply waits for its fsync.
resp() {
    local data=$DISK/$2 sock=$TMP/$2.sock
    rm -rf "$data" && mkdir -p "$data"
    "$TOOLS/$2/src/$2-server" --port 0 --unixsocket "$sock" --dir "$data" --save "" \
        --appendonly yes --appendfsync always --logfile "$data/log" &
    local pid=$! status=0
    wait_socket $pid "$sock" "$data/log"
    rm -f "$TMP/m.json"
    "$MEMTIER" --unix-socket="$sock" --protocol=redis --clients="$1" --threads=1 --pipeline=1 \
        --requests="$PER" --data-size=$SIZE --command="RPUSH __key__ __data__" \
        --command-key-pattern=R --key-prefix=s --key-minimum=1 --key-maximum=64 \
        --hide-histogram --print-percentiles=50,99,99.9 --json-out-file="$TMP/m.json" \
        >"$TMP/memtier.log" 2>&1 || status=$?
    kill $pid && wait $pid || true
    [ $status = 0 ] || { cat "$TMP/memtier.log" >&2 && return $status; }
    python3 - "$TMP/m.json" <<'PY'
import json, sys
totals = json.load(open(sys.argv[1]))["ALL STATS"]["Totals"]
lat = totals["Percentile Latencies"]
print(totals["Ops/sec"], *(lat[k] * 1000 for k in ("p50.00", "p99.00", "p99.90")),
      totals["Max Latency"] * 1000)
PY
}

sqlite() {
    rm -f "$DISK"/h2h.db*
    python3 bench/h2h_sqlite.py "$DISK/h2h.db" "$1" "$PER" $SIZE | pick
}

{
    echo "# $(uname -sr) $(findmnt -no SOURCE,FSTYPE --target "$DISK")"
    echo "# zerodb ${H2H_ZERODB_REV:-$(git rev-parse --short HEAD)}, memtier $("$MEMTIER" --version | head -1)"
    for name in redis valkey dicedb; do echo "# $("$TOOLS/$name/src/$name-server" --version)"; done
    echo "# sqlite $(python3 -c 'import sqlite3; print(sqlite3.sqlite_version)')"
    printf 'system\twriters\trep\tturns_per_sec\tp50_us\tp99_us\tp999_us\tmax_us\n'
} >"$OUT"
for rep in $(seq "$REPS"); do
    for writers in $WRITERS; do
        for system in ${H2H_SYSTEMS:-zerodb redis valkey dicedb sqlite}; do
            case $system in
                zerodb) row=$(zerodb "$writers") ;;
                sqlite) row=$(sqlite "$writers") ;;
                *) row=$(resp "$writers" "$system") ;;
            esac
            printf '%s\t%s\t%s\t%s\n' "$system" "$writers" "$rep" "${row// /$'\t'}" | tee -a "$OUT"
        done
    done
done

# Medians across reps, as a Markdown table.
python3 - "$OUT" <<'PY'
import statistics, sys, collections
rows = [l.split("\t") for l in open(sys.argv[1]) if l[0] != "#" and not l.startswith("system")]
runs = collections.defaultdict(list)
for system, writers, _, *values in rows:
    runs[(int(writers), system)].append([float(v) for v in values])
print("| Writers | System | Turns/s | p50 µs | p99 µs | p99.9 µs | max µs |")
print("|---|---|---|---|---|---|---|")
for (writers, system), values in sorted(runs.items()):
    tps, p50, p99, p999, top = (statistics.median(column) for column in zip(*values))
    print(f"| {writers} | {system} | {tps:,.0f} | {p50:,.0f} | {p99:,.0f} | {p999:,.0f} | {top:,.0f} |")
PY
