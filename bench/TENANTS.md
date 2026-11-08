# Tenants per VM

This bench prices a hosted offer. It compares 2 layouts on 1 machine:

- **per-process**: 1 `atticdb serve` and 1 data dir per tenant.
- **shared**: 1 `atticdb serve` with N tenants in 1 config and 1 data dir.

Design 0009 section 0c explains why. The verdict comes from the Azure run in Task 4. This file holds the runbook and a local shape check.

## Runbook

You need Python 3 (stdlib only) and a release build.

```sh
cargo build --release -p atticdb
export ATTICDB_BIN=$PWD/target/release/atticdb
# Run dirs go here. Default is bench/.tb. Keep it short, see "Socket paths".
# Required on a VM for N=1,000: point it at the data disk, not the OS disk.
export TENANTS_BASE=/mnt/nvme/tb
# Linux at N=1,000: raise the fd and thread limits first.
ulimit -n 65536
ulimit -u 65536   # counts threads. Keep it well above N * 10.
```

1 run, 1 TSV row:

```sh
python3 bench/tenants.py --layout shared --tenants 10 --active-pct 20 \
    --duration 20 --think-ms 50 --out bench/tenants-smoke.tsv
```

The sweep used for the local shape check:

```sh
for layout in shared per-process; do for n in 1 10 100; do for pct in 1 5 20; do
  python3 bench/tenants.py --layout $layout --tenants $n --active-pct $pct \
      --duration 60 --think-ms 50 --out bench/tenants-local-mac.tsv
done; done; done
```

The script appends 1 row per run. It writes the header when the file is new. It refuses a file whose header has other columns. A failed run writes no row and exits non-zero.

Before it starts any server, it checks free space under `TENANTS_BASE`. per-process needs about N x 35 MiB. shared needs 256 MiB. A short disk fails the run with a message that names `TENANTS_BASE`.

## What 1 run does

1. Each tenant gets a random 32-hex token. Each config is written mode 0600 with `segment_mib: 16`.
2. per-process starts N `atticdb serve --listen none --unix-socket <dir>/s`. shared starts 1.
3. It waits until every server answers `GET /v1/facts/get?key=x` with 200.
4. It waits 5 s, then takes the idle sample.
5. `round(N * active_pct / 100)` tenants go active, at least 1. The pick is seeded, so it repeats.
6. Each active tenant gets 1 client process with 1 request in flight.
7. Before each op the client sleeps a seeded uniform 0 to 2x `--think-ms`.
8. Ops alternate `POST /v1/append` and `POST /v1/facts/put`. Each carries 256 bytes and an idem key. Each sample is tagged with its op.
9. Fact puts pick 1 of 16 keys.
10. 2 s before the end of the load it takes the load sample. Clients are still connected then.
11. per-process only: it stops tenant 0's `serve`, spawns it again and times the first 200. It does this 5 times and keeps the median.
12. Teardown stops every `serve` it started, by pid. Then it removes the run dir.

SIGTERM and Ctrl-C still run the teardown. The first signal makes the script ignore later ones, so a second signal can't cut the teardown short. A SIGKILL can leave servers and `bench/.tb` behind.

## Columns

| Column | Meaning |
|---|---|
| `layout` | `per-process` or `shared` |
| `tenants` | N |
| `active_pct` | share of tenants with a client |
| `ops` | ops completed by all clients |
| `p50_ms`, `p99_ms` | nearest-rank percentile over every op of every active tenant |
| `p50_append_ms`, `p99_append_ms` | the same, over `POST /v1/append` ops only |
| `p50_fact_ms`, `p99_fact_ms` | the same, over `POST /v1/facts/put` ops only. `-` if there were none |
| `rss_mib_idle` | RSS summed over every `serve` pid, after 5 s idle |
| `rss_mib_load` | RSS summed over every `serve` pid, 2 s before the load ends |
| `fds` | open fds summed over every `serve` pid, in the load sample |
| `threads` | threads summed over every `serve` pid, in the load sample |
| `disk_mib_per_tenant` | `du -sk` of all data dirs, divided by N |
| `cold_start_ms` | per-process only: median of 5 spawns to first 200 on an existing data dir. `-` for shared |

Latency is the client round trip on the same host. The timer starts before the send and stops after the full reply. Think time is outside it. Design 0009 sets its bar on server-side p99. The round trip is an upper bound on that.

RSS comes from `ps -o rss=` on both systems. fds and threads differ:

- **Linux**: `/proc/<pid>/fd` entries and the `Threads:` line of `/proc/<pid>/status`.
- **macOS**: `lsof -nP -F f -p <pids>`, counting only numbered fds. cwd, txt and mem rows are skipped. Threads come from `ps -M -p <pids>`, 1 line per thread after the header.

Every sample checks that each `serve` is still alive. A dead server fails the run. If `du` can't size a data dir, the script warns on stderr. It also warns if it can't raise the fd limit.

## Socket paths

macOS limits a Unix socket path to 104 bytes. Linux allows 108. The socket is `$TENANTS_BASE/<8 hex>/t<i>/s`. The script refuses a path over 103 bytes. On the Mac the default base gave about 60 bytes at N=100.

## Limits

The script raises its soft `RLIMIT_NOFILE` to min(hard, 65536) when it's lower. Children inherit it. On the Mac these limits applied, and none was hit:

| Limit | Value |
|---|---|
| `ulimit -n` (soft) | 1048575 |
| `kern.maxfilesperproc` | 61440 |
| `kern.maxprocperuid` | 2666 |
| `kern.num_taskthreads` | 4096 |

For N=1,000 on Linux, check `ulimit -u` first. It counts threads, not processes. per-process at N=1,000 needs about 7,000 `serve` threads plus clients. Also check free disk. per-process uses about 34 MiB per tenant, so 34 GiB at N=1,000. Set `TENANTS_BASE` to the data disk. The default is under the repo, which is usually on the OS disk.

## Local results

**Local shape check, not the verdict.** The Mac is not the target VM. Latency includes macOS fsync on an Apple SSD.

Machine: MacBook Air (Mac16,12), Apple M4, 10 cores (4 performance, 6 efficiency), 16 GB RAM. macOS 27.2 (build 26B123). Internal APFS SSD, 494 GB. Python 3.14.6. `atticdb 0.1.0+dev` release build at commit 243140f.

Command: the sweep above with `ATTICDB_BIN=target/release/atticdb` and the default base. 18 runs of 60 s took 1,191 s in total. The append and fact columns are in ms. Raw rows: `bench/tenants-local-mac.tsv`.

| layout | N | active % | ops | p50 ms | p99 ms | p50 append | p99 append | p50 fact | p99 fact | RSS idle MiB | RSS load MiB | fds | threads | disk MiB/tenant | cold start ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| shared | 1 | 1 | 1024 | 4.609 | 6.261 | 4.630 | 6.348 | 4.573 | 5.949 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 1 | 5 | 1023 | 4.608 | 6.065 | 4.632 | 6.065 | 4.580 | 6.014 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 1 | 20 | 1025 | 4.557 | 6.511 | 4.605 | 6.257 | 4.523 | 6.511 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 10 | 1 | 1029 | 4.601 | 6.263 | 4.611 | 6.237 | 4.589 | 6.263 | 3.6 | 5.5 | 11 | 8 | 3.41 | - |
| shared | 10 | 5 | 997 | 4.600 | 6.063 | 4.612 | 6.271 | 4.585 | 6.063 | 3.6 | 5.4 | 11 | 8 | 3.41 | - |
| shared | 10 | 20 | 1975 | 4.499 | 8.224 | 4.517 | 8.568 | 4.486 | 7.704 | 3.6 | 6.3 | 13 | 9 | 3.42 | - |
| shared | 100 | 1 | 998 | 4.591 | 6.370 | 4.606 | 6.130 | 4.585 | 6.454 | 3.7 | 5.5 | 11 | 8 | 0.34 | - |
| shared | 100 | 5 | 4959 | 4.488 | 8.590 | 4.546 | 8.758 | 4.440 | 8.207 | 3.7 | 8.8 | 19 | 12 | 0.34 | - |
| shared | 100 | 20 | 19478 | 5.404 | 8.561 | 5.571 | 8.763 | 5.225 | 8.337 | 3.7 | 17.9 | 49 | 27 | 0.36 | - |
| per-process | 1 | 1 | 982 | 4.597 | 6.681 | 4.607 | 6.420 | 4.586 | 6.854 | 3.6 | 5.3 | 11 | 8 | 34.11 | 41.5 |
| per-process | 1 | 5 | 978 | 4.591 | 6.732 | 4.625 | 6.762 | 4.564 | 6.732 | 3.6 | 5.4 | 11 | 8 | 34.11 | 43.3 |
| per-process | 1 | 20 | 979 | 4.625 | 6.341 | 4.643 | 6.341 | 4.620 | 6.343 | 3.6 | 5.3 | 11 | 8 | 34.11 | 43.1 |
| per-process | 10 | 1 | 987 | 4.564 | 6.796 | 4.584 | 7.267 | 4.536 | 5.998 | 37.0 | 38.8 | 83 | 71 | 34.02 | 42.4 |
| per-process | 10 | 5 | 985 | 4.577 | 6.882 | 4.612 | 7.416 | 4.564 | 6.082 | 37.1 | 38.9 | 83 | 71 | 34.02 | 40.2 |
| per-process | 10 | 20 | 1976 | 4.451 | 8.875 | 4.437 | 9.035 | 4.480 | 8.849 | 36.1 | 39.5 | 86 | 72 | 34.03 | 41.0 |
| per-process | 100 | 1 | 1047 | 4.528 | 6.645 | 4.547 | 6.782 | 4.500 | 6.571 | 285.7 | 290.5 | 803 | 701 | 33.53 | 38.1 |
| per-process | 100 | 5 | 5159 | 4.494 | 11.119 | 4.503 | 11.474 | 4.484 | 10.819 | 366.9 | 375.9 | 815 | 705 | 32.58 | 37.1 |
| per-process | 100 | 20 | 19053 | 9.760 | 18.223 | 9.763 | 18.223 | 9.756 | 18.188 | 366.4 | 401.5 | 860 | 720 | 33.87 | 40.8 |

What the shape shows:

- Every cell held p99 under 20 ms, for both ops. The 50 ms bar from design 0009 was never close.
- append and fact p99 stayed within 1.3 ms of each other in every cell.
- An idle `serve` costs about 3 to 4 MiB RSS, 8 fds and 7 threads.
- per-process scales that cost by N. At N=100 it held 286 to 367 MiB idle against 3.7 MiB shared.
- shared adds about 2 fds and 1 thread per open connection. That matches 1 thread per connection.
- per-process disk is 34 MiB per tenant. shared spreads 1 tape, so it's 0.34 MiB at N=100.
- At N=100 and 20% active, per-process p50 doubled to 9.8 ms. shared stayed at 5.4 ms.
- Cold start on an existing dir took a median of 37 to 43 ms.

The p50 jump is a hypothesis, not a finding. 20 servers fsync on their own, while 1 shared server may batch them. The Linux run should show whether it holds on NVMe.

These rows come from the third sweep. The first sampled fds and threads after the clients closed, so it missed connection threads. The second had no per-op columns or repeated cold starts. Both were discarded. The first also showed shared N=1 p99 spikes of 130 to 320 ms. Neither later sweep repeated them.
